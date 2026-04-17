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
GIT="$(git describe --tags --dirty --always)"
cp "$WEBD/index.html" "$WEBD/wasm-mod.js" "$TMPD/$PREFIX/"
sed -i "s/GIT_VERSION_NOT_SET/$GIT/g" "$TMPD/$PREFIX/index.html"
(
        cd "$TMPD" && tar czf - "$PREFIX"
) > ruwasm.tgz
