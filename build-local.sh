#!/usr/bin/env bash
set -euo pipefail
# RUSTFLAGS2='-C target-feature=+atomics,+bulk-memory,+mutable-globals'
#
WEBD="web"
PREFIX="ruwasm"
TMPD="$(mktemp -d)"
#wasm-pack build --target web -d "$TMPD/$PREFIX" --dev
#wasm-pack build --target web -d "$TMPD/$PREFIX" --release -- . -Z build-std=panic_abort,std
wasm-pack build --target web -d "$TMPD/$PREFIX" --release 
cp "$WEBD/index.html" "$WEBD/wasm-mod.js" "$TMPD/$PREFIX/"
(
        cd "$TMPD" && tar czf - "$PREFIX"
) > ruwasm.tgz
