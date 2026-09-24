#!/usr/bin/env bash
set -euo pipefail
[[ "$(uname -s)" == Linux ]]
sudo apt-get update
sudo env DEBIAN_FRONTEND=noninteractive apt-get install -y \
  build-essential clang cmake pkg-config protobuf-compiler libssl-dev \
  libfontconfig1-dev libfreetype-dev libdbus-1-dev libegl1-mesa-dev \
  libsqlite3-dev libwayland-dev libx11-dev libxcb1-dev libxkbcommon-dev \
  libxkbcommon-x11-dev git curl ca-certificates tmux fish zsh python3
