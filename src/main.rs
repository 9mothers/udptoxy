mod proxy;
mod types;
mod ui;

use clap::Parser;
use parking_lot::{Mutex, RwLock};
use std::{
    collections::VecDeque,
    net::{IpAddr, Ipv4Addr},
    sync::Arc,
    time::Duration,
};
use tokio::sync::watch;

use crate::types::{Args, EVENT_BUFFER, EventBuffer, ProxySettings, parse_addr};

fn main() -> Result<(), Box<dyn std::error::Error>> {
    const REFRESH_MS: u64 = 200;

    let args = Args::parse();

    let in_addr = parse_addr(&args.in_addr, IpAddr::V4(Ipv4Addr::UNSPECIFIED))
        .map_err(|e| format!("--in {e}"))?;
    let out_addr = parse_addr(&args.out_addr, IpAddr::V4(Ipv4Addr::LOCALHOST))
        .map_err(|e| format!("--out {e}"))?;

    let settings = Arc::new(RwLock::new(ProxySettings::from_args(&args)));
    let events: EventBuffer = Arc::new(Mutex::new(VecDeque::with_capacity(EVENT_BUFFER)));
    let (shutdown_tx, shutdown_rx) = watch::channel(false);

    let settings_clone = Arc::clone(&settings);
    let events_clone = Arc::clone(&events);
    let proxy_thread = std::thread::spawn(move || {
        let runtime = tokio::runtime::Builder::new_multi_thread()
            .enable_io()
            .enable_time()
            .build();
        let runtime = match runtime {
            Ok(rt) => rt,
            Err(err) => {
                eprintln!("Failed to start runtime: {err}");
                return;
            }
        };
        if let Err(err) = runtime.block_on(proxy::run_proxy(
            in_addr,
            out_addr,
            settings_clone,
            events_clone,
            shutdown_rx,
            None,
        )) {
            eprintln!("Proxy error: {err}");
        }
    });

    let refresh = Duration::from_millis(REFRESH_MS);
    let ui_result = ui::run_ui(in_addr, out_addr, refresh, settings, events);

    let _ = shutdown_tx.send(true);
    let _ = proxy_thread.join();
    ui_result
}
