setup:
    git config core.hooksPath .githooks

pre_push: test
    cargo fmt --check
    cargo clippy --all-targets --all-features -- -D warnings
    cargo build --release

style:
    cargo fmt

build:
    cargo build --release

test:
    cargo test

dev in out:
    cargo run --bin udptoxy -- --in {{in}} --out {{out}}

dev_test port:
    cargo run --bin udp_blaster -- {{port}}

update:
    cargo update
