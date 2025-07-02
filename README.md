rustup target add wasm32-unknown-unknow
cargo build --target wasm32-unknown-unknown --release
cp target/wasm32-unknown-unknown/release/ruwasm.wasm web/add.wasm
