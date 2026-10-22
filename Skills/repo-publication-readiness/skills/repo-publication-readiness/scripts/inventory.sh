#!/usr/bin/env bash
# SPDX-License-Identifier: Apache-2.0
#
# inventory.sh — list everything a repository would expose, before deciding
# anything about it.
#
#   inventory.sh --repo MIRROR.git --out DIR [--github OWNER/REPO]
#
# The git half needs only git. The GitHub half (--github) needs an
# authenticated `gh`; every surface it cannot read is printed UNKNOWN, never
# assumed empty — a surface you could not see is a surface you have not
# audited.
#
# Writes DIR/inventory.txt (and git-filter-repo's --analyze reports to
# DIR/analysis/ when git-filter-repo is installed). Exit 0 always on success;
# 3 on usage errors. This script decides nothing; it is the input to the
# decisions in SKILL.md.

set -uo pipefail

REPO="" OUT="" GH=""
while [ $# -gt 0 ]; do
  case "$1" in
    --repo) REPO="${2:?}"; shift 2 ;;
    --out) OUT="${2:?}"; shift 2 ;;
    --github) GH="${2:?}"; shift 2 ;;
    -h|--help) sed -n '3,17p' "$0" | sed 's/^# \{0,1\}//'; exit 0 ;;
    *) echo "inventory: unknown argument $1" >&2; exit 3 ;;
  esac
done
[ -n "$REPO" ] && [ -n "$OUT" ] || { echo "usage: inventory.sh --repo PATH --out DIR [--github OWNER/REPO]" >&2; exit 3; }
REPO="$(cd "$REPO" && pwd)" || exit 3
git -C "$REPO" rev-parse --git-dir >/dev/null 2>&1 || { echo "inventory: not a git repository: $REPO" >&2; exit 3; }
mkdir -p "$OUT" && OUT="$(cd "$OUT" && pwd)"
case "$OUT/" in "$REPO"/*) echo "inventory: --out must be outside the repository" >&2; exit 3 ;; esac

g() { git -C "$REPO" "$@"; }
exec > >(tee "$OUT/inventory.txt") 2>&1

echo "# Inventory of $REPO — $(date -u +%Y-%m-%dT%H:%M:%SZ)"
echo
echo "## Refs (every one of these is published by a visibility flip)"
g for-each-ref --format='%(refname)' | sed -E 's#^(refs/(heads|tags|remotes|pull|notes|stash|replace|original))(/.*)?$#\1#; t; s#^(refs/[^/]+).*#\1 (other)#' \
  | sort | uniq -c | sed 's/^/  /'
[ "$(g rev-parse --is-bare-repository)" = true ] || echo "  (working-tree clone: refs/pull/* are absent here; use git clone --mirror to see them)"
if g rev-parse -q --verify refs/stash >/dev/null; then echo "  stash entries: $(g stash list | wc -l)"; fi
for ns in refs/replace refs/original; do
  c="$(g for-each-ref "$ns" | wc -l)"; [ "$c" -gt 0 ] && echo "  WARNING: $c ref(s) under $ns — leftovers of a rewrite; they map or keep OLD history. Never push them."
done

echo
echo "## History"
echo "  commits (all refs): $(g rev-list --all | wc -l)"
echo "  merges:             $(g rev-list --all --merges | wc -l)"
echo "  root commits:       $(g rev-list --all --max-parents=0 | wc -l)"
echo "  first / last:       $(g log --all --reverse --format=%cs | head -1) / $(g log --all -1 --format=%cs)"
echo "  signed commits:     $(g log --all --format=%G? 2>/dev/null | grep -cv "^N$" || true) (a rewrite strips every signature)"
unr="$(g fsck --unreachable --no-reflogs --no-progress 2>/dev/null | wc -l)"
echo "  unreachable objects: $unr (not pushed by a branch/tag push; a server copy can still hold its own — see references/github-exposure.md)"

echo
echo "## Identities (author, committer, tagger) — every one becomes public"
{ g log --all --format='%an <%ae>%n%cn <%ce>'; g for-each-ref refs/tags --format='%(taggername) %(taggeremail)' | grep '<' || true; } \
  | sort | uniq -c | sort -rn | sed 's/^/  /'
echo "  trailer identities (Signed-off-by, Co-authored-by, …):"
g log --all --format='%(trailers:only,unfold)' | grep -iE '^[A-Za-z-]+-by: ' | sed -E 's/^[^:]+: //' | sort | uniq -c | sort -rn | sed 's/^/    /' | head -40

echo
echo "## Content that needs a decision"
echo "  submodules ever declared:"
g log --all --format= --name-only -- .gitmodules '**/.gitmodules' | sort -u | sed 's/^/    /'
for ref in $(g for-each-ref --format='%(refname)' refs/heads refs/tags | head -50); do
  g show "$ref:.gitmodules" 2>/dev/null | grep -E '^\s*url' | sed "s#^#    $ref: #"
