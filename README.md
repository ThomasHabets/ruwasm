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

## Useful links

* <https://notes.brooklynzelenka.com/Blog/Notes-on-Writing-Wasm>
