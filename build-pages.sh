#!/usr/bin/env bash
set -euo pipefail

#VER="$(git describe --tags --dirty --always)"
WEBD="web"
PREFIX="ruwasm"
OUTDIR="dist"

rm -rf "$OUTDIR"
mkdir -p "$OUTDIR"

GIT="$(git describe --tags --dirty --always)"
wasm-pack build --target web -d "$OUTDIR/$PREFIX"
cp "$WEBD/index.html" "$WEBD/wasm-mod.js" "$OUTDIR/$PREFIX/"
sed -i "s/GIT_VERSION_NOT_SET/$GIT/g" "$OUTDIR/$PREFIX/index.html"

# publish contents of ruwasm/ at site root
shopt -s dotglob
mv "$OUTDIR/$PREFIX"/* "$OUTDIR"/
rmdir "$OUTDIR/$PREFIX"
