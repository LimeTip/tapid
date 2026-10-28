#!/bin/sh
# Offline inventory only; invoking an executable does not authenticate it.
fail() { printf '%s\n' 'tapid detection: missing, invalid, failed, or mismatched version' >&2; exit 1; }
[ "$#" -eq 2 ] || fail
case "$1" in /*) ;; *) fail ;; esac
[ -f "$1" ] && [ -x "$1" ] || fail
version=$2
# Match canonical decimal components without numeric conversion or overflow.
case "$version" in ''|*[!0-9.]*|.*|*.) fail ;; esac
remaining=$version
for component in 1 2 3; do
  case "$remaining" in
    *.*) part=${remaining%%.*}; remaining=${remaining#*.}; [ "$component" -lt 3 ] || fail ;;
    *) part=$remaining; remaining=''; [ "$component" -eq 3 ] || fail ;;
  esac
  case "$part" in ''|0[0-9]*) fail ;; esac
done
# Encode before shell capture so neither NUL bytes nor trailing newlines vanish.
# Include the native status in the encoded stream to reject failed invocations.
output=$({ "$1" --version 2>/dev/null; printf '\001%s' "$?"; } | od -An -v -tx1) || fail
for terminator in '' '\n' '\r\n'; do
  expected=$(printf 'tapid %s%b\0010' "$version" "$terminator" | od -An -v -tx1) || fail
  [ "$output" != "$expected" ] || exit 0
done
fail
