#!/usr/bin/env sh
set -eu
cargo check
cargo clippy -- -D warnings
cargo test
