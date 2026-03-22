#!/usr/bin/env bash
set -euo pipefail

WEBD="web"
PREFIX="ruwasm"
OUTDIR="dist"

rm -rf "$OUTDIR"
mkdir -p "$OUTDIR"

wasm-pack build --target web -d "$OUTDIR/$PREFIX"
cp "$WEBD/index.html" "$WEBD/worker.js" "$OUTDIR/$PREFIX/"

# publish contents of ruwasm/ at site root
shopt -s dotglob
mv "$OUTDIR/$PREFIX"/* "$OUTDIR"/
rmdir "$OUTDIR/$PREFIX"
