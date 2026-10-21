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
# Fixtures are assembled at run time so this file itself passes the repo scan.
live=uk_live_; uip=uip_ak.; uuid=0a1b2c3d-0000-4000-8000-000000000000; ip=10.20.30; sa=.iam.gserviceaccount; num=123456
expect_found "key" "k=${live}abcdef123456"
expect_found "console key" "k=${uip}prod.${uuid}.${uuid}.secretvalue"
expect_found "ip" "ip=${ip}.40"
expect_found "project, 4-letter prefix" "p=taba-${num}"
expect_found "project, long" "p=meu-outro-projeto-${num}"
expect_found "service account" "x@proj-a${sa}.com"
expect_clean "placeholders" "meu-projeto-${num} 127.0.0.1 ${live}... ${uip}prod.xxxxxxxx"
exit "$fail"
