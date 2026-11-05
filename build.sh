#!/usr/bin/env bash
# Build antpad inside a Rust container with podman.
# Output: ./dist/antpad   (cargo downloads are cached in a named podman volume)
set -euo pipefail
cd "$(dirname "$0")"

IMAGE="${RUST_IMAGE:-docker.io/library/rust:1-slim}"
mkdir -p dist

podman run --rm \
  -v "$PWD":/src:Z \
  -v antpad-cargo:/usr/local/cargo/registry:Z \
  -w /src \
  -e CARGO_TARGET_DIR=/tmp/target \
  "$IMAGE" \
  sh -c 'cargo build --release && cp /tmp/target/release/antpad /src/dist/antpad'

echo "Built: $PWD/dist/antpad"
