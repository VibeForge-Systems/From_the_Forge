#!/usr/bin/env bash
# SPDX-License-Identifier: Apache-2.0
#
# verify-rewrite.sh — prove a history rewrite removed what it was meant to,
# before anything is pushed anywhere.
#
#   verify-rewrite.sh --old OLD_MIRROR.git --new REWRITTEN [--identities FILE]
#                     [--denylist FILE] [--allow FILE] [--out DIR] [--no-scan]
#
# OLD is an untouched mirror of the private repository (keep it; it is your
# evidence and your rollback). NEW is the repository git-filter-repo rewrote,
# or a fresh-root squash. The two are only ever compared by listing — no object
# from OLD is fetched into NEW, because that would undo the rewrite.
#
# Checks, each PASS or FAIL:
#   1. every commit filter-repo changed (commit-map) is ABSENT from NEW;
#      for a squash (no commit-map), every OLD commit is absent
#   2. no refs/original, refs/replace or refs/stash in NEW — they keep or map
#      old history, and `git push --mirror` would publish them
#   3. no unreachable objects left in NEW (expire reflogs and gc first)
#   4. every identity in NEW matches --identities (plus GitHub noreply)
#   5. per-branch tip trees: which paths changed between OLD and NEW, so you
#      can see the rewrite touched only what you meant it to
#   6. the scanners run clean on NEW (scan-secrets.sh, sweep-leaks.py)
#
# Exit 0 all PASS · 1 a FAIL · 2 a scan could not complete · 3 usage.

set -uo pipefail
HERE="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
OLD="" NEW="" IDS="" DENY="" ALLOW="" OUT="" SCAN=1
while [ $# -gt 0 ]; do
  case "$1" in
    --old) OLD="${2:?}"; shift 2 ;; --new) NEW="${2:?}"; shift 2 ;;
    --identities) IDS="${2:?}"; shift 2 ;; --denylist) DENY="${2:?}"; shift 2 ;;
    --allow) ALLOW="${2:?}"; shift 2 ;; --out) OUT="${2:?}"; shift 2 ;;
    --no-scan) SCAN=0; shift ;;
    -h|--help) sed -n '3,25p' "$0" | sed 's/^# \{0,1\}//'; exit 0 ;;
    *) echo "verify-rewrite: unknown argument $1" >&2; exit 3 ;;
  esac
done
[ -n "$OLD" ] && [ -n "$NEW" ] || { echo "usage: verify-rewrite.sh --old OLD --new NEW [...]" >&2; exit 3; }
for r in "$OLD" "$NEW"; do git -C "$r" rev-parse --git-dir >/dev/null 2>&1 || { echo "verify-rewrite: not a repo: $r" >&2; exit 3; }; done
OLD="$(cd "$OLD" && pwd)"; NEW="$(cd "$NEW" && pwd)"
[ "$OLD" != "$NEW" ] || { echo "verify-rewrite: --old and --new are the same repository" >&2; exit 3; }
[ -n "$OUT" ] || OUT="$(dirname "$NEW")/$(basename "$NEW" .git)-verify"
mkdir -p "$OUT"

FAILS=0 GAPS=0
pass() { printf '  PASS  %s\n' "$*"; }
fail() { printf '  FAIL  %s\n' "$*"; FAILS=$((FAILS + 1)); }
info() { printf '        %s\n' "$*"; }

NEWGIT="$(git -C "$NEW" rev-parse --absolute-git-dir)"
MAP="$NEWGIT/filter-repo/commit-map"

echo "1. old commits are gone from the rewritten repository"
if [ -f "$MAP" ]; then
  # commit-map: "old new" per line, header first; old==new means untouched
  awk 'NR>1 && $1!=$2 && $1!~/^0+$/ {print $1}' "$MAP" > "$OUT/changed-old-shas"
  n="$(wc -l < "$OUT/changed-old-shas")"
  mode="filter-repo (commit-map: $n changed)"
else
  git -C "$OLD" rev-list --all > "$OUT/changed-old-shas"
  n="$(wc -l < "$OUT/changed-old-shas")"
  mode="no commit-map: treating NEW as a fresh root — every one of $n OLD commits must be absent"
fi
info "$mode"
if [ "$n" -eq 0 ]; then
  fail "filter-repo changed no commits — the rewrite did nothing"
else
  present="$(git -C "$NEW" cat-file --batch-check < "$OUT/changed-old-shas" | grep -cv ' missing$' || true)"
  if [ "$present" -eq 0 ]; then pass "all $n rewritten-away commits are absent"
  else
    fail "$present old commit(s) still present in NEW (a ref, a reflog or an unpruned pack still holds them)"
    git -C "$NEW" cat-file --batch-check < "$OUT/changed-old-shas" | grep -v ' missing$' | head -5 | sed 's/^/          /'
  fi
fi

echo "2. no refs that keep or map old history"
bad="$(git -C "$NEW" for-each-ref --format='%(refname)' refs/original refs/replace refs/stash | head -20)"
if [ -z "$bad" ]; then pass "no refs/original, refs/replace, refs/stash"
else fail "$(wc -l <<<"$bad") leftover ref(s) — delete them before any push:"; sed 's/^/          /' <<<"$bad"; fi
pr="$(git -C "$NEW" for-each-ref refs/pull | wc -l)"
[ "$pr" -gt 0 ] && info "$pr refs/pull/* present locally (rewritten). Push ONLY refs/heads/* and refs/tags/* to the new repo."

