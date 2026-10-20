#!/usr/bin/env bash
# Run a command inside a throwaway headless GNOME Shell (Wayland + Xwayland),
# then kill the compositor and clean up its sockets. For automated testing
# only (screenshots over SSH, visual tests); on a desktop run the app directly.
#
# Usage: tools/headless-run.sh [--x11] [--size WxH] -- <command...>
#   FW_HEADLESS_SCALE=1.5  set the virtual monitor's (fractional) scale via gdctl
#   --x11   run the command against Xwayland (DISPLAY) instead of Wayland
set -u
MODE=wayland; SIZE=1920x1080
while [[ $# -gt 0 ]]; do
  case "$1" in
    --x11) MODE=x11; shift;;
    --size) SIZE="$2"; shift 2;;
    --) shift; break;;
    *) break;;
  esac
done
NAME="fw-test-$$"
HAD_GUARD=""
[[ -e "/run/user/$(id -u)/gnome-shell-disable-extensions" ]] && HAD_GUARD=1
LOG="${FW_HEADLESS_LOG:-${TMPDIR:-/tmp}/fw-headless-$$.log}"
mkdir -p "$(dirname "$LOG")"
# Own D-Bus session so the throwaway shell can never clash with (or replace
# names owned by) a real GNOME session running for the same user.
dbus-run-session -- bash -c 'echo "DBUS=$DBUS_SESSION_BUS_ADDRESS"; exec gnome-shell --headless --wayland --no-x11 --wayland-display="$0" --virtual-monitor "$1"' "$NAME" "$SIZE" >"$LOG" 2>&1 &
GS=$!
cleanup() {
  pkill -KILL -f "^gnome-shell --headless --wayland --no-x11 --wayland-display=$NAME " 2>/dev/null
  kill -KILL "$GS" 2>/dev/null
  wait "$GS" 2>/dev/null
  # D-Bus activation on the private bus spawns portals, gvfs, goa, etc. that
  # outlive the shell: kill every process still attached to that bus.
  # Match on the bus address from the log, and also on WAYLAND_DISPLAY=$NAME
  # (activated services inherit it), so cleanup still works if the log could
  # not be written (e.g. /tmp quota exhausted).
  local bus; bus=$(grep -o "^DBUS=.*" "$LOG" 2>/dev/null | head -1 | cut -d= -f2- | cut -d, -f1)
  local pat="^WAYLAND_DISPLAY=$NAME\$"
  [[ -n "$bus" ]] && pat="$pat|^DBUS_SESSION_BUS_ADDRESS=$bus"
  for sig in TERM KILL; do
    for p in $(ps -u "$(id -u)" -o pid=); do
      [[ "$p" == "$$" ]] && continue
      { tr '\0' '\n' < "/proc/$p/environ"; } 2>/dev/null | grep -qE "$pat" && kill -$sig "$p" 2>/dev/null
    done
    sleep 0.5
  done
  rm -f "/run/user/$(id -u)/$NAME" "/run/user/$(id -u)/$NAME.lock"
  # gnome-shell's crash-loop guard: it creates this at startup and deletes it
  # after running a while. Killing it early leaves the file behind, which would
  # make the *real* next GNOME login start with extensions disabled.
  if [[ -z "$HAD_GUARD" ]]; then rm -f "/run/user/$(id -u)/gnome-shell-disable-extensions"; fi
}
trap cleanup EXIT
for _ in $(seq 1 50); do [[ -S "/run/user/$(id -u)/$NAME" ]] && break; sleep 0.2; done
# gnome-shell accepts connections only once fully started (~5 s).
for _ in $(seq 1 60); do grep -q "GNOME Shell started" "$LOG" && break; sleep 0.25; done
sleep 0.5
# Expose the private bus so callers can drive mutter (e.g. gdctl for scaling).
export FW_HEADLESS_DBUS=$(grep -o "^DBUS=.*" "$LOG" | head -1 | cut -d= -f2-)
if [[ -n "${FW_HEADLESS_SCALE:-}" ]]; then
  DBUS_SESSION_BUS_ADDRESS="$FW_HEADLESS_DBUS" gdctl set --logical-monitor --primary --monitor Meta-0 --scale "$FW_HEADLESS_SCALE" >>"$LOG" 2>&1 || echo "gdctl scale failed (see $LOG)" >&2
  sleep 0.5
fi
if [[ "$MODE" == x11 ]]; then
  # gnome-shell's own on-demand Xwayland fails to start in this sandboxed
  # environment, so run a rootful Xwayland as a Wayland client instead.
  XN=$(( ($$ % 50) + 40 ))
  WAYLAND_DISPLAY="$NAME" Xwayland ":$XN" -geometry "$SIZE" -noreset ${FW_XWAYLAND_ARGS:-} >>"$LOG" 2>&1 &
  XW=$!
  trap 'kill -KILL $XW 2>/dev/null; wait $XW 2>/dev/null; rm -f /tmp/.X$XN-lock /tmp/.X11-unix/X$XN; cleanup' EXIT
  for _ in $(seq 1 40); do [[ -S "/tmp/.X11-unix/X$XN" ]] && break; sleep 0.25; done
  env -u WAYLAND_DISPLAY DISPLAY=":$XN" XDG_SESSION_TYPE=x11 "$@"
else
  env -u DISPLAY WAYLAND_DISPLAY="$NAME" XDG_SESSION_TYPE=wayland "$@"
fi
RC=$?
exit $RC
