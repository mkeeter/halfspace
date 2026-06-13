# Halfspace
[Try the demo](https://www.mattkeeter.com/projects/halfspace/demo/?example=spinner.half)

Halfspace is an experimental IDE for doing solid modeling with distance fields.

It is lamentably undocumented.
See the [`fidget::rhai`](https://docs.rs/fidget-rhai/latest/fidget_rhai/index.html)
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

## LLM usage
I don't use LLMs to write non-trivial code (or documentation) in
[Fidget](https://github.com/mkeeter/fidget) or Halfspace. In particular, I
eschew agentic systems like Claude Code.  One goal of these projects is to find
the "right" APIs and software architecture for working with implicit surfaces,
and I have to be using the APIs myself to discover rough edges and seams.
Agentic loops are incredibly good at bandaging over paper cuts, which would
defeat the purpose.

However, I _do_ use LLMs for code review (typically using GitHub Copilot) and
brainstorming (occasional chats with frontier models). Along with correct API
design, an overarching goal of the project is to be as good as possible on
various axes: correctness, performance, usability, documentation, etc.  Since
I'm the sole author, adding an additional layer of review helps me deliver
better software.
