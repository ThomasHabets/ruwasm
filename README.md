# Temporary repo, to be moved into rustradio as an example

```
./build-local.sh \
    && ssh example.com "cd /var/www/wasm/ && tar xfz -" < ruwasm.tgz
```

## TODO

* Send the shape of the graph for graphviz-like rendering.
* Make most of time sink in HTML templated instead of assuming HTML has it.
* Waterfall sink.

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

## Useful links

* <https://notes.brooklynzelenka.com/Blog/Notes-on-Writing-Wasm>
