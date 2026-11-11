#!/bin/bash
# Packages the existing Rust release binary; does not run cargo or install a service.
# Native trampoline follows Apple's supported bundle/main-executable guidance:
# https://developer.apple.com/forums/thread/678819
# https://developer.apple.com/forums/thread/720057?answerId=736527022
set -euo pipefail

repo_dir="$(cd "$(dirname "$0")/.." && pwd)"
bundle="$repo_dir/target/Remote Web Desktop.app"
bundle_id="local.remote-web-desktop.server"
server="$bundle/Contents/MacOS/remote-web-desktop"
launcher="$bundle/Contents/MacOS/RemoteWebDesktopLauncher"
log_path="$HOME/Library/Logs/Remote Web Desktop/server.log"
command_name="${1:-package}"
if [ "$#" -gt 0 ]; then shift; fi

if [ "$(uname -s)" != Darwin ]; then
  echo 'This packaging helper requires macOS.' >&2
  exit 1
fi

running_pids() {
  /bin/ps -axo pid=,comm= | while read -r pid executable; do
    if [ "$executable" = "$server" ] || [ "$executable" = "$launcher" ]; then
      printf '%s\n' "$pid"
    fi
  done
}

case "$command_name" in
  status)
    pids="$(running_pids)"
    if [ -n "$pids" ]; then printf 'Running PID(s): %s\n' "$pids";
    else echo 'Not running.'; fi
    printf 'Private startup/password log: %s\n' "$log_path"
    exit 0
    ;;
  log-path)
    printf '%s\n' "$log_path"
    exit 0
    ;;
  stop)
    pids="$(running_pids)"
    if [ -z "$pids" ]; then echo 'Not running.'; exit 0; fi
    while read -r pid; do /bin/kill -TERM "$pid"; done <<< "$pids"
    for ((attempt = 0; attempt < 50; attempt++)); do
      if [ -z "$(running_pids)" ]; then echo 'Stopped; held inputs released.'; exit 0; fi
      sleep 0.1
    done
    echo 'Graceful termination requested; process is still shutting down.' >&2
    exit 1
    ;;
  start)
    if [ ! -x "$launcher" ]; then
      echo 'Run scripts/package-macos.sh first.' >&2
      exit 1
    fi
    if [ "${RWD_PASSWORD+x}" = x ]; then
      /usr/bin/open --env "RWD_PASSWORD=$RWD_PASSWORD" "$bundle" --args "$@"
    else
      /usr/bin/open "$bundle" --args "$@"
    fi
    printf 'Opened: %s\nPrivate startup/password log: %s\n' "$bundle" "$log_path"
    exit 0
    ;;
  package)
    ;;
  *)
    echo 'Usage: scripts/package-macos.sh [package | start [server arguments...] | stop | status | log-path]' >&2
    exit 2
    ;;
esac

if [ "$#" -ne 0 ]; then
  echo 'The package command takes no arguments.' >&2
  exit 2
fi
if [ -n "$(running_pids)" ]; then
  echo 'Stop the packaged server before replacing its bundle.' >&2
  exit 1
fi
release_binary="$repo_dir/target/release/remote-web-desktop"
if [ ! -x "$release_binary" ]; then
  echo 'Build the release binary first: cargo build --release' >&2
  exit 1
fi

stage="$(mktemp -d "$repo_dir/target/macos-package.XXXXXX")"
trap 'rm -rf "$stage"' EXIT
staged_bundle="$stage/Remote Web Desktop.app"
mkdir -p "$staged_bundle/Contents/MacOS"
cp "$release_binary" "$staged_bundle/Contents/MacOS/remote-web-desktop"

# A shell script as CFBundleExecutable can break TCC attribution. This small
# Mach-O launcher redirects output before exec() of the unchanged Rust program.
cat > "$stage/launcher.c" <<'C'
#include <errno.h>
#include <fcntl.h>
#include <limits.h>
#include <mach-o/dyld.h>
#include <pwd.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <sys/file.h>
#include <sys/stat.h>
#include <time.h>
#include <unistd.h>

static void fail(const char *message) {
    perror(message);
    exit(EXIT_FAILURE);
}

static void ensure_directory(const char *path, int private_directory) {
    if (mkdir(path, 0700) != 0 && errno != EEXIST) fail(path);
    struct stat details;
    if (lstat(path, &details) != 0) fail(path);
    if (!S_ISDIR(details.st_mode) || details.st_uid != getuid()) {
        errno = EPERM;
        fail("Invalid log directory");
    }
    if (private_directory && chmod(path, 0700) != 0) fail("chmod logs");
}

