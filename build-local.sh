#!/usr/bin/env bash
set -euo pipefail
WEBD="web"
PREFIX="ruwasm"
TMPD="$(mktemp -d)"
if [[ ${1:-release} = "release" ]]; then
        wasm-pack build --target web -d "$TMPD/$PREFIX" --release
else
        wasm-pack build --target web -d "$TMPD/$PREFIX" --dev
fi
GIT="$(git describe --tags --dirty --always)"
cp "$WEBD/index.html" "$WEBD/style.css" "$WEBD/wasm-mod.js" "$TMPD/$PREFIX/"
sed -i "s/GIT_VERSION_NOT_SET/$GIT/g" "$TMPD/$PREFIX/index.html"
(
        cd "$TMPD" && tar czf - "$PREFIX"
) > ruwasm.tgz
