cargo-web := "RUSTFLAGS='-C target-feature=+atomics,+bulk-memory --cfg getrandom_backend=\"wasm_js\"' \
rustup run nightly-2025-06-30 \
cargo -Z build-std=std,panic_abort"

# Build a web application in `pkg/`
dist:
    {{cargo-web}} build --lib --release --target wasm32-unknown-unknown 
    wasm-bindgen target/wasm32-unknown-unknown/release/halfspace.wasm --out-dir pkg --target web
    wasm-opt -O pkg/halfspace_bg.wasm -o pkg/halfspace_bg.opt.wasm
    mv pkg/halfspace_bg.opt.wasm pkg/halfspace_bg.wasm
    cp -r web/index.html pkg
    cp -r web/htaccess pkg/.htaccess

# Build and serve the web application
serve:
    just dist
    npx serve -c ../web/serve.json pkg

# Run `cargo check` for both native and web builds
check:
    cargo check
    {{cargo-web}} check --lib --target=wasm32-unknown-unknown

# Run `cargo clippy` for both native and web builds
clippy:
    cargo clippy
    {{cargo-web}} clippy --lib --target=wasm32-unknown-unknown

# Checks all of the shaders with `naga`
naga:
    naga --bulk-validate shaders/*.wgsl

deploy:
    just dist
    rsync -avz --delete -e ssh ./pkg/ mkeeter@mattkeeter.com:mattkeeter.com/projects/halfspace/demo
