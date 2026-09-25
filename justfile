set shell := ["bash", "-e", "-u", "-o", "pipefail", "-c"]

export RUST_BACKTRACE := 'full'

[group("z")]
@def:
  just -l

clean:
  cargo clean

fmt:
  cargo fmt

test:
  cargo test

build: fmt
  cargo build

clippy: fmt build
  cargo clippy


