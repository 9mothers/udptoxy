use rand::Rng;
use std::{env, net::UdpSocket, thread, time::Duration};

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let port = env::args()
        .nth(1)
        .ok_or("usage: udp_blaster <port>")?
        .parse::<u16>()?;

    let target = format!("127.0.0.1:{port}");
    let socket = UdpSocket::bind("0.0.0.0:0")?;

    let mut rng = rand::rng();
    let mut count: u64 = 0;

    loop {
        let size = rng.random_range(16..=256);
        let mut buf = vec![0u8; size];
        rng.fill(&mut buf[..]);

        socket.send_to(&buf, &target)?;
        count += 1;
        if count.is_multiple_of(100) {
            println!("sent {count} packets to {target}");
        }
        thread::sleep(Duration::from_millis(50));
    }
}
