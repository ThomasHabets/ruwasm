# Temporary repo. This will become rustradio-ui

```
./build-local.sh \
    && ssh example.com "cd /var/www/wasm/ && tar xfz -" < ruwasm.tgz
```

## Quick start

Build the `ws_stdout` example from this repo.

```
cargo build --release --bin ws_stdout
# binary now in target/release/ws_stdout
```

Then build the `rtl_data_stream` binary from [rustradio][rustradio] repo.

```
cargo build -F rtlsdr --release --example rtl_data_stream
# binary now in target/release/examples/rtl_data_stream
```

Then start them, locally.

```
ws_stdout -- rtl_data_stream --freq 144750000
```

Then go to <https://thomashabets.github.io/ruwasm/>, check the "RTL-SDR unsigned
8-bit I/Q input" box, click "start rustradio", then "Connect WebSocket stream".

Enjoy the awesomeness.

## TODO

* Send the shape of the graph for graphviz-like rendering.
* Make most of time sink in HTML templated instead of assuming HTML has it.
* Waterfall sink.

## Browser smoke test

Run the packaged app in headless Chrome and verify that the WASM UI starts and
generates the time sink component:

```
tests/browser-smoke.sh
```

## WebSocket

You can run a websocket data provider on localhost, or somewhere that has a
valid HTTPS cert. For `ws://`, only localhost will work, because of browser
security boundaries.

The WebSocket source expects the `DATA_STREAM.md` protocol from rustradio. Raw
byte streams such as `cat some_file.c32` should use the file input instead.

### WebSocket live stream

It's not keeping up with real time, and delays instead of drops data (TODO), but
you can stream directly from RTL-SDR using a rustradio example binary
`rtl_data_stream`.

```
cargo run --bin ws_stdout -- \
    cargo run --manifest-path ../rustradio/Cargo.toml \
        --example rtl_data_stream --features rtlsdr -- \
        --freq 144750000
```

Don't forget to tick the RTL-SDR format checkbox in the UI.

## Life of a WASM worker

The bootstrapping of WASM has a few steps, even though wasm-pack and
`wasm_bindgen` hide a lot of complexity.

First the HTML imports `web/wasm-mod.js` as a JS module. That file has our WASM
bootstrapping code, and is the only non-generated JS in this project.

This javascript creates the shared memory area, initializes the main UI WASM,
and then starts it by calling the async function `src/lib.rs:start()`. The main
UI thread is thereby done with JS, and will only run Rust WASM from now on.

After setting up logging, the main UI thread starts the worker, in
`src/mainthread.rs:worker()`. Like the HTML, it points back to
`web/wasm-mod.js` to load the WASM.

The JS detects that it is now running in the worker, and therefore waits to
receive the `WebAssembly.Memory`. It then uses the memory to initialize the
worker WASM. Then it starts the worker, and no more JS will execute.

After that, all communication between main thread and worker happens via
`post_message()`, defined both in `src/mainthread.rs` and `src/worker.rs`,
depending on which code is running.

## Useful links

* <https://notes.brooklynzelenka.com/Blog/Notes-on-Writing-Wasm>

[rustradio]: https://github.com/ThomasHabets/rustradio
