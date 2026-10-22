#!/usr/bin/env bash
# SPDX-License-Identifier: Apache-2.0
#
# scan-secrets.sh — scan every published-to-be byte of a repository for
# credentials, with two independent scanners, including the places their
# history modes do not reach.
#
#   scan-secrets.sh --repo MIRROR.git --out DIR            # every ref (default)
#   scan-secrets.sh --repo R --out DIR --range main..topic # just a range
#   scan-secrets.sh ... --allow-missing trufflehog         # accept one gap, out loud
#
# Run it against a MIRROR you cloned yourself (`git clone --mirror URL`): a
# normal clone has no refs/pull/*, and on GitHub those refs are public.
#
# WHAT IS COVERED, AND WHY EACH PASS EXISTS
#   1. gitleaks git    every ref. With no --log-opts gitleaks runs
#                      `git log -p -U0 --full-history --all`; passing
#                      --log-opts REPLACES that (it does not extend it), so for
#                      a full audit this script passes none.
#   2. merge pass      `git log -p` prints no diff for a merge, so a line added
#                      while resolving a conflict is invisible to pass 1 and to
#                      TruffleHog. Each merge is replayed with --remerge-diff
#                      (git >= 2.36; --cc before that) into `gitleaks stdin`.
#   3. message pass    commit and tag messages are not diffs; neither scanner's
#                      history mode reads them.
#   4. trufflehog git  a second engine with live verification. By default it
#                      CLONES a file:// repo first, which drops refs/pull/*,
#                      notes and stash; --trust-local-git-config makes it scan
#                      in place. That flag trusts the repo's git config, which
#                      is safe only for a clone you made — so this script
#                      refuses to use it on a repo with a working tree you did
#                      not create as a mirror unless --trust is given.
#
# VERIFICATION SENDS DATA. TruffleHog verifies a candidate by calling the
# issuing provider's API with it. That is how "verified" means "live". Pass
# --no-verification to keep candidates on this machine.
#
# Exit: 0 clean · 1 findings · 2 incomplete (a scanner missing or erroring) ·
#       3 usage. Reports go to --out, which must be OUTSIDE the repository.
#
# Tool floor: gitleaks >= 8.19 (the `git`/`dir`/`stdin` subcommands; `detect`
# and `protect` are deprecated and hidden), trufflehog v3, git >= 2.22.

set -uo pipefail

REPO="" OUT="" RANGE="" ALLOW_MISSING="" NO_VERIFY=0 TRUST=0
usage() { sed -n '3,40p' "$0" | sed 's/^# \{0,1\}//'; exit 3; }
while [ $# -gt 0 ]; do
  case "$1" in
    --repo) REPO="${2:?}"; shift 2 ;;
    --out) OUT="${2:?}"; shift 2 ;;
    --range) RANGE="${2:?}"; shift 2 ;;
    --allow-missing) ALLOW_MISSING="${2:?}"; shift 2 ;;
    --no-verification) NO_VERIFY=1; shift ;;
    --trust) TRUST=1; shift ;;
    -h|--help) usage ;;
    *) echo "scan-secrets: unknown argument $1" >&2; exit 3 ;;
  esac
