#!/usr/bin/env bash
set -euo pipefail
cd "$(dirname "$0")/.."
version="$1"
export CARGO_ABOUT_CONFIG="$PWD/distribution/about.toml"
export CARGO_ABOUT_FAIL=1
export PATH="$PWD/target/personal-tools/bin:$PATH"
export TERM=xterm-256color
arch=$(uname -m)
target=$(rustc -vV | sed -n 's/^host: //p')
rust_arch=${target%-apple-darwin}
# The public OSS channel has no upstream updater or proprietary channel config.
# Build with a lockfile first; cargo-bundle only packages these binaries.
cargo build --locked --profile release-lto --bin warp-oss --bin generate_settings_schema --target "$target" --features release_bundle,extern_plist,gui,nld_classifier_v3,nld_heuristic_v2
# Share the app's exact target/features instead of rebuilding its library during bundling.
export SETTINGS_SCHEMA_CACHE="$PWD/target/$target/release-lto/personal-settings-schema.json"
"target/$target/release-lto/generate_settings_schema" --channel oss "$SETTINGS_SCHEMA_CACHE"
bash script/macos/bundle --channel oss --arch "$rust_arch" --nosign --skip-build --bundle-only
app="target/$target/release-lto/bundle/osx/Warp Custom.app"
python3 distribution/package.py "$app" "$version" "$arch"