done | sort -u
echo "  Git LFS (filter=lfs in any .gitattributes at a branch/tag tip):"
for ref in $(g for-each-ref --format='%(refname)' refs/heads refs/tags | head -200); do
  g show "$ref:.gitattributes" 2>/dev/null | grep -q 'filter=lfs' && echo "    $ref"
done | sort -u
echo "  largest blobs in history:"
g rev-list --objects --all | g cat-file --batch-check='%(objecttype) %(objectname) %(objectsize) %(rest)' \
  | awk '$1=="blob"' | sort -k3 -n -r | head -15 | awk '{printf "    %8.1f KiB  %s  %s\n", $3/1024, substr($2,1,10), $4}'
echo "  file kinds that usually carry data or metadata (count of paths ever added):"
g log --all --format= --name-only --diff-filter=A | sort -u \
  | grep -ioE '\.(ipynb|sqlite|sqlite3|db|csv|xlsx?|docx?|pptx?|pdf|png|jpe?g|heic|har|pcap|log|bak|dump|sql|zip|tar|gz|7z|env)$' \
  | tr 'A-Z' 'a-z' | sort | uniq -c | sort -rn | sed 's/^/    /'

if command -v git-filter-repo >/dev/null 2>&1; then
  echo
  echo "## git filter-repo --analyze (reports in $OUT/analysis/)"
  rm -rf "$OUT/analysis"
  if g filter-repo --analyze --report-dir "$OUT/analysis" >/dev/null 2>&1; then
    echo "  written: $(ls "$OUT/analysis" | tr '\n' ' ')"
    echo "  deleted paths still in history (top 15):"
    sed -n '3,17p' "$OUT/analysis/path-deleted-sizes.txt" 2>/dev/null | sed 's/^/    /'
  else
    echo "  git filter-repo --analyze failed; run it by hand in a fresh clone"
  fi
else
  echo; echo "## git filter-repo not installed (pip install git-filter-repo) — --analyze report not produced"
fi

if [ -n "$GH" ]; then
  echo
  echo "## GitHub surfaces of $GH (UNKNOWN = could not read it, which is not the same as empty)"
  if ! command -v gh >/dev/null 2>&1; then
    echo "  UNKNOWN: gh not installed — audit these by hand in the web UI"
  else
    ghq() {  # LABEL  ENDPOINT  JQ
      local v
      if v="$(gh api -X GET "$2" --jq "$3" 2>/dev/null)"; then printf '  %-34s %s\n' "$1" "${v:-0}"
      else printf '  %-34s UNKNOWN (no access)\n' "$1"; fi
    }
    ghq "visibility"                    "repos/$GH" '.visibility'
    ghq "wiki enabled"                  "repos/$GH" '.has_wiki'
    ghq "pages enabled"                 "repos/$GH" '.has_pages'
    ghq "open+closed issues & PRs (approx)" "search/issues?q=repo:$GH&per_page=1" '.total_count'
    ghq "pull requests (each is a refs/pull ref)" "search/issues?q=repo:$GH+is:pr&per_page=1" '.total_count'
    ghq "releases"                      "repos/$GH/releases?per_page=100" 'length'
    ghq "release assets"                "repos/$GH/releases?per_page=100" '[.[].assets[]] | length'
    ghq "forks (private forks join the public network)" "repos/$GH/forks?per_page=100" 'length'
    ghq "environments"                  "repos/$GH/environments" '.total_count'
    ghq "deploy keys"                   "repos/$GH/keys" 'length'
    ghq "webhooks"                      "repos/$GH/hooks" 'length'
    ghq "Actions secrets (names publish via logs)" "repos/$GH/actions/secrets" '.total_count'
    ghq "Actions variables (values are NOT secret)" "repos/$GH/actions/variables" '.total_count'
    ghq "Actions runs (logs go public on a flip)" "repos/$GH/actions/runs?per_page=1" '.total_count'
    ghq "Actions artifacts"             "repos/$GH/actions/artifacts?per_page=1" '.total_count'
    ghq "self-hosted runners"           "repos/$GH/actions/runners" '.total_count'
    ghq "discussions enabled"           "repos/$GH" '.has_discussions'
    echo "  packages: list them in the org/user Packages tab — linked packages keep their own visibility."
  fi
fi
echo
echo "# end of inventory"
