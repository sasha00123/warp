#!/bin/bash
set -euo pipefail

here=$(cd "$(dirname "$0")" && pwd)
state="$here/.state"
docker=${DOCKER:-docker}
image=eternalwarp-ssh-lab:local
mkdir -p "$state"
chmod 700 "$state"
umask 077

case "${1:-}" in
  up)
    if [[ -f "$state/container" ]]; then
      printf '%s\n' 'A fixture is already recorded. Use down before creating another.' >&2
      exit 1
    fi
    "$docker" build --tag "$image" "$here"
    if [[ ! -f "$state/id_ed25519" ]]; then
      ssh-keygen -q -t ed25519 -N '' -C eternalwarp-test-only -f "$state/id_ed25519"
    fi
    id=$(openssl rand -hex 8)
    container="eternalwarp-lab-$id"
    "$docker" run --detach --name "$container" \
      --label "com.eternalwarp.lab.id=$id" \
      --cpus 1 --memory 256m --pids-limit 128 \
      --publish 127.0.0.1::22 \
      --mount "type=bind,src=$state/id_ed25519.pub,dst=/run/lab-authorized-key,readonly" \
      "$image" > /dev/null
    printf '%s\n' "$container" > "$state/container"
    printf '%s\n' "$id" > "$state/id"
    port=$("$docker" port "$container" 22/tcp)
    port=${port##*:}
    printf '%s\n' "$port" > "$state/port"
    ready=false
    for ((attempt = 0; attempt < 40; attempt++)); do
      if key=$("$docker" exec "$container" cat /etc/ssh/ssh_host_ed25519_key.pub 2>/dev/null); then
        printf '[127.0.0.1]:%s %s\n' "$port" "$key" > "$state/known_hosts"
        if ssh -F /dev/null -i "$state/id_ed25519" -p "$port" \
          -o BatchMode=yes -o IdentitiesOnly=yes -o ConnectTimeout=2 \
          -o StrictHostKeyChecking=yes -o "UserKnownHostsFile=$state/known_hosts" \
          tester@127.0.0.1 true 2>/dev/null; then
          ready=true
          break
        fi
      fi
      sleep 0.25
    done
    if [[ "$ready" != true ]]; then
      "$docker" logs "$container" >&2
      printf '%s\n' 'SSH did not become ready. The fixture is retained for diagnosis; use down to remove it.' >&2
      exit 1
    fi
    printf 'Isolated SSH fixture: tester@127.0.0.1 port %s\n' "$port"
    ;;
  test)
    python3 "$here/test_tmux_baseline.py" \
      --port "$(cat "$state/port")" --identity "$state/id_ed25519" \
      --known-hosts "$state/known_hosts" --report "$state/baseline-results.json"
    ;;
  down)
    if [[ ! -f "$state/container" ]]; then
      printf '%s\n' 'No fixture is recorded.'
      exit 0
    fi
    container=$(cat "$state/container")
    id=$(cat "$state/id")
    owner=$("$docker" inspect --format '{{index .Config.Labels "com.eternalwarp.lab.id"}}' "$container")
    if [[ "$owner" != "$id" || "$container" != "eternalwarp-lab-$id" ]]; then
      printf '%s\n' 'Refusing to remove a container not owned by this lab.' >&2
      exit 1
    fi
    "$docker" rm --force "$container"
    rm "$state/container" "$state/id" "$state/port" "$state/known_hosts"
    ;;
  *)
    printf 'Usage: bash %s {up|test|down}\n' "$0" >&2
    exit 2
    ;;
esac

