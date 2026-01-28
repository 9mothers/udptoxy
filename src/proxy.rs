use crate::types::{
    CLIENT_QUEUE, EVENT_BUFFER, EventBuffer, PacketAction, PacketDirection, PacketEvent,
    ProxySettings,
};
use parking_lot::{Mutex, RwLock};
use rand::Rng;
use std::{
    cmp::Ordering as CmpOrdering,
    collections::{BinaryHeap, HashMap},
    net::{Ipv4Addr, SocketAddr},
    sync::{
        Arc,
        atomic::{AtomicU64, Ordering},
    },
    time::{Duration, Instant},
};
use tokio::{
    net::UdpSocket,
    sync::{mpsc, oneshot, watch},
    time::sleep,
};

pub async fn run_proxy(
    in_addr: SocketAddr,
    out_addr: SocketAddr,
    settings: Arc<RwLock<ProxySettings>>,
    events: EventBuffer,
    shutdown_rx: watch::Receiver<bool>,
    ready_tx: Option<oneshot::Sender<SocketAddr>>,
) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    let rate_limiter_in = Arc::new(Mutex::new(RateLimiter::new()));
    let rate_limiter_out = Arc::new(Mutex::new(RateLimiter::new()));

    proxy_loop(
        in_addr,
        out_addr,
        settings,
        events,
        rate_limiter_in,
        rate_limiter_out,
        shutdown_rx,
        ready_tx,
    )
    .await
}

struct RateLimiter {
    tokens: u64,
    last: Instant,
    last_rate: u64,
}

impl RateLimiter {
    fn new() -> Self {
        Self {
            tokens: 0,
            last: Instant::now(),
            last_rate: 0,
        }
    }

    fn allow(&mut self, bytes: usize, rate_per_sec: u64) -> bool {
        let now = Instant::now();
        if rate_per_sec == 0 {
            self.tokens = 0;
            self.last_rate = 0;
            self.last = now;
            return true;
        }

        if self.last_rate != rate_per_sec {
            self.last_rate = rate_per_sec;
            self.tokens = rate_per_sec;
            self.last = now;
        } else {
            let elapsed = now.duration_since(self.last);
            let added = (elapsed.as_secs_f64() * rate_per_sec as f64) as u64;
            if added > 0 {
                self.tokens = (self.tokens + added).min(rate_per_sec);
                self.last = now;
            }
        }

        let needed = bytes as u64;
        if needed <= self.tokens {
            self.tokens -= needed;
            true
        } else {
            false
        }
    }
}

struct PacketJob {
    id: u64,
    data: Vec<u8>,
    src: SocketAddr,
    dest_addr: SocketAddr,
    time_in_ms: u128,
    arrival: Instant,
    settings: ProxySettings,
    dest_socket: Arc<UdpSocket>,
    events: EventBuffer,
    rate_limiter: Arc<Mutex<RateLimiter>>,
    direction: PacketDirection,
    start: Instant,
}

struct ScheduledPacket {
    send_at: Instant,
    seq: u64,
    job: PacketJob,
}

impl PartialEq for ScheduledPacket {
    fn eq(&self, other: &Self) -> bool {
        self.send_at == other.send_at && self.seq == other.seq
    }
}

impl Eq for ScheduledPacket {}

impl PartialOrd for ScheduledPacket {
    fn partial_cmp(&self, other: &Self) -> Option<CmpOrdering> {
        Some(self.cmp(other))
    }
}

impl Ord for ScheduledPacket {
    fn cmp(&self, other: &Self) -> CmpOrdering {
        self.send_at
            .cmp(&other.send_at)
            .then_with(|| self.seq.cmp(&other.seq))
    }
}

struct ClientPacket {
    id: u64,
    data: Vec<u8>,
    src: SocketAddr,
    time_in_ms: u128,
    arrival: Instant,
    settings: ProxySettings,
}

struct ClientState {
    tx: mpsc::Sender<ClientPacket>,
}

struct InboundJob {
    upstream: Arc<UdpSocket>,
    out_addr: SocketAddr,
    events: EventBuffer,
    rate_limiter: Arc<Mutex<RateLimiter>>,
    start: Instant,
    rx: mpsc::Receiver<ClientPacket>,
    shutdown_rx: watch::Receiver<bool>,
}

