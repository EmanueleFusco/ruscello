#!/usr/bin/env bash
# System packages needed to build Ruscello on Ubuntu CI runners.
# Extend this list when GUI/webview/tray crates are introduced (see AGENTS.md §2).
set -euo pipefail

PKGS=(
  pkg-config
  # iced / winit
  libxkbcommon-dev
  libwayland-dev
  # wry / tao (ruscello-webview) — needed from M6
  libwebkit2gtk-4.1-dev
  libsoup-3.0-dev
  libgtk-3-dev
  # tray-icon / global hotkeys
  libxdo-dev
  libayatana-appindicator3-dev
  # notifications / D-Bus
  libdbus-1-dev
)

sudo apt-get update -qq
sudo apt-get install -y --no-install-recommends "${PKGS[@]}"
