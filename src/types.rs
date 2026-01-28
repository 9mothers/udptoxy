use clap::Parser;
use parking_lot::Mutex;
use std::{
    collections::VecDeque,
    net::{IpAddr, SocketAddr},
    sync::Arc,
};

pub const EVENT_BUFFER: usize = 2048;
pub const CLIENT_QUEUE: usize = 1024;
pub const RATE_PRESETS: &[u64] = &[
    10_000,
    25_000,
    50_000,
    100_000,
    250_000,
    300_000,
    350_000,
    400_000,
    450_000,
    500_000,
    1_000_000,
    2_000_000,
    5_000_000,
    10_000_000,
    20_000_000,
    50_000_000,
    100_000_000,
];

#[derive(Parser, Debug, Clone)]
#[command(name = "udptoxy", version, about = "UDP toxiproxy-style CLI")]
pub struct Args {
    #[arg(short = 'i', long = "in", value_name = "ADDR_OR_PORT")]
    pub in_addr: String,

    #[arg(short = 'o', long = "out", value_name = "ADDR_OR_PORT")]
    pub out_addr: String,

    #[arg(long, default_value_t = 0)]
    pub latency_ms: u64,

    #[arg(long, default_value_t = 0)]
    pub jitter_ms: u64,

    #[arg(long, default_value_t = 0.0)]
    pub loss_percent: f64,

    #[arg(short = 'r', long = "rate-bytes-per-sec", default_value_t = 0)]
    pub rate_bytes_per_sec: u64,
}

#[derive(Debug, Clone)]
pub struct ProxySettings {
    pub latency_ms: u64,
    pub jitter_ms: u64,
    pub loss_percent: f64,
    pub rate_bytes_per_sec: u64,
    pub latency_enabled: bool,
    pub jitter_enabled: bool,
    pub loss_enabled: bool,
    pub rate_enabled: bool,
}

impl ProxySettings {
    pub fn from_args(args: &Args) -> Self {
        let loss_percent = args.loss_percent.clamp(0.0, 100.0);
        let rate_bytes_per_sec = args.rate_bytes_per_sec;
        Self {
            latency_ms: args.latency_ms,
            jitter_ms: args.jitter_ms,
            loss_percent,
            rate_bytes_per_sec,
            latency_enabled: args.latency_ms > 0,
            jitter_enabled: args.jitter_ms > 0,
            loss_enabled: loss_percent > 0.0,
            rate_enabled: rate_bytes_per_sec > 0,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SettingKind {
    Latency,
    Jitter,
    Loss,
    Rate,
}

impl SettingKind {
    pub fn next(self) -> Self {
        match self {
            SettingKind::Latency => SettingKind::Jitter,
            SettingKind::Jitter => SettingKind::Loss,
            SettingKind::Loss => SettingKind::Rate,
            SettingKind::Rate => SettingKind::Latency,
        }
    }

    pub fn prev(self) -> Self {
        match self {
            SettingKind::Latency => SettingKind::Rate,
            SettingKind::Jitter => SettingKind::Latency,
            SettingKind::Loss => SettingKind::Jitter,
            SettingKind::Rate => SettingKind::Loss,
        }
    }
}

#[derive(Clone, Copy, Debug)]
pub enum AdjustDirection {
    Increase,
    Decrease,
}

#[derive(Clone, Debug)]
pub enum PacketAction {
    Forwarded,
    Dropped,
    Error,
}

impl PacketAction {
    pub fn as_str(&self) -> &'static str {
        match self {
            PacketAction::Forwarded => "forward",
            PacketAction::Dropped => "drop",
            PacketAction::Error => "error",
        }
    }
}

#[derive(Clone, Copy, Debug)]
pub enum PacketDirection {
    InToOut,
    OutToIn,
}

impl PacketDirection {
    pub fn as_str(&self) -> &'static str {
        match self {
            PacketDirection::InToOut => "in->out",
            PacketDirection::OutToIn => "out->in",
        }
    }
}

#[derive(Clone, Debug)]
pub struct PacketEvent {
    pub id: u64,
    pub direction: PacketDirection,
    pub time_in_ms: u128,
    pub latency_ms: u64,
    pub time_out_ms: Option<u128>,
    pub action: PacketAction,
    pub size: usize,
    pub src: SocketAddr,
}

pub type EventBuffer = Arc<Mutex<VecDeque<PacketEvent>>>;

pub fn parse_addr(value: &str, default_host: IpAddr) -> Result<SocketAddr, String> {
    if let Ok(port) = value.parse::<u16>() {
        return Ok(SocketAddr::new(default_host, port));
    }
    value
        .parse::<SocketAddr>()
        .map_err(|e| format!("invalid address '{value}': {e}"))
}

pub fn next_rate(current: u64) -> u64 {
    for preset in RATE_PRESETS {
        if *preset > current {
            return *preset;
        }
    }
    *RATE_PRESETS.last().unwrap_or(&current)
}

pub fn prev_rate(current: u64) -> u64 {
    for preset in RATE_PRESETS.iter().rev() {
        if *preset < current {
            return *preset;
        }
    }
    0
}

pub fn format_rate(bytes_per_sec: u64) -> String {
    if bytes_per_sec >= 1_000_000_000 {
        format!("{:.1}GB/s", bytes_per_sec as f64 / 1_000_000_000.0)
    } else if bytes_per_sec >= 1_000_000 {
        format!("{:.1}MB/s", bytes_per_sec as f64 / 1_000_000.0)
    } else if bytes_per_sec >= 1_000 {
        format!("{:.0}KB/s", bytes_per_sec as f64 / 1_000.0)
    } else {
        format!("{bytes_per_sec}B/s")
    }
}