struct ResponseJob {
    client_addr: SocketAddr,
    upstream: Arc<UdpSocket>,
    socket_in: Arc<UdpSocket>,
    settings: Arc<RwLock<ProxySettings>>,
    events: EventBuffer,
    rate_limiter: Arc<Mutex<RateLimiter>>,
    counter: Arc<AtomicU64>,
    start: Instant,
    shutdown_rx: watch::Receiver<bool>,
}

#[allow(clippy::map_entry, clippy::too_many_arguments)]
async fn proxy_loop(
    in_addr: SocketAddr,
    out_addr: SocketAddr,
    settings: Arc<RwLock<ProxySettings>>,
    events: EventBuffer,
    rate_limiter_in: Arc<Mutex<RateLimiter>>,
    rate_limiter_out: Arc<Mutex<RateLimiter>>,
    mut shutdown_rx: watch::Receiver<bool>,
    ready_tx: Option<oneshot::Sender<SocketAddr>>,
) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    let socket_in = Arc::new(UdpSocket::bind(in_addr).await?);
    if let Some(tx) = ready_tx {
        let _ = tx.send(socket_in.local_addr()?);
    }
    let start = Instant::now();
    let counter = Arc::new(AtomicU64::new(1));
    let mut clients: HashMap<SocketAddr, ClientState> = HashMap::new();

    let mut buf = vec![0u8; 65_535];

    loop {
        tokio::select! {
            res = shutdown_rx.changed() => {
                if res.is_err() || *shutdown_rx.borrow() {
                    break;
                }
            }
            recv = socket_in.recv_from(&mut buf) => {
                let (len, src) = match recv {
                    Ok(value) => value,
                    Err(err) => {
                        eprintln!("recv error: {err}");
                        continue;
                    }
                };
                let id = counter.fetch_add(1, Ordering::Relaxed);
                let arrival = Instant::now();
                let time_in_ms = arrival.duration_since(start).as_millis();
                let data = buf[..len].to_vec();
                let settings_snapshot = settings.read().clone();

                if !clients.contains_key(&src) {
                    let socket = match UdpSocket::bind((Ipv4Addr::UNSPECIFIED, 0)).await {
                        Ok(sock) => sock,
                        Err(err) => {
                            eprintln!("bind upstream error for {src}: {err}");
                            continue;
                        }
                    };
                    let upstream = Arc::new(socket);
                    let (tx, rx) = mpsc::channel(CLIENT_QUEUE);

                    let inbound_job = InboundJob {
                        upstream: Arc::clone(&upstream),
                        out_addr,
                        events: Arc::clone(&events),
                        rate_limiter: Arc::clone(&rate_limiter_in),
                        start,
                        rx,
                        shutdown_rx: shutdown_rx.clone(),
                    };
                    tokio::spawn(async move {
                        inbound_loop(inbound_job).await;
                    });

                    let response_job = ResponseJob {
                        client_addr: src,
                        upstream: Arc::clone(&upstream),
                        socket_in: Arc::clone(&socket_in),
                        settings: Arc::clone(&settings),
                        events: Arc::clone(&events),
                        rate_limiter: Arc::clone(&rate_limiter_out),
                        counter: Arc::clone(&counter),
                        start,
                        shutdown_rx: shutdown_rx.clone(),
                    };
                    tokio::spawn(async move {
                        response_loop(response_job).await;
                    });

                    clients.insert(src, ClientState { tx });
                }

                let Some(state) = clients.get(&src) else {
                    continue;
                };
                let packet = ClientPacket {
                    id,
                    data,
                    src,
                    time_in_ms,
                    arrival,
                    settings: settings_snapshot,
                };
                if state.tx.try_send(packet).is_err() {
                    let event = PacketEvent {
                        id,
                        direction: PacketDirection::InToOut,
                        time_in_ms,
                        latency_ms: 0,
                        time_out_ms: None,
                        action: PacketAction::Dropped,
                        size: len,
                        src,
                    };
                    push_event(&events, event);
                }
            }
        }
    }

    Ok(())
}

