#!/usr/bin/env bash
set -euo pipefail
cd "$(dirname "$0")/.."
version="$1"
[[ "$version" =~ ^[0-9]+\.[0-9]+\.[0-9]+$ ]] || { printf 'Expected numeric major.minor.patch\n' >&2; exit 1; }
[[ "$(uname -s)" == Linux ]] || { printf 'Build remote artifacts in Linux\n' >&2; exit 1; }
export WARP_CUSTOM_RELEASE_VERSION="$version"
export GIT_RELEASE_TAG="personal-v$version"
cargo build --locked --profile release-lto --bin warp-oss --features release_bundle,gui,nld_classifier_v3,nld_heuristic_v2
case "$(uname -m)" in
  aarch64|arm64) arch=aarch64 ;;
  x86_64|amd64) arch=x86_64 ;;
  *) printf 'Unsupported architecture\n' >&2; exit 1 ;;
esac
python3 distribution/package_remote.py "${CARGO_TARGET_DIR:-target}/release-lto/warp-oss" "$version" "$arch"
