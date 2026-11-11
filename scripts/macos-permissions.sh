#!/usr/bin/env bash
set -euo pipefail
if [ "$(uname -s)" != Darwin ]; then
  echo 'This command requires macOS.' >&2
  exit 1
fi
repo_dir="$(cd "$(dirname "$0")/.." && pwd)"
cd "$repo_dir"
bundle="$repo_dir/target/Remote Web Desktop.app"
bash scripts/package-macos.sh stop
# Preserve the existing signature when the packaged binary is already current.
if [ ! -x "$bundle/Contents/MacOS/RemoteWebDesktopLauncher" ] || \
   ! cmp -s target/release/remote-web-desktop "$bundle/Contents/MacOS/remote-web-desktop"; then
  bash scripts/package-macos.sh package
fi
bash scripts/package-macos.sh start --request-permissions "$@"
open -R "$bundle"
# The server requests missing permissions and opens the relevant settings pane.
echo 'Allow Remote Web Desktop in Screen Recording and Accessibility.'
echo 'If an old entry is enabled but access still fails, select this exact app with the + button.'
echo 'Then run make macos-restart with the same HOST, PORT and RWD_PASSWORD.'