async fn inbound_loop(job: InboundJob) {
    let InboundJob {
        upstream,
        out_addr,
        events,
        rate_limiter,
        start,
        mut rx,
        mut shutdown_rx,
    } = job;
    let mut last_deadline = Instant::now();
    let mut heap: BinaryHeap<std::cmp::Reverse<ScheduledPacket>> = BinaryHeap::new();

    loop {
        if let Some(next) = heap.peek() {
            let now = Instant::now();
            let sleep_dur = next.0.send_at.saturating_duration_since(now);
            tokio::select! {
                res = shutdown_rx.changed() => {
                    if res.is_err() || *shutdown_rx.borrow() {
                        break;
                    }
                }
                packet = rx.recv() => {
                    let Some(packet) = packet else {
                        break;
                    };
                    if let Some(scheduled) = schedule_packet(
                        PacketJob {
                            id: packet.id,
                            data: packet.data,
                            src: packet.src,
                            dest_addr: out_addr,
                            time_in_ms: packet.time_in_ms,
                            arrival: packet.arrival,
                            settings: packet.settings,
                            dest_socket: Arc::clone(&upstream),
                            events: Arc::clone(&events),
                            rate_limiter: Arc::clone(&rate_limiter),
                            direction: PacketDirection::InToOut,
                            start,
                        },
                        &mut last_deadline,
                    ) {
                        heap.push(std::cmp::Reverse(scheduled));
                    }
                }
                _ = sleep(sleep_dur) => {
                    send_due(&mut heap).await;
                }
            }
        } else {
            tokio::select! {
                res = shutdown_rx.changed() => {
                    if res.is_err() || *shutdown_rx.borrow() {
                        break;
                    }
                }
                packet = rx.recv() => {
                    let Some(packet) = packet else {
                        break;
                    };
                    if let Some(scheduled) = schedule_packet(
                        PacketJob {
                            id: packet.id,
                            data: packet.data,
                            src: packet.src,
                            dest_addr: out_addr,
                            time_in_ms: packet.time_in_ms,
                            arrival: packet.arrival,
                            settings: packet.settings,
                            dest_socket: Arc::clone(&upstream),
                            events: Arc::clone(&events),
                            rate_limiter: Arc::clone(&rate_limiter),
                            direction: PacketDirection::InToOut,
                            start,
                        },
                        &mut last_deadline,
                    ) {
                        heap.push(std::cmp::Reverse(scheduled));
                    }
                }
            }
        }
    }
}

async fn response_loop(job: ResponseJob) {
    let ResponseJob {
        client_addr,
        upstream,
        socket_in,
        settings,
        events,
        rate_limiter,
        counter,
        start,
        mut shutdown_rx,
    } = job;
    let mut buf = vec![0u8; 65_535];
    let mut last_deadline = Instant::now();
    let mut heap: BinaryHeap<std::cmp::Reverse<ScheduledPacket>> = BinaryHeap::new();

    loop {
        if let Some(next) = heap.peek() {
            let now = Instant::now();
            let sleep_dur = next.0.send_at.saturating_duration_since(now);
            tokio::select! {
                res = shutdown_rx.changed() => {
                    if res.is_err() || *shutdown_rx.borrow() {
                        break;
                    }
                }
                recv = upstream.recv_from(&mut buf) => {
                    let (len, src) = match recv {
                        Ok(value) => value,
                        Err(err) => {
                            eprintln!("upstream recv error for {client_addr}: {err}");
                            break;
                        }
                    };
                    let arrival = Instant::now();
                    let time_in_ms = arrival.duration_since(start).as_millis();
                    let data = buf[..len].to_vec();
                    let settings_snapshot = settings.read().clone();
                    let events = Arc::clone(&events);
                    let rate_limiter = Arc::clone(&rate_limiter);
                    let dest_socket = Arc::clone(&socket_in);
                    let id = counter.fetch_add(1, Ordering::Relaxed);

                    if let Some(scheduled) = schedule_packet(
                        PacketJob {
                            id,
                            data,
                            src,
                            dest_addr: client_addr,
                            time_in_ms,
                            arrival,
                            settings: settings_snapshot,
                            dest_socket,
                            events,
                            rate_limiter,
                            direction: PacketDirection::OutToIn,
                            start,
                        },
                        &mut last_deadline,
                    ) {
                        heap.push(std::cmp::Reverse(scheduled));
                    }
                }
                _ = sleep(sleep_dur) => {
                    send_due(&mut heap).await;
                }
            }
        } else {
            tokio::select! {
                res = shutdown_rx.changed() => {
                    if res.is_err() || *shutdown_rx.borrow() {
                        break;
                    }
                }
                recv = upstream.recv_from(&mut buf) => {
                    let (len, src) = match recv {
                        Ok(value) => value,
                        Err(err) => {
                            eprintln!("upstream recv error for {client_addr}: {err}");
                            break;
                        }
                    };
                    let arrival = Instant::now();
                    let time_in_ms = arrival.duration_since(start).as_millis();
                    let data = buf[..len].to_vec();
                    let settings_snapshot = settings.read().clone();
                    let events = Arc::clone(&events);
                    let rate_limiter = Arc::clone(&rate_limiter);
                    let dest_socket = Arc::clone(&socket_in);
                    let id = counter.fetch_add(1, Ordering::Relaxed);

                    if let Some(scheduled) = schedule_packet(
                        PacketJob {
                            id,
                            data,
                            src,
                            dest_addr: client_addr,
                            time_in_ms,
                            arrival,
                            settings: settings_snapshot,
                            dest_socket,
                            events,
                            rate_limiter,
                            direction: PacketDirection::OutToIn,
                            start,
                        },
                        &mut last_deadline,
                    ) {
                        heap.push(std::cmp::Reverse(scheduled));
                    }
                }
            }
        }
    }
}

