#!/usr/bin/env bash
# Stops a commit that adds private text: host names, addresses, paths.
#
# The patterns are private too, so they live outside the repository, one
# extended regular expression per line (lines starting with # are comments):
#   $PRIVATE_PATTERNS, or ${XDG_CONFIG_HOME:-~/.config}/git/private-patterns.txt
#
# Use as a git pre-commit hook (`make hooks`), or run it by hand: it checks
# the staged changes.
set -euo pipefail

patterns="${PRIVATE_PATTERNS:-${XDG_CONFIG_HOME:-$HOME/.config}/git/private-patterns.txt}"
if [[ ! -f "$patterns" ]]; then
    echo "check-private: no pattern file at $patterns; nothing checked." >&2
    exit 0
fi

rules="$(grep -vE '^\s*(#|$)' "$patterns" || true)"
[[ -z "$rules" ]] && exit 0

# Added lines of the staged diff, and the names of the staged files.
added="$(git diff --cached --unified=0 --no-color | grep -E '^\+' | grep -vE '^\+\+\+ ' || true)"
names="$(git diff --cached --name-only)"

found="$(printf '%s\n%s\n' "$added" "$names" | grep -inE -f <(printf '%s\n' "$rules") || true)"
if [[ -n "$found" ]]; then
    echo "check-private: the staged changes contain private text:" >&2
    printf '%s\n' "$found" | head -20 >&2
    echo "Remove it, or edit $patterns if this is a false alarm." >&2
    exit 1
fi
