#!/bin/bash
set -euo pipefail

case "$(/usr/sbin/sysctl -n hw.model)" in
  VirtualMac*) ;;
  *) printf '%s\n' 'Refusing to build outside a macOS virtual machine.' >&2; exit 1 ;;
esac

if (($# < 3)); then
  printf '%s\n' 'Usage: build.sh {warp|zed} GUEST_SOURCE_DIRECTORY COMMAND [ARGS...]' >&2
  exit 2
fi
project=$1
source_dir=$2
shift 2
case "$project" in
  warp|zed) ;;
  *) printf 'Unsupported build project: %s\n' "$project" >&2; exit 2 ;;
esac

cd "$source_dir"
source_dir=$(pwd -P)
guest_home=$(cd "$HOME" && pwd -P)
case "$source_dir/" in
  "$guest_home/Developer/"*) ;;
  *) printf '%s\n' 'Source must be copied into the guest under ~/Developer, not built on a host share.' >&2; exit 1 ;;
esac

cache_root="$guest_home/Library/Caches/EternalWarpBuild"
export CARGO_HOME="$cache_root/cargo"
export RUSTUP_HOME="$cache_root/rustup"
export CARGO_TARGET_DIR="$cache_root/targets/$project"
export SCCACHE_DIR="$cache_root/sccache/$project"
export XDG_CACHE_HOME="$cache_root/xdg/$project"
export CARGO_BUILD_JOBS=2
export CMAKE_BUILD_PARALLEL_LEVEL=2
export PATH="$CARGO_HOME/bin:$guest_home/.cargo/bin:/opt/homebrew/bin:/usr/local/bin:$PATH"
unset RUSTC_WRAPPER RUSTC_WORKSPACE_WRAPPER
mkdir -p "$CARGO_HOME" "$CARGO_TARGET_DIR" "$SCCACHE_DIR" "$XDG_CACHE_HOME"
printf 'Building %s inside the guest; Cargo output: %s\n' "$project" "$CARGO_TARGET_DIR"
exec "$@"
