#!/usr/bin/env bash
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
TMPD="$(mktemp -d)"
trap 'rm -rf "$TMPD"' EXIT

cd "$ROOT"
./build-local.sh dev

mkdir -p "$TMPD/site"
tar xzf "$ROOT/ruwasm.tgz" -C "$TMPD/site" --strip-components=1

node "$ROOT/tests/browser-smoke.mjs" "$TMPD/site"
