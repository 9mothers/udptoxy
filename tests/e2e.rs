use parking_lot::RwLock;
use std::{collections::VecDeque, net::SocketAddr, sync::Arc, time::Duration};
use tokio::{
    net::UdpSocket,
    sync::{oneshot, watch},
    time::timeout,
};

use udptoxy::{
    proxy::run_proxy,
    types::{EVENT_BUFFER, EventBuffer, ProxySettings},
};

fn base_settings() -> ProxySettings {
    ProxySettings {
        latency_ms: 0,
        jitter_ms: 0,
        loss_percent: 0.0,
        rate_bytes_per_sec: 0,
        latency_enabled: false,
        jitter_enabled: false,
        loss_enabled: false,
        rate_enabled: false,
    }
}

async fn start_proxy(
    out_addr: SocketAddr,
    settings: ProxySettings,
) -> (
    SocketAddr,
    watch::Sender<bool>,
    tokio::task::JoinHandle<Result<(), Box<dyn std::error::Error + Send + Sync>>>,
    EventBuffer,
) {
    let settings = Arc::new(RwLock::new(settings));
    let events: EventBuffer = Arc::new(parking_lot::Mutex::new(VecDeque::with_capacity(
        EVENT_BUFFER,
    )));
    let (shutdown_tx, shutdown_rx) = watch::channel(false);
    let (ready_tx, ready_rx) = oneshot::channel::<SocketAddr>();

    let handle = tokio::spawn(run_proxy(
        SocketAddr::from(([127, 0, 0, 1], 0)),
        out_addr,
        Arc::clone(&settings),
        Arc::clone(&events),
        shutdown_rx,
        Some(ready_tx),
    ));

    let in_addr = ready_rx.await.unwrap();
    (in_addr, shutdown_tx, handle, events)
}

async fn bind_udp(addr: &str) -> Option<UdpSocket> {
    match UdpSocket::bind(addr).await {
        Ok(sock) => Some(sock),
        Err(err) if err.kind() == std::io::ErrorKind::PermissionDenied => {
            eprintln!("skipping e2e test: UDP bind not permitted");
            None
        }
        Err(err) => panic!("bind failed: {err}"),
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn forwards_packet() {
    let Some(out_sock) = bind_udp("127.0.0.1:0").await else {
        return;
    };
    let out_addr = out_sock.local_addr().unwrap();

    let (in_addr, shutdown_tx, proxy_handle, _events) =
        start_proxy(out_addr, base_settings()).await;
    let Some(client) = bind_udp("127.0.0.1:0").await else {
        return;
    };
    let payload = b"hello";
    client.send_to(payload, in_addr).await.unwrap();

    let mut buf = [0u8; 64];
    let (len, _) = timeout(Duration::from_millis(200), out_sock.recv_from(&mut buf))
        .await
        .unwrap()
        .unwrap();

    assert_eq!(&buf[..len], payload);

    let _ = shutdown_tx.send(true);
    let _ = proxy_handle.await.unwrap();
}

#[tokio::test(flavor = "multi_thread")]
async fn drops_when_loss_100() {
    let Some(out_sock) = bind_udp("127.0.0.1:0").await else {
        return;
    };
    let out_addr = out_sock.local_addr().unwrap();

    let mut settings = base_settings();
    settings.loss_percent = 100.0;
    settings.loss_enabled = true;
    let (in_addr, shutdown_tx, proxy_handle, _events) = start_proxy(out_addr, settings).await;
    let Some(client) = bind_udp("127.0.0.1:0").await else {
        return;
    };
    let payload = b"drop";
    client.send_to(payload, in_addr).await.unwrap();

    let mut buf = [0u8; 64];
    let recv = timeout(Duration::from_millis(200), out_sock.recv_from(&mut buf)).await;
    assert!(recv.is_err(), "packet unexpectedly received");

    let _ = shutdown_tx.send(true);
    let _ = proxy_handle.await.unwrap();
}

#[tokio::test(flavor = "multi_thread")]
async fn preserves_order_with_latency() {
    let Some(out_sock) = bind_udp("127.0.0.1:0").await else {
        return;
    };
    let out_addr = out_sock.local_addr().unwrap();

    let mut settings = base_settings();
    settings.latency_ms = 10;
    settings.latency_enabled = true;
    let (in_addr, shutdown_tx, proxy_handle, _events) = start_proxy(out_addr, settings).await;

    let Some(client) = bind_udp("127.0.0.1:0").await else {
        return;
    };

    let count = 20u32;
    for seq in 0..count {
        client.send_to(&seq.to_be_bytes(), in_addr).await.unwrap();
    }

    let mut received = Vec::new();
    for _ in 0..count {
        let mut buf = [0u8; 8];
        let (len, _) = timeout(Duration::from_millis(500), out_sock.recv_from(&mut buf))
            .await
            .unwrap()
            .unwrap();
        let seq = u32::from_be_bytes(buf[..len].try_into().unwrap());
        received.push(seq);
    }

    let expected: Vec<u32> = (0..count).collect();
    assert_eq!(received, expected);

    let _ = shutdown_tx.send(true);
    let _ = proxy_handle.await.unwrap();
}

#[tokio::test(flavor = "multi_thread")]
async fn jitter_can_reorder() {
    let Some(out_sock) = bind_udp("127.0.0.1:0").await else {
        return;
    };
    let out_addr = out_sock.local_addr().unwrap();

    let mut settings = base_settings();
    settings.jitter_ms = 50;
    settings.jitter_enabled = true;
    let (in_addr, shutdown_tx, proxy_handle, _events) = start_proxy(out_addr, settings).await;

    let Some(client) = bind_udp("127.0.0.1:0").await else {
        return;
    };

    let count = 100u32;
    for seq in 0..count {
        client.send_to(&seq.to_be_bytes(), in_addr).await.unwrap();
    }

    let deadline = tokio::time::Instant::now() + Duration::from_millis(2000);
    let mut received = Vec::new();
    while received.len() < count as usize {
        let now = tokio::time::Instant::now();
        if now >= deadline {
            break;
        }
        let remaining = deadline - now;
        let mut buf = [0u8; 8];
        if let Ok(Ok((len, _))) = timeout(remaining, out_sock.recv_from(&mut buf)).await {
            let seq = u32::from_be_bytes(buf[..len].try_into().unwrap());
            received.push(seq);
        }
    }

    assert_eq!(received.len(), count as usize);
    let mut out_of_order = false;
    for i in 1..received.len() {
        if received[i] < received[i - 1] {
            out_of_order = true;
            break;
        }
    }
    assert!(
        out_of_order,
        "expected jitter to reorder at least one packet"
    );

    let _ = shutdown_tx.send(true);
    let _ = proxy_handle.await.unwrap();
}
