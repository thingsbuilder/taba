#!/bin/sh
# Criterion 61: no secret, GCP project id or IP in the repository or in the compiled PWA.
#   scripts/check-secrets.sh repo        scans the files tracked by git
#   scripts/check-secrets.sh dir <path>  scans a directory (CI uses pwa/dist)
# Matches are reported as file:line only, never printed: CI logs of a public repo are public.
set -eu

mode=${1:-repo}
case "$mode" in
  repo) cd "$(git rev-parse --show-toplevel)" ;;
  dir) [ -d "${2:-}" ] || { echo "usage: $0 repo | dir <path>" >&2; exit 2; } ;;
  *) echo "usage: $0 repo | dir <path>" >&2; exit 2 ;;
esac

scan() { # scan <label> <regex> <allowed-match-regex>
  if [ "$mode" = repo ]; then
    hits=$(git ls-files -z | xargs -0 grep -nHoIE "$2" 2>/dev/null || true)
  else
    hits=$(grep -rnHoIE "$2" "$3" 2>/dev/null || true)
  fi
  # drop lock files and the documented placeholders (the match is the last field)
  hits=$(printf '%s\n' "$hits" | grep -vE '(^|/)(package-lock\.json|Cargo\.lock):' | grep -vE ":($ALLOWED)\$" || true)
  if [ -n "$hits" ]; then
    echo "FOUND $1:"
    printf '%s\n' "$hits" | cut -d: -f1,2 | sed 's/^/  /'
    failed=1
  fi
}

ALLOWED='127\.0\.0\.1|0\.0\.0\.0|meu-projeto-123456'
failed=0
target=${2:-.}

scan "Unico access key (uk_live_...)" 'uk_live_[A-Za-z0-9]{8,}' "$target"
scan "IPv4 address" '\b([0-9]{1,3}\.){3}[0-9]{1,3}\b' "$target"
scan "GCP project id (name-NNNNNN)" '\b[a-z][a-z0-9-]{3,28}-[0-9]{6}\b' "$target"
scan "GCP service account email" '@[a-z][a-z0-9-]+\.iam\.gserviceaccount\.com' "$target"

# On the maintainer's machine, also look for the real values of this installation.
prod=infra/ansible/inventory/prod/group_vars/all/vars.yml
if [ "$mode" = repo ] && [ -f "$prod" ]; then
  project=$(sed -n 's/^gcp_project:[[:space:]]*"\{0,1\}\([^"]*\)"\{0,1\}[[:space:]]*$/\1/p' "$prod")
  if [ -n "$project" ] && git ls-files -z | xargs -0 grep -lIF -e "$project" >/dev/null 2>&1; then
    echo "FOUND the real gcp_project of inventory/prod in a tracked file"
    failed=1
  fi
fi

[ "$failed" -eq 0 ] && echo "secrets check: clean"
exit "$failed"
