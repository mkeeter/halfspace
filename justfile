dist:
    RUSTFLAGS='-C target-feature=+atomics,+bulk-memory --cfg getrandom_backend="wasm_js"' \
        rustup run nightly-2025-06-30 \
        cargo build --lib --release --target wasm32-unknown-unknown -Z build-std=std,panic_abort
    wasm-bindgen target/wasm32-unknown-unknown/release/halfspace.wasm --out-dir pkg --target web
    wasm-opt -O pkg/halfspace_bg.wasm -o pkg/halfspace_bg.opt.wasm
    mv pkg/halfspace_bg.opt.wasm pkg/halfspace_bg.wasm
    cp -r web/index.html pkg

serve:
    just dist
    npx serve -c ../web/serve.json pkg
