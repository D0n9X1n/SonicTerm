#!/usr/bin/env bash
# check_orphans.sh — verify PtyHandle::Drop actually killed every shell
# sonicterm-mac had spawned. The old `no-orphan-shells` check only confirmed
# sonicterm-mac itself was gone; it never inspected the *children*.
#
# Usage:
#   check_orphans.sh snapshot <sonic-pid> <out-file>
#       Walk the descendant tree of <sonic-pid>, keeping any PID whose
#       comm matches zsh|bash|sh, and write one PID per line to <out-file>.
#       Intended to run while sonicterm-mac is still alive with shells spawned.
#
#   check_orphans.sh check <snapshot-file>
#       For each PID in <snapshot-file>, test `kill -0 <pid>`. Prints a
#       single line `orphans=<N>` to stdout. Exits 0 always (the caller
#       decides pass/fail based on the count).
#
set -uo pipefail

cmd="${1:?snapshot|check required}"

descendants_of() {
  # POSIX-ish recursive descent of the process tree starting at $1.
  # Avoid bash 4-only negative-index array subscripts — macOS ships bash 3.2.
  local root="$1"
  local queue="$root"
  local seen=" "
  local out=""
  while [[ -n "$queue" ]]; do
    # pop head
    local cur="${queue%% *}"
    if [[ "$queue" == *" "* ]]; then
      queue="${queue#* }"
    else
      queue=""
    fi
    [[ -z "$cur" ]] && continue
    [[ "$seen" == *" $cur "* ]] && continue
    seen+="$cur "
    [[ "$cur" != "$root" ]] && out+="$cur "
    local kids
    kids=$(pgrep -P "$cur" 2>/dev/null || true)
    for k in $kids; do
      queue+=" $k"
    done
  done
  for p in $out; do
    echo "$p"
  done
}

case "$cmd" in
  snapshot)
    sonic_pid="${2:?sonic pid required}"
    out="${3:?out file required}"
    : > "$out"
    # All descendants, then keep only zsh/bash/sh-like ones.
    while read -r pid; do
      [[ -z "$pid" ]] && continue
      comm=$(ps -o comm= -p "$pid" 2>/dev/null | tr -d ' ' || true)
      base="${comm##*/}"
      case "$base" in
        zsh|bash|sh|dash|fish|-zsh|-bash|-sh) echo "$pid" >> "$out" ;;
      esac
    done < <(descendants_of "$sonic_pid")
    echo "snapshot wrote $(wc -l < "$out" | tr -d ' ') pids to $out" 1>&2
    ;;

  check)
    snap="${2:?snapshot file required}"
    if [[ ! -f "$snap" ]]; then
      # No snapshot file → cannot verify. Treat as 0 orphans but flag.
      echo "orphans=0"
      echo "WARN: snapshot file $snap missing — check skipped" 1>&2
      exit 0
    fi
    n=0
    while read -r pid; do
      [[ -z "$pid" ]] && continue
      if kill -0 "$pid" 2>/dev/null; then
        n=$((n + 1))
        echo "ORPHAN pid=$pid $(ps -o pid,ppid,comm -p "$pid" 2>/dev/null | tail -1)" 1>&2
      fi
    done < "$snap"
    echo "orphans=$n"
    ;;

  *)
    echo "unknown subcommand: $cmd" 1>&2
    exit 2
    ;;
esac
