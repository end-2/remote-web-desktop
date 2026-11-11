#!/usr/bin/env bash
# Native build dependencies for macOS and supported Linux distributions.
set -euo pipefail

as_root() {
  if [ "$(id -u)" -eq 0 ]; then "$@"; else sudo "$@"; fi
}

case "$(uname -s)" in
  Darwin)
    if ! xcode-select -p >/dev/null 2>&1; then
      xcode-select --install
      echo 'Complete the Command Line Tools installer, then run make setup again.' >&2
      exit 1
    fi
    xcrun clang --version >/dev/null
    xcrun swift --version
    if ! command -v brew >/dev/null 2>&1; then
      if [ -x /opt/homebrew/bin/brew ]; then
        eval "$(/opt/homebrew/bin/brew shellenv)"
      elif [ -x /usr/local/bin/brew ]; then
        eval "$(/usr/local/bin/brew shellenv)"
      else
        installer=$(mktemp)
        curl --proto '=https' --tlsv1.2 -fsSL https://raw.githubusercontent.com/Homebrew/install/HEAD/install.sh -o "$installer"
        /bin/bash "$installer"
        rm -f "$installer"
        if [ -x /opt/homebrew/bin/brew ]; then
          eval "$(/opt/homebrew/bin/brew shellenv)"
        else
          eval "$(/usr/local/bin/brew shellenv)"
        fi
      fi
    fi
    brew install pkg-config nasm
    ;;
  Linux)
    if command -v apt-get >/dev/null 2>&1; then
      as_root apt-get update
      as_root apt-get install -y build-essential curl ca-certificates pkg-config clang libclang-dev nasm \
        libxcb1-dev libxrandr-dev libdbus-1-dev libxkbcommon-dev \
        libpipewire-0.3-dev libwayland-dev libegl-dev
    elif command -v dnf >/dev/null 2>&1; then
      as_root dnf install -y gcc gcc-c++ make curl ca-certificates pkgconf-pkg-config clang clang-devel nasm \
        libxcb-devel libXrandr-devel dbus-devel libxkbcommon-devel \
        pipewire-devel wayland-devel mesa-libEGL-devel
    elif command -v pacman >/dev/null 2>&1; then
      as_root pacman -S --needed --noconfirm base-devel curl ca-certificates pkgconf clang nasm \
        libxcb libxrandr dbus libxkbcommon pipewire wayland mesa
    else
      echo 'Supported Linux package managers: apt-get, dnf, pacman. Install native dependencies manually on other distributions.' >&2
      exit 1
    fi
    ;;
  *)
    echo 'On Windows, run: powershell -ExecutionPolicy Bypass -File scripts/setup.ps1' >&2
    exit 1
    ;;
esac

export PATH="${CARGO_HOME:-$HOME/.cargo}/bin:$PATH"
if ! command -v rustup >/dev/null 2>&1; then
  installer=$(mktemp)
  curl --proto '=https' --tlsv1.2 -fsSL https://sh.rustup.rs -o "$installer"
  sh "$installer" -y --profile minimal --default-toolchain stable
  rm -f "$installer"
fi
rustup toolchain install stable --profile minimal
cargo +stable fetch --locked
echo 'Build dependencies ready. Run make demo or make run.'
