rustup := "RUSTFLAGS='-C target-feature=+atomics,+bulk-memory --cfg getrandom_backend=\"wasm_js\"' \
        rustup run nightly-2025-06-30"
flags := "-Z build-std=std,panic_abort"

# Build a web application in `pkg/`
dist:
    {{rustup}} cargo build --lib --release --target wasm32-unknown-unknown {{flags}}
    wasm-bindgen target/wasm32-unknown-unknown/release/halfspace.wasm --out-dir pkg --target web
    wasm-opt -O pkg/halfspace_bg.wasm -o pkg/halfspace_bg.opt.wasm
    mv pkg/halfspace_bg.opt.wasm pkg/halfspace_bg.wasm
    cp -r web/index.html pkg

# Build and serve the web application
serve:
    just dist
    npx serve -c ../web/serve.json pkg

# Run `cargo check` for both native and web builds
check:
    cargo check
    {{rustup}} cargo check --lib --target=wasm32-unknown-unknown {{flags}}