int main(int argc, char **argv) {
    (void)argc;
    umask(0077);
    struct passwd *account = getpwuid(getuid());
    if (account == NULL) fail("getpwuid");
    char path[PATH_MAX];
    if (snprintf(path, sizeof(path), "%s/Library/Logs", account->pw_dir) >= (int)sizeof(path)) {
        errno = ENAMETOOLONG;
        fail("Log directory");
    }
    ensure_directory(path, 0);
    if (strlcat(path, "/Remote Web Desktop", sizeof(path)) >= sizeof(path)) {
        errno = ENAMETOOLONG;
        fail("Log directory");
    }
    ensure_directory(path, 1);
    if (strlcat(path, "/server.log", sizeof(path)) >= sizeof(path)) {
        errno = ENAMETOOLONG;
        fail("Log path");
    }
    int log_fd = open(path, O_CREAT | O_WRONLY | O_NOFOLLOW, 0600);
    if (log_fd < 0) fail("Open private log");
    struct stat log_details;
    if (fstat(log_fd, &log_details) != 0) fail("fstat log");
    if (!S_ISREG(log_details.st_mode) || log_details.st_uid != getuid()) {
        errno = EPERM;
        fail("Invalid private log");
    }
    // The lock remains held through dup2/exec and prevents a second launch from
    // replacing the active session's random password log.
    if (flock(log_fd, LOCK_EX | LOCK_NB) != 0) {
        fprintf(stderr, "Remote Web Desktop is already running.\n");
        return EXIT_FAILURE;
    }
    if (fchmod(log_fd, 0600) != 0 || ftruncate(log_fd, 0) != 0) fail("Prepare private log");
    if (dup2(log_fd, STDOUT_FILENO) < 0 || dup2(log_fd, STDERR_FILENO) < 0) fail("Redirect log");
    if (log_fd > STDERR_FILENO) close(log_fd);
    time_t now = time(NULL);
    fprintf(stderr, "Remote Web Desktop starting: %s", ctime(&now));
    fprintf(stderr, "Log contains the current session password. Permissions: owner only.\n");
    uint32_t path_size = sizeof(path);
    if (_NSGetExecutablePath(path, &path_size) != 0) fail("Executable path");
    char *last_slash = strrchr(path, '/');
    if (last_slash == NULL) fail("Executable directory");
    last_slash[1] = '\0';
    if (strlcat(path, "remote-web-desktop", sizeof(path)) >= sizeof(path)) {
        errno = ENAMETOOLONG;
        fail("Server path");
    }
    argv[0] = path;
    // Bound each native capture start/stop operation; the crate otherwise
    // waits 30 seconds on OS completion. Preserve an explicit operator value.
    if (setenv("SCREENCAPTUREKIT_COMPLETION_TIMEOUT_SECS", "5", 0) != 0) {
        fail("Set capture completion timeout");
    }
    execv(path, argv);
    fail("Launch Rust server");
}
C

/usr/bin/xcrun clang -Os -Wall -Wextra -Werror -o "$staged_bundle/Contents/MacOS/RemoteWebDesktopLauncher" "$stage/launcher.c"
cat > "$staged_bundle/Contents/Info.plist" <<'PLIST'
<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0"><dict>
  <key>CFBundleIdentifier</key><string>local.remote-web-desktop.server</string>
  <key>CFBundleName</key><string>Remote Web Desktop</string>
  <key>CFBundleDisplayName</key><string>Remote Web Desktop</string>
  <key>CFBundleExecutable</key><string>RemoteWebDesktopLauncher</string>
  <key>CFBundlePackageType</key><string>APPL</string>
  <key>CFBundleVersion</key><string>1</string>
  <key>CFBundleShortVersionString</key><string>0.1.0</string>
  <key>LSMinimumSystemVersion</key><string>12.3</string>
  <key>LSUIElement</key><true/>
  <key>LSMultipleInstancesProhibited</key><true/>
</dict></plist>
PLIST

/usr/bin/plutil -lint "$staged_bundle/Contents/Info.plist"
/usr/bin/codesign --force --sign - --identifier "$bundle_id.worker" "$staged_bundle/Contents/MacOS/remote-web-desktop"
/usr/bin/codesign --force --sign - --identifier "$bundle_id" "$staged_bundle"
/usr/bin/codesign --verify --deep --strict "$staged_bundle"

if [ -d "$bundle" ]; then
  # Preserve the previous generated artifact until the replacement is installed.
  mv "$bundle" "$stage/previous.app"
fi
mv "$staged_bundle" "$bundle"
printf 'Packaged existing release binary: %s\n' "$bundle"
printf 'Launch with Finder or: scripts/package-macos.sh start\n'
printf 'Private startup/password log: %s\n' "$log_path"
printf 'Stop gracefully: scripts/package-macos.sh stop\n'
printf 'Grant permissions to Remote Web Desktop.app; ad-hoc rebuilds may need a fresh grant.\n'
