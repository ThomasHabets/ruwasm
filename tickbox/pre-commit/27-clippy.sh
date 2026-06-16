#!/usr/bin/env bash
set -ueo pipefail
cd "$TICKBOX_TEMPDIR/work"
export CARGO_TARGET_DIR="$TICKBOX_CWD/target/${TICKBOX_BRANCH}.clippy"
export RUSTFLAGS="--cfg=web_sys_unstable_apis"
exec cargo clippy --target wasm32-unknown-unknown --workspace --all-features --all-targets -- -D warnings -D clippy::pedantic
