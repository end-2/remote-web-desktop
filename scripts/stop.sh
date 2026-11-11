#!/usr/bin/env bash
# Match executable paths, not ports or command-line substrings, so other
# checkouts and unrelated services are left alone.
set -euo pipefail

repo_dir="$(cd "$(dirname "$0")/.." && pwd -P)"
platform="$(uname -s)"
user_id="$(id -u)"
executables=(
  "$repo_dir/target/release/remote-web-desktop"
  "$repo_dir/target/debug/remote-web-desktop"
  "$repo_dir/target/Remote Web Desktop.app/Contents/MacOS/remote-web-desktop"
  "$repo_dir/target/Remote Web Desktop.app/Contents/MacOS/RemoteWebDesktopLauncher"
)

is_server() {
  local executable="${1% (deleted)}" expected
  for expected in "${executables[@]}"; do
    if [ "$executable" = "$expected" ]; then return 0; fi
  done
  return 1
}

running_pids() {
  local record pid='' entry executable
  case "$platform" in
    Darwin)
      # lsof resolves relative launches such as ./target/release/... and
      # reports mapped executable paths even after a rebuild replaces them.
      while IFS= read -r record; do
        case "$record" in
          p*) pid="${record#p}" ;;
          n*) if is_server "${record#n}"; then printf '%s\n' "$pid"; fi ;;
        esac
      done < <(/usr/sbin/lsof -nP -a -u "$user_id" -c remote-web -c RemoteWeb -d txt -Fpn 2>/dev/null || true)
      ;;
    Linux)
      for entry in /proc/[0-9]*/exe; do
        [ -O "${entry%/exe}" ] || continue
        executable="$(readlink "$entry" 2>/dev/null)" || continue
        if is_server "$executable"; then
          pid="${entry#/proc/}"
          printf '%s\n' "${pid%/exe}"
        fi
      done
      ;;
  esac
}

case "$platform" in
  Darwin | Linux) ;;
  *) echo 'Use scripts/stop.ps1 on Windows; stop.sh supports macOS and Linux.' >&2; exit 1 ;;
esac

pids="$(running_pids | sort -u)"
if [ -z "$pids" ]; then
  echo 'Not running.'
  exit 0
fi
while read -r pid; do
  if ! kill -TERM "$pid" 2>/dev/null && kill -0 "$pid" 2>/dev/null; then
    printf 'Could not stop server PID %s.\n' "$pid" >&2
    exit 1
  fi
done <<< "$pids"

# SIGTERM follows the server's shutdown path and releases held input events.
for ((attempt = 0; attempt < 50; attempt++)); do
  if [ -z "$(running_pids)" ]; then
    echo 'Stopped; held inputs released.'
    exit 0
  fi
  sleep 0.1
done
echo 'Graceful termination requested; a server is still running. No forced kill was sent.' >&2
exit 1
