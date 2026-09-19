#!/usr/bin/env bash
set -euo pipefail
cd "$(dirname "$0")/.."
[[ "$(uname -s)" == Darwin ]]
xcodebuild -version
if ! xcrun -sdk macosx metal -v >/dev/null 2>&1; then
  xcodebuild -downloadComponent MetalToolchain
fi
rustup show active-toolchain
brew install cmake pkg-config protobuf
if [[ -d app ]]; then
  mkdir -p target/personal-tools/bin
  npm install --prefix target/personal-tools --no-save corepack@0.34.0
  node target/personal-tools/node_modules/corepack/dist/corepack.js enable --install-directory "$PWD/target/personal-tools/bin"
  if [[ -n "${GITHUB_PATH:-}" ]]; then
    echo "$PWD/target/personal-tools/bin" >> "$GITHUB_PATH"
  fi
  # Install from the public registry, never a precompiled upstream-internal cache.
  cargo install cargo-bundle --version 0.11.0 --locked
else
  cargo install cargo-bundle --git https://github.com/zed-industries/cargo-bundle.git --rev 2be2669972dff3ddd4daf89a2cb29d2d06cad7c7 --locked
fi
