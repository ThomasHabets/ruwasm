#!/usr/bin/env bash
#
# async-channel, when built with `std` feature, will do a mutex lock sometimes.
# This is not allowed in the main UI thread.
#
# So here we assert that we don't have that feature enabled.

set -euo pipefail

for crate in async-channel event-listener; do
    features="$(
        cargo tree \
            --locked \
            --target wasm32-unknown-unknown \
            --edges normal,build \
            --package "$crate" \
            --depth 0 \
            --format '{f}'
    )"
    if [[ ",${features}," == *,std,* ]]; then
        echo "ERROR: ${crate} enables std: ${features}" >&2
        exit 1
    fi
done
