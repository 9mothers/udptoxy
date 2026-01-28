# udptoxy

UDP toxiproxy-style CLI/TUI that forwards UDP bidirectionally between an inbound port and an outbound address while simulating latency, jitter, packet loss, and rate limits in both directions.

![demo](demo.gif)

## Run

```bash
just dev 9000 9001
```

You can also pass full addresses:

```bash
just dev 0.0.0.0:9000 127.0.0.1:9001
```

If you pass only a port number, `--in` binds to `0.0.0.0:<port>` and `--out` targets `127.0.0.1:<port>`.

## CLI options

- `--in <ADDR_OR_PORT>`
- `--out <ADDR_OR_PORT>`
- `--latency-ms <u64>`
- `--jitter-ms <u64>`
- `--loss-percent <f64>`
- `--rate-bytes-per-sec <u64>` (0 disables, drops packets when exceeded)

Note: IPv4 only (no IPv6 or hostname resolution).
Packet list length is based on current terminal height.
The packet table shows direction (`in->out` and `out->in`).

## TUI shortcuts

- `l` toggle latency
- `j` toggle jitter
- `p` toggle loss
- `r` toggle rate limit
- `Tab` / `Shift+Tab` or `Left` / `Right` cycle selected setting
- `+` / `-` or `Up` / `Down` adjust selected setting
- `q` quit

Rate adjustments use preset steps (10KB/s .. 100MB/s).
