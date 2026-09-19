#!/usr/bin/env bash
set -euo pipefail
cd "$(dirname "$0")/.."
version="$1"
export PATH="$PWD/target/personal-tools/bin:$PATH"
arch=$(uname -m)
target=$(rustc -vV | sed -n 's/^host: //p')
rust_arch=${target%-apple-darwin}
# The public OSS channel has no upstream updater or proprietary channel config.
# Build with a lockfile first; cargo-bundle only packages these binaries.
cargo build --locked --profile release-lto --bin warp-oss --target "$target" --features release_bundle,extern_plist,gui,nld_classifier_v3,nld_heuristic_v2
bash script/macos/bundle --channel oss --arch "$rust_arch" --nosign --skip-build --bundle-only
app="target/$target/release-lto/bundle/osx/SashaTerm.app"
python3 distribution/package.py "$app" "$version" "$arch"
