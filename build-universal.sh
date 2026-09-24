#!/bin/bash
# Build a universal (arm64 + x86_64) release binary into dist/.
#
# The build directory lives outside the synced Yandex.Disk tree
# (see .cargo/config.toml); mirror that decision here.
set -euo pipefail
cd "$(dirname "$0")"

TARGET_DIR="${CARGO_TARGET_DIR:-$HOME/Library/Caches/cryocon-gui-target}"
export CARGO_TARGET_DIR="$TARGET_DIR"

rustup target add aarch64-apple-darwin x86_64-apple-darwin
cargo build --release --target aarch64-apple-darwin
cargo build --release --target x86_64-apple-darwin

mkdir -p dist
lipo -create \
    "$TARGET_DIR/aarch64-apple-darwin/release/cryocon-gui" \
    "$TARGET_DIR/x86_64-apple-darwin/release/cryocon-gui" \
    -output dist/cryocon-gui

# ad-hoc signature: lets the binary run locally without a developer account
codesign --force --sign - dist/cryocon-gui >/dev/null 2>&1 || true

echo "built:"
lipo -info dist/cryocon-gui
