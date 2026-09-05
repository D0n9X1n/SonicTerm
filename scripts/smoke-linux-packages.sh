#!/usr/bin/env bash
# Exercise portable and Debian packages on X11 and Wayland with lavapipe.
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"

usage() {
  printf 'usage: %s <tar.gz> <deb>\n' "$0" >&2
  exit 2
}

[[ $# -eq 2 ]] || usage
tarball="$1"
deb="$2"
[[ -f "$tarball" ]] || { printf 'tarball not found: %s\n' "$tarball" >&2; exit 1; }
[[ -f "$deb" ]] || { printf 'Debian package not found: %s\n' "$deb" >&2; exit 1; }
[[ "$(id -u)" -eq 0 ]] || {
  printf 'Linux package smoke requires root inside an ephemeral CI container\n' >&2
  exit 1
}

for command in dpkg dpkg-query python3 tar Xvfb xdpyinfo weston; do
  command -v "$command" >/dev/null 2>&1 || {
    printf 'required smoke tool is missing: %s\n' "$command" >&2
    exit 1
  }
done
if dpkg-query -W -f='${Status}' sonicterm 2>/dev/null | grep -Fq 'install ok installed'; then
  printf 'refusing to replace an existing sonicterm Debian installation\n' >&2
  exit 1
fi

lvp_icd="${VK_ICD_FILENAMES:-/usr/share/vulkan/icd.d/lvp_icd.x86_64.json}"
[[ -f "$lvp_icd" ]] || {
  printf 'lavapipe ICD not found: %s\n' "$lvp_icd" >&2
  exit 1
}

work="$(mktemp -d "${TMPDIR:-/tmp}/sonicterm-linux-smoke.XXXXXX")"
display_pid=""
installed=false
cleanup() {
  local status=$?
  if [[ -n "$display_pid" ]]; then
    kill "$display_pid" 2>/dev/null || true
    wait "$display_pid" 2>/dev/null || true
  fi
  if [[ $status -ne 0 ]]; then
    local destination="${GITHUB_WORKSPACE:-$work}"
    for log in "$work"/*.log; do
      [[ -f "$log" ]] || continue
      cp "$log" "$destination/sonicterm-$(basename "$log")" 2>/dev/null || true
    done
  fi
  if "$installed"; then
    dpkg --purge sonicterm >/dev/null 2>&1 || true
  fi
  rm -rf "$work"
  return "$status"
}
trap cleanup EXIT INT TERM

tar -xzf "$tarball" -C "$work"
mapfile -t portable_roots < <(find "$work" -mindepth 1 -maxdepth 1 -type d -name 'SonicTerm-*-linux-x86_64' -print)
[[ ${#portable_roots[@]} -eq 1 ]] || {
  printf 'tarball must contain one SonicTerm Linux payload directory\n' >&2
  exit 1
}
portable_binary="${portable_roots[0]}/sonicterm"
[[ -x "$portable_binary" ]] || { printf 'portable binary is not executable\n' >&2; exit 1; }
[[ -d "${portable_roots[0]}/assets" ]] || { printf 'portable assets are missing\n' >&2; exit 1; }

dpkg --install "$deb"
installed=true
[[ -x /usr/bin/sonicterm ]] || { printf 'installed Debian binary is missing\n' >&2; exit 1; }
[[ -d /usr/share/sonicterm/assets ]] || { printf 'installed Debian assets are missing\n' >&2; exit 1; }

export LIBGL_ALWAYS_SOFTWARE=1
export MESA_LOADER_DRIVER_OVERRIDE=llvmpipe
export VK_ICD_FILENAMES="$lvp_icd"
export WGPU_BACKEND=vulkan

run_smoke() {
  local display_kind="$1"
  local package_kind="$2"
  local binary="$3"
  local state_dir="$work/state-$display_kind-$package_kind"
  local log="$work/$display_kind-$package_kind.log"
  mkdir -p "$state_dir"

  set +e
  python3 "$ROOT/scripts/native-smoke-runner.py" \
    --timeout-seconds 45 \
    --state-dir "$state_dir" \
    --log-file "$log" \
    -- "$binary" --runtime-smoke
  local status=$?
  set -e
  if [[ $status -ne 0 ]]; then
    printf '%s %s smoke failed with code %s\n' "$display_kind" "$package_kind" "$status" >&2
    cp "$log" "${GITHUB_WORKSPACE:-$work}/sonicterm-$display_kind-$package_kind-smoke.log" 2>/dev/null || true
    sed -n '1,240p' "$log" >&2
    exit "$status"
  fi
  printf '%s %s smoke passed\n' "$display_kind" "$package_kind"
}

start_x11() {
  export DISPLAY=:99
  unset WAYLAND_DISPLAY WAYLAND_SOCKET
  Xvfb "$DISPLAY" -screen 0 1280x800x24 -nolisten tcp >"$work/xvfb.log" 2>&1 &
  display_pid=$!
  for _ in {1..100}; do
    if xdpyinfo -display "$DISPLAY" >/dev/null 2>&1; then
      return
    fi
    if ! kill -0 "$display_pid" 2>/dev/null; then
      cat "$work/xvfb.log" >&2
      exit 1
    fi
    sleep 0.1
  done
  printf 'Xvfb did not become ready\n' >&2
  cat "$work/xvfb.log" >&2
  exit 1
}

start_wayland() {
  unset DISPLAY WAYLAND_SOCKET
  export XDG_RUNTIME_DIR="$work/wayland-runtime"
  export WAYLAND_DISPLAY=sonicterm-wayland
  mkdir -p "$XDG_RUNTIME_DIR"
  chmod 0700 "$XDG_RUNTIME_DIR"
  weston --backend=headless-backend.so --socket="$WAYLAND_DISPLAY" --idle-time=0 \
    --width=1280 --height=800 >"$work/weston.log" 2>&1 &
  display_pid=$!
  for _ in {1..100}; do
    if [[ -S "$XDG_RUNTIME_DIR/$WAYLAND_DISPLAY" ]]; then
      return
    fi
    if ! kill -0 "$display_pid" 2>/dev/null; then
      cat "$work/weston.log" >&2
      exit 1
    fi
    sleep 0.1
  done
  printf 'Weston did not become ready\n' >&2
  cat "$work/weston.log" >&2
  exit 1
}

stop_display() {
  kill "$display_pid" 2>/dev/null || true
  wait "$display_pid" 2>/dev/null || true
  display_pid=""
}

start_x11
run_smoke x11 tar "$portable_binary"
run_smoke x11 deb /usr/bin/sonicterm
stop_display

start_wayland
run_smoke wayland tar "$portable_binary"
run_smoke wayland deb /usr/bin/sonicterm
stop_display
