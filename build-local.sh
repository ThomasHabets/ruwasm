#!/usr/bin/env bash
set -euo pipefail

WEBD="web"
PREFIX="ruwasm"
UI_MANIFEST="$(cargo metadata --format-version=1 | jq -r '.packages[] | select(.name=="rustradio-ui") | .manifest_path' | head -n1)"
UI_DIR="$(dirname "$UI_MANIFEST")"
UI_ASSETS="${UI_DIR}/assets"

TMPD="$(mktemp -d)"
if [[ ${1:-release} = "release" ]]; then
        wasm-pack build --target web -d "$TMPD/$PREFIX" --profiling
else
        wasm-pack build --target web -d "$TMPD/$PREFIX" --dev
fi
GIT="$(git describe --tags --dirty --always)"
cp \
        "$WEBD/index.html" \
        "$WEBD/style.css" \
        "$WEBD/wasm-mod.js" \
        "$TMPD/$PREFIX/"
cp "$UI_ASSETS/bootstrap.js" "$TMPD/$PREFIX/rustradio-ui-bootstrap.js"

sed -i "s/GIT_VERSION_NOT_SET/$GIT/g" "$TMPD/$PREFIX/index.html"
(
        cd "$TMPD" && tar czf - "$PREFIX"
) > ruwasm.tgz