echo "3. no unreachable objects"
unr="$(git -C "$NEW" fsck --unreachable --no-reflogs --no-progress 2>/dev/null | wc -l)"
if [ "$unr" -eq 0 ]; then pass "object store holds only reachable history"
else fail "$unr unreachable object(s) — run: git reflog expire --expire=now --all && git gc --prune=now --aggressive"; fi

echo "4. identities"
git -C "$NEW" log --all --format='%ae%n%ce%n%(trailers:key=Signed-off-by,valueonly)%(trailers:key=Co-authored-by,valueonly)' \
  | grep -oE '[^<> ]+@[^<> ]+' | tr 'A-Z' 'a-z' | sort -u > "$OUT/identities"
git -C "$NEW" for-each-ref refs/tags --format='%(taggeremail)' | tr -d '<>' | grep @ | tr 'A-Z' 'a-z' | sort -u >> "$OUT/identities"
sort -u -o "$OUT/identities" "$OUT/identities"
unexpected=""
while read -r e; do
  [ -n "$e" ] || continue
  ok=0
  grep -qiE '@users\.noreply\.github\.com$|^noreply@github\.com$' <<<"$e" && ok=1
  if [ "$ok" = 0 ] && [ -n "$IDS" ]; then
    while read -r pat; do
      case "$pat" in ''|'#'*) continue ;; esac
      grep -qiE -- "$pat" <<<"$e" && { ok=1; break; }
    done < "$IDS"
  fi
  [ "$ok" = 1 ] || unexpected="$unexpected $e"
done < "$OUT/identities"
if [ -z "$unexpected" ]; then pass "$(wc -l < "$OUT/identities") identities, all expected"
elif [ -z "$IDS" ]; then fail "identities not covered by GitHub noreply (no --identities file given):$unexpected"
else fail "identities not in $IDS:$unexpected"; fi

echo "5. what changed at each branch tip (paths whose content differs)"
for b in $(git -C "$OLD" for-each-ref --format='%(refname:short)' refs/heads); do
  if ! git -C "$NEW" rev-parse -q --verify "refs/heads/$b" >/dev/null; then info "$b: absent in NEW"; continue; fi
  diff <(git -C "$OLD" ls-tree -r "refs/heads/$b" | awk '{print $4"\t"$3}' | sort) \
       <(git -C "$NEW" ls-tree -r "refs/heads/$b" | awk '{print $4"\t"$3}' | sort) \
    | awk -F'\t' '/^[<>]/ {sub(/^[<>] /,""); print $1}' | sort -u > "$OUT/tipdiff-$(tr '/' '_' <<<"$b")"
  c="$(wc -l < "$OUT/tipdiff-$(tr '/' '_' <<<"$b")")"
  oc="$(git -C "$OLD" rev-list --count "refs/heads/$b")"; nc="$(git -C "$NEW" rev-list --count "refs/heads/$b")"
  info "$b: $c path(s) differ at the tip; commits $oc -> $nc"
  head -10 "$OUT/tipdiff-$(tr '/' '_' <<<"$b")" | sed 's/^/            /'
done

if [ "$SCAN" = 1 ]; then
  echo "6. scanners on the rewritten repository"
  "$HERE/scan-secrets.sh" --repo "$NEW" --out "$OUT/secrets" --trust > "$OUT/scan-secrets.log" 2>&1
  case $? in 0) pass "scan-secrets: clean" ;; 1) fail "scan-secrets: findings (see $OUT/scan-secrets.log)" ;;
    *) printf '  GAP   scan-secrets did not complete (see %s)\n' "$OUT/scan-secrets.log"; GAPS=1 ;; esac
  sargs=(--repo "$NEW" --json "$OUT/sweep.json" --quiet)
  [ -n "$DENY" ] && sargs+=(--denylist "$DENY")
  [ -n "$ALLOW" ] && sargs+=(--allow "$ALLOW")
  [ -n "$IDS" ] && sargs+=(--identities "$IDS")
  python3 "$HERE/sweep-leaks.py" "${sargs[@]}" > "$OUT/sweep.log" 2>&1
  case $? in 0) pass "sweep-leaks: clean" ;; 1) fail "sweep-leaks: findings (see $OUT/sweep.log)" ;;
    *) printf '  GAP   sweep-leaks did not complete (see %s)\n' "$OUT/sweep.log"; GAPS=1 ;; esac
fi

echo
if [ "$FAILS" -gt 0 ]; then echo "NOT READY — $FAILS check(s) failed. Reports: $OUT"; exit 1; fi
if [ "$GAPS" -gt 0 ]; then echo "INCOMPLETE — a scan did not run to completion; this is not a pass. Reports: $OUT"; exit 2; fi
echo "VERIFIED — safe to push refs/heads/* and refs/tags/* (nothing else) to a NEW repository. Reports: $OUT"
exit 0
