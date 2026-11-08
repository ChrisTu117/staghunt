#!/usr/bin/env bash
# Build the contract to wasm and copy it (with its ABI) into res/.
# Needs Rust >= 1.95 with the wasm32-unknown-unknown target and cargo-near
# (https://github.com/near/cargo-near). A prebuilt copy is committed in res/.
set -euo pipefail
cd "$(dirname "$0")/.."
TARGET="${CARGO_TARGET_DIR:-$PWD/target}"
cargo near build non-reproducible-wasm
cp "$TARGET/near/staghunt.wasm" "$TARGET/near/staghunt_abi.json" res/
ls -l res/
sha256sum res/staghunt.wasm
