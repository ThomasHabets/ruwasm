#!/usr/bin/env bash
set -ueo pipefail
cd "$TICKBOX_TEMPDIR/work"
export CARGO_TARGET_DIR="$TICKBOX_CWD/target/${TICKBOX_BRANCH}.test.normal"
export RUSTFLAGS="--cfg=web_sys_unstable_apis"
cargo test --workspace --target wasm32-unknown-unknown --no-run
if [[ ${CLEANUP:-} = true ]]; then
        rm -fr "${CARGO_TARGET_DIR?}"
fi
