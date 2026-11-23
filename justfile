_default:
    @just --list

# Install exitnodule into ~/.cargo/bin.
install:
    cargo install --path .

# Format, lint and build, as CI does.
check:
    cargo fmt --check
    cargo clippy --release -- -D warnings
    cargo build --release
