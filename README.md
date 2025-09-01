# Halfspace
[Try the demo](https://www.mattkeeter.com/projects/halfspace/demo/?example=spinner.half)

Halfspace is an experimental IDE for doing solid modeling with distance fields.

It is lamentably undocumented.
See the [`fidget::rhai`](https://docs.rs/fidget/latest/fidget/rhai/index.html)
documentation for details on scripting; otherwise, look to the examples for
inspiration.  When in doubt, read the source code!

## Platforms
Halfspace runs as either a web or native application.

### Native
Install [Rust](https://www.rust-lang.org/), then run
```
cargo run --release
```

### Web
Install [Rust](https://www.rust-lang.org/), [`wasm-bindgen`](https://github.com/wasm-bindgen/wasm-bindgen), [`wasm-opt`](https://github.com/WebAssembly/binaryen),
and [`npm`](https://www.npmjs.com/).

```
just serve # serves a local copy of the app
just dist  # builds the web app in `pkg/`
```

