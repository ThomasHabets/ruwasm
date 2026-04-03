# Temporary repo, to be moved into rustradio as an example

cargo install wasm-pack
wasm-pack build --target web
stuff now gets added to pkg/, and needs to work with the HTML and JS in web/

```
clear \
&& wasm-pack build --target web --dev \
&& scp -r web/*.html web/*.js pkg/*.wasm pkg/*.js example.com:/var/www/wasm/
```

`--dev` in order to get backtraces.

## Useful links

* <https://notes.brooklynzelenka.com/Blog/Notes-on-Writing-Wasm>