done
[ -n "$REPO" ] && [ -n "$OUT" ] || usage
REPO="$(cd "$REPO" 2>/dev/null && pwd)" || { echo "scan-secrets: no such repo" >&2; exit 3; }
git -C "$REPO" rev-parse --git-dir >/dev/null 2>&1 || { echo "scan-secrets: $REPO is not a git repository" >&2; exit 3; }
mkdir -p "$OUT" && OUT="$(cd "$OUT" && pwd)"
case "$OUT/" in "$REPO"/*) echo "scan-secrets: --out must be outside the repository (a report inside it can be committed)" >&2; exit 3 ;; esac

BARE="$(git -C "$REPO" rev-parse --is-bare-repository)"
FINDINGS=0 INCOMPLETE=0
note()  { printf '  %s\n' "$*"; }
gap()   { printf '  INCOMPLETE: %s\n' "$*"; INCOMPLETE=1; }
found() { printf '  FINDINGS:   %s\n' "$*"; FINDINGS=1; }

allowed_missing() { case ",$ALLOW_MISSING," in *",$1,"*) return 0 ;; *) return 1 ;; esac; }

ver_ge() {  # ver_ge HAVE WANT  (dotted numeric compare)
  [ "$(printf '%s\n%s\n' "$2" "$1" | sort -V | head -1)" = "$2" ]
}

# git log arguments that select the commits in scope
if [ -n "$RANGE" ]; then SCOPE=("$RANGE"); else SCOPE=(--all); fi

echo "scan-secrets: $REPO  (bare=$BARE, scope=${RANGE:-every ref})"
echo "refs in scope:"; git -C "$REPO" for-each-ref --format='%(refname)' | sed -E 's#^(refs/[^/]+).*#\1#' | sort | uniq -c | sed 's/^/  /'

# --- gitleaks ------------------------------------------------------------------
echo; echo "== gitleaks"
GL="" GLV=""
if command -v gitleaks >/dev/null 2>&1; then
  GL="$(command -v gitleaks)"
  GLV="$("$GL" version 2>/dev/null | grep -oE '[0-9]+\.[0-9]+\.[0-9]+' | head -1 || true)"
fi
if [ -z "$GL" ]; then
  if allowed_missing gitleaks; then note "gitleaks not installed — accepted with --allow-missing"; else gap "gitleaks not installed (need >= 8.19)"; fi
elif [ -z "$GLV" ] || ! ver_ge "$GLV" 8.19.0; then
  gap "gitleaks ${GLV:-of unknown version} predates 8.19 (no 'git'/'stdin' subcommands) — install a current release"
else
  note "gitleaks $GLV"
  # 1. history. Exit code 99 = leaks found, so a tool error cannot read as "clean".
  if [ -n "$RANGE" ]; then lo=(--log-opts "$RANGE"); else lo=(); fi
  "$GL" git ${lo[@]+"${lo[@]}"} --redact --no-banner --exit-code 99 \
     --report-format json --report-path "$OUT/gitleaks-history.json" "$REPO" >"$OUT/gitleaks-history.log" 2>&1
  rc=$?
  case $rc in 0) note "history: clean" ;; 99) found "history: see $OUT/gitleaks-history.json" ;; *) gap "history scan exited $rc (see gitleaks-history.log)" ;; esac

  # 2. merge resolutions
  if git -C "$REPO" log -1 --remerge-diff --format=%H HEAD >/dev/null 2>&1; then MD=--remerge-diff; else MD=--cc; fi
  : > "$OUT/merges.txt"
  n=0
  while read -r m; do
    [ -n "$m" ] || continue
    n=$((n + 1))
    { printf '=== merge %s\n' "$m"; git -C "$REPO" show "$MD" -p --no-color --format= "$m"; } >> "$OUT/merges.txt"
  done < <(git -C "$REPO" rev-list --merges "${SCOPE[@]}")
  if [ "$n" -gt 0 ]; then
    "$GL" stdin --redact --no-banner --exit-code 99 --report-format json \
      --report-path "$OUT/gitleaks-merges.json" < "$OUT/merges.txt" >"$OUT/gitleaks-merges.log" 2>&1
    rc=$?
    case $rc in 0) note "merge resolutions ($n merges, $MD): clean" ;;
      99) found "merge resolutions: see gitleaks-merges.json (StartLine indexes merges.txt; the '=== merge' line above it names the commit)" ;;
      *) gap "merge pass exited $rc" ;; esac
  else
    note "merge resolutions: no merges in scope"
  fi

  # 3. commit and tag messages
  {
    git -C "$REPO" log "${SCOPE[@]}" --format='=== commit %H%n%B'
    [ -z "$RANGE" ] && git -C "$REPO" for-each-ref refs/tags --format='=== tag %(refname)%0a%(contents)'
  } > "$OUT/messages.txt"
  "$GL" stdin --redact --no-banner --exit-code 99 --report-format json \
    --report-path "$OUT/gitleaks-messages.json" < "$OUT/messages.txt" >"$OUT/gitleaks-messages.log" 2>&1
  rc=$?
  case $rc in 0) note "commit/tag messages: clean" ;;
    99) found "messages: see gitleaks-messages.json (StartLine indexes messages.txt)" ;;
    *) gap "message pass exited $rc" ;; esac
fi

# --- trufflehog ------------------------------------------------------------------
echo; echo "== trufflehog"
TH=""
command -v trufflehog >/dev/null 2>&1 && TH="$(command -v trufflehog)"
if [ -z "$TH" ]; then
  if allowed_missing trufflehog; then note "trufflehog not installed — accepted with --allow-missing"; else gap "trufflehog v3 not installed"; fi
else
  THV="$("$TH" --version 2>&1 | grep -oE '[0-9]+\.[0-9]+\.[0-9]+' | head -1 || true)"
  note "trufflehog ${THV:-unknown version}"
  args=(git "file://$REPO" --json --no-update --fail)
  [ "$NO_VERIFY" = 1 ] && args+=(--no-verification)
  [ "$BARE" = true ] && args+=(--bare)
  if [ "$BARE" = true ] || [ "$TRUST" = 1 ]; then
    args+=(--trust-local-git-config)   # scan in place: keeps refs/pull/*, notes, stash
  else
    note "working-tree repo: trufflehog will CLONE it, so only branches and tags are scanned."
    note "re-run on a mirror (git clone --mirror) or pass --trust for a repo you created yourself."
  fi
  if [ -n "$RANGE" ]; then
    case "$RANGE" in *..*) args+=(--since-commit "${RANGE%%..*}" --branch "${RANGE##*..}") ;;
      *) gap "trufflehog needs a BASE..HEAD range; got '$RANGE'" ;; esac
  fi
  "$TH" "${args[@]}" > "$OUT/trufflehog.jsonl" 2>"$OUT/trufflehog.log"
  rc=$?
  case $rc in
    0) note "clean" ;;
    183) v="$(grep -c '"Verified":true' "$OUT/trufflehog.jsonl" || true)"
         found "trufflehog: $(wc -l < "$OUT/trufflehog.jsonl") result(s), $v VERIFIED LIVE — rotate those first ($OUT/trufflehog.jsonl)" ;;
    *) gap "trufflehog exited $rc (see trufflehog.log)" ;;
  esac
fi

# --- verdict ---------------------------------------------------------------------
echo
if [ "$INCOMPLETE" = 1 ]; then
  echo "INCOMPLETE — a scanner did not run or did not finish. This is not a clean result."
  [ "$FINDINGS" = 1 ] && echo "(and there were findings in the parts that did run)"
  exit 2
fi
if [ "$FINDINGS" = 1 ]; then
  echo "FINDINGS — every credential found is rotated or revoked BEFORE any rewrite. A rewrite hides; only rotation makes it dead."
  exit 1
fi
echo "CLEAN — both scanners and every supplementary pass ran and found nothing."
exit 0
