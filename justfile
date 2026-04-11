cargo-web := "RUSTFLAGS='-C target-feature=+atomics,+bulk-memory  \
-C link-arg=--shared-memory \
-C link-arg=--max-memory=1073741824 \
-C link-arg=--import-memory \
-C link-arg=--export=__wasm_init_tls \
-C link-arg=--export=__tls_size \
-C link-arg=--export=__tls_align \
-C link-arg=--export=__tls_base \
--cfg getrandom_backend=\"wasm_js\"' \
rustup run nightly-2026-04-10 \
cargo -Z build-std=std,panic_abort"

_default:
  just --list --unsorted

PKG_DIR  := "pkg"
PKG_JS   := PKG_DIR / "halfspace.js"
PKG_WA   := PKG_DIR / "halfspace.opt.wasm"
DIST_DIR := "dist"
CUT8     := ".{56}$" # regex to strip the trailing 56 characters
OPT      := "-O"

# Build a web application in `dist/`
dist:
    just _dist '{{OPT}}'

# Build a web application in `dist/` without optimization
dist-fast:
    just _dist ''

_dist opt:
    rustup +nightly target add wasm32-unknown-unknown
    {{cargo-web}} build --lib --release --target wasm32-unknown-unknown
    wasm-bindgen target/wasm32-unknown-unknown/release/halfspace.wasm --out-dir {{PKG_DIR}} --target web
    wasm-opt {{opt}} pkg/halfspace_bg.wasm -o {{PKG_WA}}
    mkdir -p {{DIST_DIR}}
    rm    -rf {{DIST_DIR}}/*
    cp {{PKG_WA}} '{{DIST_DIR}}/halfspace.{{replace_regex(sha256_file(PKG_WA), CUT8, "")}}.wasm'
    cp {{PKG_JS}} '{{DIST_DIR}}/halfspace.{{replace_regex(sha256_file(PKG_JS), CUT8, "")}}.js'
    cat web/index.html \
        | sed s/JSHASH/{{replace_regex(sha256_file(PKG_JS), CUT8, "")}}/g \
        | sed s/WAHASH/{{replace_regex(sha256_file(PKG_WA), CUT8, "")}}/g \
        > {{DIST_DIR}}/index.html
    cp -r web/htaccess {{DIST_DIR}}/.htaccess
    cp -r pkg/snippets {{DIST_DIR}}/

_serve opt:
    just _dist '{{opt}}'
    npx serve -c ../web/serve.json {{DIST_DIR}}

# Build and serve the web application
serve:
    just _serve '{{OPT}}'

# Build and serve the web application without optimization
serve-fast:
    just _serve ''

# Run `cargo check` for both native and web builds
check:
    just check-native
    just check-web

# Run `cargo check` for the native build
check-native:
    cargo check

# Run `cargo check` for the web build
check-web:
    {{cargo-web}} check --lib --target=wasm32-unknown-unknown

# Run `cargo clippy` for both native and web builds
clippy:
    cargo clippy
    {{cargo-web}} clippy --lib --target=wasm32-unknown-unknown

# Checks all of the shaders with `naga`
naga:
    naga --bulk-validate shaders/*.wgsl

# Deploy the demo to `mattkeeter.com/projects/halfspace/demo`
deploy:
    just dist
    rsync -avz --delete -e ssh {{DIST_DIR}} mkeeter@mattkeeter.com:mattkeeter.com/projects/halfspace/demo
