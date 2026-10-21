#!/bin/sh
# Proves check-secrets.sh catches each kind of leak, including 4-letter project prefixes.
set -eu
script="$(cd "$(dirname "$0")" && pwd)/check-secrets.sh"
dir=$(mktemp -d)
trap 'rm -rf "$dir"' EXIT
fail=0
expect_found() { # <name> <content>
  printf '%s\n' "$2" > "$dir/f.txt"
  if "$script" dir "$dir" >/dev/null 2>&1; then echo "NOT detected: $1"; fail=1; fi
}
expect_clean() {
  printf '%s\n' "$2" > "$dir/f.txt"
  "$script" dir "$dir" >/dev/null 2>&1 || { echo "false positive: $1"; fail=1; }
}
expect_found "key" "k=uk_live_abcdef123456"
expect_found "ip" "ip=10.20.30.40"
expect_found "project, 4-letter prefix" "p=taba-123456"
expect_found "project, long" "p=meu-outro-projeto-654321"
expect_found "service account" "x@proj-a.iam.gserviceaccount.com"
expect_clean "placeholders" "meu-projeto-123456 127.0.0.1 uk_live_..."
exit "$fail"
