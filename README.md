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

## Useful links

* <https://notes.brooklynzelenka.com/Blog/Notes-on-Writing-Wasm>
