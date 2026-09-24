#!/bin/bash
set -euo pipefail

here=$(cd "$(dirname "$0")" && pwd)
repo=$(cd "$here/../.." && pwd)
runtime=${VM_RUNTIME:-"$HOME/Library/Application Support/EternalWarpLab"}
tart=${TART:-"$runtime/tart.app/Contents/MacOS/tart"}
export TART_HOME=${TART_HOME:-"$runtime/vms"}
export TART_NO_AUTO_PRUNE=1
name=${VM_NAME:-warp-custom-test}
artifacts=${ARTIFACTS_DIR:-"$here/.state/artifacts"}
image=${VM_IMAGE:-ghcr.io/cirruslabs/macos-tahoe-base:latest}

if [[ ! -x "$tart" ]]; then
  printf '%s\n' 'Set TART to an installed Tart executable.' >&2
  exit 1
fi

case "${1:-}" in
  prepare)
    mkdir -p "$TART_HOME"
    available_kib=$(df -Pk "$TART_HOME" | awk 'END { print $4 }')
    required_kib=$((100 * 1024 * 1024))
    if ((available_kib < required_kib)); then
      printf 'VM setup blocked: %s GiB available; reserve 100 GiB for VM and build work. Set TART_HOME to a suitable volume.\n' \
        "$((available_kib / 1024 / 1024))" >&2
      exit 1
    fi
    "$tart" clone "$image" "$name" --concurrency 2
    "$tart" set "$name" --cpu 2 --memory 12288
    ;;
  run)
    mkdir -p "$artifacts"
    exec "$tart" run "$name" --no-graphics --no-audio --no-clipboard \
      --dir="artifacts:$artifacts:ro" --dir="build-tools:$here/guest:ro"
    ;;
  build)
    shift
    if (($# < 3)); then
      printf '%s\n' 'Usage: vm.sh build {warp|zed} GUEST_SOURCE_DIRECTORY COMMAND [ARGS...]' >&2
      exit 2
    fi
    exec "$tart" exec "$name" /bin/bash \
      '/Volumes/My Shared Files/build-tools/build.sh' "$@"
    ;;
  exec)
    shift
    if (($# == 0)); then
      printf '%s\n' 'Supply a command to execute INSIDE the guest.' >&2
      exit 2
    fi
    exec "$tart" exec "$name" "$@"
    ;;
  stop)
    exec "$tart" stop "$name"
    ;;
  *)
    printf 'Usage: bash %s {prepare|run|build PROJECT GUEST_SOURCE COMMAND...|exec COMMAND...|stop}\n' "$0" >&2
    exit 2
    ;;
esac
