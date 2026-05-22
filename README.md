# Temporary repo, to be moved into rustradio as an example

```
./build-local.sh \
    && ssh example.com "cd /var/www/wasm/ && tar xfz -" < ruwasm.tgz
```

## TODO

* Send the shape of the graph for graphviz-like rendering.
* Send log lines to a text box.
* Send decodes to a text box too.
* Make most of time sink in HTML templated instead of assuming HTML has it.
* Waterfall sink.
* Fix the crashing bug on large inputs.

## Websocket

You can run a websocket data provider on localhost, or somewhere that has a
valid HTTPS cert. For `ws://`, only localhost will work, because of browser
security boundaries.

Simple proof of concept example included:

```
cargo run --bin ws_stdout -- cat some_file.c32
```

### Websocket live stream

It's not keeping up with real time, and delays instead of drops data (TODO), but
you can stream directly from RTL-SDR using a rustradio example binary
`rtl_downsampled` (without downsampling it works even worse, since the minimum
data rate from an RTL-SDR dongle is 200k+ sps).

```
cargo run --bin ws_stdout -- -- .../path/to/rtl_downsampled --freq 144750000
```

Don't forget to tick the RTL-SDR format checkbox in the UI.

## Websocket

You can run a websocket data provider on localhost, or somewhere that has a
valid HTTPS cert. For `ws://`, only localhost will work, because of browser
security boundaries.

Simple proof of concept example included:

```
cargo run --bin ws_stdout -- cat some_file.c32
```

## Useful links

* <https://notes.brooklynzelenka.com/Blog/Notes-on-Writing-Wasm>
