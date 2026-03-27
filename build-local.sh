#!/usr/bin/env bash
set -euo pipefail
WEBD="web"
PREFIX="ruwasm"
TMPD="$(mktemp -d)"
wasm-pack build --target no-modules -d "$TMPD/$PREFIX" --dev
cp "$WEBD/index.html" "$WEBD/worker.js" "$TMPD/$PREFIX/"
(
        cd "$TMPD" && tar czf - "$PREFIX"
) > ruwasm.tgz
