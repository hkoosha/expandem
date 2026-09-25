set shell := ["bash", "-e", "-u", "-o", "pipefail", "-c"]

export RUST_BACKTRACE := 'full'

[private]
@def:
  just -l

clean:
  cargo clean

fmt:
  cargo fmt

test:
  cargo test

clippy: fmt
  cargo clippy

build:
  cargo build
alias b := build

build-release: fmt clippy test
  cargo build --release
alias l := build-release

run *args:
  cargo run -- {{ args }}
alias r := run



