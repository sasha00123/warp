#!/bin/bash
set -euo pipefail

status_file=${1:?Supply the exit-status file path}
trap 'status=$?; printf "%s\n" "$status" > "$status_file"; printf "VM preparation finished with status %s at " "$status"; date -u' EXIT
trap 'exit 143' TERM

here=$(cd "$(dirname "$0")" && pwd)
workspace=$(cd "$here/../../.." && pwd)
export TART_HOME="$workspace/.vm"
export TART_NO_AUTO_PRUNE=1
printf 'Supervised VM preparation started at '
date -u
printf 'Worker PID: %s\n' "$$"
bash "$here/vm.sh" prepare
"$workspace/.tools/tart/tart.app/Contents/MacOS/tart" set warp-custom-test --disk-size 180
