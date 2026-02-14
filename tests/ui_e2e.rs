use std::{
    net::UdpSocket,
    path::{Path, PathBuf},
    process::Command,
    time::{Duration, Instant},
};

fn tmux_available() -> bool {
    Command::new("tmux")
        .arg("-V")
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status()
        .map(|s| s.success())
        .unwrap_or(false)
}

fn binary_path() -> PathBuf {
    PathBuf::from(env!("CARGO_BIN_EXE_udptoxy"))
}

fn free_port() -> u16 {
    let sock = UdpSocket::bind("127.0.0.1:0").unwrap();
    sock.local_addr().unwrap().port()
}

struct TmuxSession {
    name: String,
}

impl TmuxSession {
    fn new(name: &str, binary: &Path, args: &[&str], width: u16, height: u16) -> Self {
        let session_name = format!("udptoxy_test_{}_{}", name, std::process::id());

        let _ = Command::new("tmux")
            .args(["kill-session", "-t", &session_name])
            .output();

        let cmd = format!("{} {}", binary.display(), args.join(" "));

        let status = Command::new("tmux")
            .args([
                "new-session",
                "-d",
                "-s",
                &session_name,
                "-x",
                &width.to_string(),
                "-y",
                &height.to_string(),
                &cmd,
            ])
            .status()
            .expect("failed to start tmux session");

        assert!(status.success(), "tmux new-session failed");

        Self {
            name: session_name,
        }
    }

    fn capture(&self) -> String {
        let output = Command::new("tmux")
            .args(["capture-pane", "-t", &self.name, "-p"])
            .output()
            .expect("tmux capture-pane failed");
        String::from_utf8_lossy(&output.stdout).to_string()
    }

    fn send_keys(&self, keys: &str) {
        let status = Command::new("tmux")
            .args(["send-keys", "-t", &self.name, keys])
            .status()
            .expect("tmux send-keys failed");
        assert!(status.success(), "tmux send-keys failed");
    }

    fn wait_for(&self, text: &str, timeout: Duration) -> String {
        let start = Instant::now();
        loop {
            let screen = self.capture();
            if screen.contains(text) {
                return screen;
            }
            if start.elapsed() > timeout {
                panic!(
                    "timed out waiting for '{}' in tmux output:\n{}",
                    text, screen
                );
            }
            std::thread::sleep(Duration::from_millis(100));
        }
    }

    fn is_alive(&self) -> bool {
        Command::new("tmux")
            .args(["has-session", "-t", &self.name])
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .status()
            .map(|s| s.success())
            .unwrap_or(false)
    }
}

impl Drop for TmuxSession {
    fn drop(&mut self) {
        let _ = Command::new("tmux")
            .args(["kill-session", "-t", &self.name])
            .output();
    }
}

const EXPECTED_INITIAL: &str = include_str!("ui_snapshot.txt");

#[test]
fn ui_initial_render() {
    if !tmux_available() {
        eprintln!("skipping UI test: tmux not found");
        return;
    }

    let binary = binary_path();
    let session = TmuxSession::new(
        "initial_render",
        &binary,
        &["--in", "19876", "--out", "19877"],
        120,
        15,
    );

    let screen = session.wait_for("Status", Duration::from_secs(5));

    assert_eq!(screen.trim_end(), EXPECTED_INITIAL.trim_end());
}

#[test]
fn ui_toggle_latency() {
    if !tmux_available() {
        eprintln!("skipping UI test: tmux not found");
        return;
    }

    let in_port = free_port();
    let out_port = free_port();
    let binary = binary_path();
    let in_arg = in_port.to_string();
    let out_arg = out_port.to_string();
    let session = TmuxSession::new(
        "toggle_latency",
        &binary,
        &["--in", &in_arg, "--out", &out_arg],
        120,
        15,
    );

    session.wait_for("Status", Duration::from_secs(5));
    session.send_keys("l");
    let screen = session.wait_for("Latency [l] ON", Duration::from_secs(3));
    assert!(screen.contains("Latency [l] ON 0ms"));
}

#[test]
fn ui_adjust_latency_value() {
    if !tmux_available() {
        eprintln!("skipping UI test: tmux not found");
        return;
    }

    let in_port = free_port();
    let out_port = free_port();
    let binary = binary_path();
    let in_arg = in_port.to_string();
    let out_arg = out_port.to_string();
    let session = TmuxSession::new(
        "adjust_latency",
        &binary,
        &["--in", &in_arg, "--out", &out_arg],
        120,
        15,
    );

    session.wait_for("Status", Duration::from_secs(5));

    // Latency is the default selected setting; Up increases by 10ms step
    session.send_keys("Up");
    let screen = session.wait_for("Latency [l] ON 10ms", Duration::from_secs(3));
    assert!(screen.contains("Latency [l] ON 10ms"));

    session.send_keys("Up");
    let screen = session.wait_for("Latency [l] ON 20ms", Duration::from_secs(3));
    assert!(screen.contains("Latency [l] ON 20ms"));
}

#[test]
fn ui_packet_table_shows_forward() {
    if !tmux_available() {
        eprintln!("skipping UI test: tmux not found");
        return;
    }

    let in_port = free_port();

    // Start a simple UDP echo server
    let echo_sock = UdpSocket::bind("127.0.0.1:0").unwrap();
    let echo_addr = echo_sock.local_addr().unwrap();
    echo_sock
        .set_read_timeout(Some(Duration::from_secs(5)))
        .unwrap();
    let echo_handle = std::thread::spawn(move || {
        let mut buf = [0u8; 65535];
        if let Ok((len, src)) = echo_sock.recv_from(&mut buf) {
            let _ = echo_sock.send_to(&buf[..len], src);
        }
    });

    let binary = binary_path();
    let in_arg = in_port.to_string();
    let out_arg = echo_addr.to_string();
    let session = TmuxSession::new(
        "packet_forward",
        &binary,
        &["--in", &in_arg, "--out", &out_arg],
        120,
        15,
    );

    session.wait_for("Status", Duration::from_secs(5));

    // Send a UDP packet through the proxy
    let client = UdpSocket::bind("127.0.0.1:0").unwrap();
    client
        .send_to(b"hello", format!("127.0.0.1:{in_port}"))
        .unwrap();

    let screen = session.wait_for("forward", Duration::from_secs(5));
    assert!(screen.contains("forward"));
    assert!(screen.contains("in->out"));

    let _ = echo_handle.join();
}

#[test]
fn ui_quit_exits() {
    if !tmux_available() {
        eprintln!("skipping UI test: tmux not found");
        return;
    }

    let in_port = free_port();
    let out_port = free_port();
    let binary = binary_path();
    let in_arg = in_port.to_string();
    let out_arg = out_port.to_string();
    let session = TmuxSession::new(
        "quit",
        &binary,
        &["--in", &in_arg, "--out", &out_arg],
        120,
        15,
    );

    session.wait_for("Status", Duration::from_secs(5));
    session.send_keys("q");

    let start = Instant::now();
    loop {
        if !session.is_alive() {
            break;
        }
        if start.elapsed() > Duration::from_secs(5) {
            panic!("udptoxy did not exit after 'q' was pressed");
        }
        std::thread::sleep(Duration::from_millis(100));
    }
}