fn schedule_packet(job: PacketJob, last_deadline: &mut Instant) -> Option<ScheduledPacket> {
    let PacketJob {
        id,
        data,
        src,
        dest_addr,
        time_in_ms,
        arrival,
        settings,
        dest_socket,
        events,
        rate_limiter,
        direction,
        start,
    } = job;

    if settings.rate_enabled {
        let allowed = {
            let mut limiter = rate_limiter.lock();
            limiter.allow(data.len(), settings.rate_bytes_per_sec)
        };
        if !allowed {
            let event = PacketEvent {
                id,
                direction,
                time_in_ms,
                latency_ms: 0,
                time_out_ms: None,
                action: PacketAction::Dropped,
                size: data.len(),
                src,
            };
            push_event(&events, event);
            return None;
        }
    }

    let (drop, delay_ms) = {
        let mut rng = rand::rng();
        let drop = if settings.loss_enabled {
            rng.random_bool(settings.loss_percent / 100.0)
        } else {
            false
        };

        let base_latency = if settings.latency_enabled {
            settings.latency_ms as i64
        } else {
            0
        };
        let jitter_bound = if settings.jitter_enabled {
            settings.jitter_ms as i64
        } else {
            0
        };
        let jitter = if jitter_bound > 0 {
            rng.random_range(-jitter_bound..=jitter_bound)
        } else {
            0
        };
        let delay_ms = (base_latency + jitter).max(0) as u64;
        (drop, delay_ms)
    };

    if drop {
        let event = PacketEvent {
            id,
            direction,
            time_in_ms,
            latency_ms: 0,
            time_out_ms: None,
            action: PacketAction::Dropped,
            size: data.len(),
            src,
        };
        push_event(&events, event);
        return None;
    }

    let desired = arrival + Duration::from_millis(delay_ms);
    let allow_reorder = settings.jitter_enabled && settings.jitter_ms > 0;
    let send_at = if allow_reorder {
        desired
    } else {
        desired.max(*last_deadline)
    };
    *last_deadline = (*last_deadline).max(send_at);

    let job = PacketJob {
        id,
        data,
        src,
        dest_addr,
        time_in_ms,
        arrival,
        settings,
        dest_socket,
        events,
        rate_limiter,
        direction,
        start,
    };

    Some(ScheduledPacket {
        send_at,
        seq: id,
        job,
    })
}

async fn send_due(heap: &mut BinaryHeap<std::cmp::Reverse<ScheduledPacket>>) {
    let now = Instant::now();
    while let Some(next) = heap.peek() {
        if next.0.send_at > now {
            break;
        }
        let scheduled = heap.pop().expect("heap empty").0;
        send_packet(scheduled.job).await;
    }
}

async fn send_packet(job: PacketJob) {
    let PacketJob {
        id,
        data,
        src,
        dest_addr,
        time_in_ms,
        arrival,
        dest_socket,
        events,
        direction,
        start,
        ..
    } = job;

    let applied_delay_ms = Instant::now()
        .saturating_duration_since(arrival)
        .as_millis() as u64;

    let (action, time_out_ms) = match dest_socket.send_to(&data, dest_addr).await {
        Ok(_) => (
            PacketAction::Forwarded,
            Some(Instant::now().duration_since(start).as_millis()),
        ),
        Err(_) => (PacketAction::Error, None),
    };

    let event = PacketEvent {
        id,
        direction,
        time_in_ms,
        latency_ms: applied_delay_ms,
        time_out_ms,
        action,
        size: data.len(),
        src,
    };

    push_event(&events, event);
}

fn push_event(events: &EventBuffer, event: PacketEvent) {
    let mut guard = events.lock();
    if guard.len() >= EVENT_BUFFER {
        guard.pop_back();
    }
    guard.push_front(event);
}
