#!/usr/bin/env bash
set -euo pipefail

#VER="$(git describe --tags --dirty --always)"
WEBD="web"
PREFIX="ruwasm"
OUTDIR="dist"
UI_MANIFEST="$(cargo metadata --format-version=1 | jq -r '.packages[] | select(.name=="rustradio-ui") | .manifest_path' | head -n1)"
UI_DIR="$(dirname "$UI_MANIFEST")"
UI_ASSETS="${UI_DIR}/assets"

rm -rf "$OUTDIR"
mkdir -p "$OUTDIR"

GIT="$(git describe --tags --dirty --always)"
wasm-pack build --target web -d "$OUTDIR/$PREFIX"
cp "$WEBD/index.html" "$WEBD/style.css" "$WEBD/wasm-mod.js" "$WEBD/coi-serviceworker.min.js" "$OUTDIR/$PREFIX/"
cp "$UI_ASSETS/bootstrap.js" "$TMPD/$PREFIX/rustradio-ui-bootstrap.js"
sed -i "s/GIT_VERSION_NOT_SET/$GIT/g" "$OUTDIR/$PREFIX/index.html"

# publish contents of ruwasm/ at site root
shopt -s dotglob
mv "$OUTDIR/$PREFIX"/* "$OUTDIR"/
rmdir "$OUTDIR/$PREFIX"
