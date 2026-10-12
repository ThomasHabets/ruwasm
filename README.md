# Temporary repo. This will become rustradio-ui

```
./build-local.sh \
    && ssh example.com "cd /var/www/wasm/ && tar xfz -" < ruwasm.tgz
```

## Quick start

Go to <https://thomashabets.github.io/ruwasm/>, set the sample rate and
frequency offset, and click "Start RustRadio graph". Then select a capture
file or connect an RTL-SDR through the browser.

For an RTL-SDR capture or live receiver, enable "RTL-SDR 8-bit I/Q input".
For a 32-bit complex I/Q capture, leave it unchecked.

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
