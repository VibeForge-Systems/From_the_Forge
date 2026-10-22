#!/usr/bin/env bash
# SPDX-License-Identifier: Apache-2.0
#
# check-github-settings.sh — read a repository's GitHub settings and judge them
# against the bar for a public repository.
#
#   check-github-settings.sh OWNER/REPO [--branch main]
#
# Run it on the NEW public repository right after the first push, and again
# whenever settings change. Needs an authenticated `gh`; most checks need admin
# on the repository (GitHub only returns security_and_analysis to admins).
#
# Verdicts: PASS · FAIL · WARN (a judgment call, named) · UNKNOWN (could not be
# read — not the same as passing). Exit 0 only if nothing FAILed and nothing is
# UNKNOWN; 1 if anything FAILed; 2 if the worst is UNKNOWN.
#
# Field names follow GitHub's OpenAPI description (github/rest-api-description,
# checked 2026-10-18). Nothing here changes a setting: each FAIL prints where to
# fix it, and the human decides.

set -uo pipefail
REPO="${1:-}"; shift || true
BRANCH=""
while [ $# -gt 0 ]; do
  case "$1" in --branch) BRANCH="${2:?}"; shift 2 ;; *) echo "unknown argument $1" >&2; exit 3 ;; esac
done
case "$REPO" in */*) ;; *) echo "usage: check-github-settings.sh OWNER/REPO [--branch NAME]" >&2; exit 3 ;; esac
command -v gh >/dev/null 2>&1 || { echo "check-github-settings: gh (GitHub CLI) is required" >&2; exit 3; }

FAILS=0 UNKNOWNS=0
row() {  # VERDICT CHECK DETAIL
  printf '  %-8s %-46s %s\n' "$1" "$2" "$3"
  case "$1" in FAIL) FAILS=$((FAILS + 1)) ;; UNKNOWN) UNKNOWNS=$((UNKNOWNS + 1)) ;; esac
}
api() { gh api -X GET -H 'Accept: application/vnd.github+json' "$@" 2>/dev/null; }
status_of() { gh api -X GET -i "$1" 2>/dev/null | head -1 | grep -oE '[0-9]{3}' | head -1; }

meta="$(api "repos/$REPO")" || { echo "cannot read repos/$REPO — check gh auth and the name" >&2; exit 3; }
j() { jq -r "$1" <<<"$meta"; }
[ -n "$BRANCH" ] || BRANCH="$(j .default_branch)"
echo "GitHub settings for $REPO (visibility: $(j .visibility), default branch: $BRANCH)"
echo

echo "Secret and vulnerability protection (Settings → Security and quality → Advanced Security)"
if [ "$(j '.security_and_analysis // empty')" = "" ]; then
  row UNKNOWN "secret scanning / push protection" "not returned — GitHub shows security_and_analysis to admins only"
else
  for k in secret_scanning secret_scanning_push_protection; do
    v="$(j ".security_and_analysis.$k.status // \"absent\"")"
    if [ "$v" = enabled ]; then row PASS "$k" enabled
    else row FAIL "$k" "$v — a visibility flip does not switch this on; enable it"; fi
  done
  v="$(j '.security_and_analysis.secret_scanning_non_provider_patterns.status // "absent"')"
  [ "$v" = enabled ] && row PASS secret_scanning_non_provider_patterns enabled \
    || row WARN secret_scanning_non_provider_patterns "$v — catches generic secrets (private keys, connection strings)"
fi
if pv="$(api "repos/$REPO/private-vulnerability-reporting")"; then
  [ "$(jq -r .enabled <<<"$pv")" = true ] && row PASS "private vulnerability reporting" enabled \
    || row FAIL "private vulnerability reporting" "disabled — not on by default; SECURITY.md should point here"
else row UNKNOWN "private vulnerability reporting" "could not read"; fi
case "$(status_of "repos/$REPO/vulnerability-alerts")" in
  204) row PASS "Dependabot alerts" enabled ;;
  404) row FAIL "Dependabot alerts" disabled ;;
  *)   row UNKNOWN "Dependabot alerts" "could not read" ;;
esac
if ds="$(api "repos/$REPO/automated-security-fixes")"; then
  [ "$(jq -r .enabled <<<"$ds")" = true ] && row PASS "Dependabot security updates" enabled \
    || row WARN "Dependabot security updates" "disabled — alerts without fixes"
else row UNKNOWN "Dependabot security updates" "could not read"; fi
if cs="$(api "repos/$REPO/code-scanning/default-setup")"; then
  [ "$(jq -r .state <<<"$cs")" = configured ] && row PASS "code scanning default setup" configured \
    || row WARN "code scanning default setup" "not configured — free on public repos, never on by itself (a CodeQL workflow also counts)"
else row UNKNOWN "code scanning default setup" "could not read"; fi
sp=""
for p in SECURITY.md .github/SECURITY.md docs/SECURITY.md; do
  api "repos/$REPO/contents/$p" >/dev/null && { sp="$p"; break; }
done
[ -n "$sp" ] && row PASS "security policy" "$sp" || row FAIL "security policy" "no SECURITY.md in /, .github/ or docs/"

echo
echo "Branch protection on $BRANCH (rulesets or classic protection)"
# Rulesets surface through rules/branches; classic protection does not, so it is
# read separately (its details need admin) and folded in as the same rule names.
if rules="$(api "repos/$REPO/rules/branches/$BRANCH")"; then
  types="$(jq -r '[.[].type] | unique | join(",")' <<<"$rules")"
  if [ "$(api "repos/$REPO/branches/$BRANCH" | jq -r .protected)" = true ]; then
    if prot="$(api "repos/$REPO/branches/$BRANCH/protection")"; then
      [ "$(jq -r '.allow_deletions.enabled' <<<"$prot")" = false ] && types="$types,deletion"
      [ "$(jq -r '.allow_force_pushes.enabled' <<<"$prot")" = false ] && types="$types,non_fast_forward"
      [ "$(jq -r '.required_pull_request_reviews // empty' <<<"$prot")" != "" ] && types="$types,pull_request"
      [ "$(jq -r '.required_status_checks // empty' <<<"$prot")" != "" ] && types="$types,required_status_checks"
    else
      row UNKNOWN "classic branch protection" "branch is protected but details need admin; rule verdicts below may be incomplete"
    fi
  fi
  for t in deletion non_fast_forward pull_request; do
    case ",$types," in *",$t,"*) row PASS "rule: $t" active ;;
      *) row FAIL "rule: $t" "absent — note a visibility flip DISABLES push rulesets" ;; esac
  done
  case ",$types," in *",required_status_checks,"*) row PASS "rule: required_status_checks" active ;;
    *) row WARN "rule: required_status_checks" "absent — fine only if no CI is meant to gate merges" ;; esac
else row UNKNOWN "branch rules" "could not read repos/$REPO/rules/branches/$BRANCH"; fi

echo
echo "GitHub Actions (Settings → Actions → General)"
if ap="$(api "repos/$REPO/actions/permissions")"; then
  [ "$(jq -r .sha_pinning_required <<<"$ap")" = true ] && row PASS "require actions pinned to a full SHA" enabled \
    || row WARN "require actions pinned to a full SHA" "off — the workflow-hardening check is then the only guard"
  row PASS "allowed actions" "$(jq -r .allowed_actions <<<"$ap") (review if 'all')"
else row UNKNOWN "Actions permissions" "could not read"; fi
if wp="$(api "repos/$REPO/actions/permissions/workflow")"; then
  [ "$(jq -r .default_workflow_permissions <<<"$wp")" = read ] && row PASS "default GITHUB_TOKEN permissions" read \
    || row FAIL "default GITHUB_TOKEN permissions" "write — set to read; grant write per job"
  [ "$(jq -r .can_approve_pull_request_reviews <<<"$wp")" = false ] && row PASS "Actions can approve PRs" "no" \
    || row FAIL "Actions can approve PRs" "yes — a workflow could approve its own PR"
else row UNKNOWN "workflow token permissions" "could not read"; fi
if fa="$(api "repos/$REPO/actions/permissions/fork-pr-contributor-approval")"; then
  v="$(jq -r .approval_policy <<<"$fa")"
  case "$v" in
    all_external_contributors) row PASS "fork PR approval" "$v" ;;
    *) row WARN "fork PR approval" "$v — first-timer policies are gamed by one trivial merged PR; all_external_contributors is strictest" ;;
  esac
else row UNKNOWN "fork PR approval" "could not read"; fi
runs="$(api "repos/$REPO/actions/runs?per_page=1" | jq -r .total_count 2>/dev/null || true)"
[ -n "$runs" ] && row WARN "workflow run history" "$runs run(s) — public, logs included, to anyone signed in" \
  || row UNKNOWN "workflow run history" "could not read"

echo
echo "Exposure surfaces"
[ "$(j .has_wiki)" = true ] && row WARN "wiki" "enabled — public; disable if unused" || row PASS "wiki" disabled
[ "$(j .has_pages)" = true ] && row WARN "Pages" "enabled — on Free/Pro the site was public even while the repo was private" || row PASS "Pages" "not enabled"
if cp="$(api "repos/$REPO/community/profile")"; then
  missing="$(jq -r '.files | to_entries | map(select(.value == null) | .key) | join(", ")' <<<"$cp")"
  row "$([ -z "$missing" ] && echo PASS || echo WARN)" "community profile $(jq -r .health_percentage <<<"$cp")%" "${missing:+missing: $missing}"
else row UNKNOWN "community profile" "could not read"; fi

echo
if [ "$FAILS" -gt 0 ]; then echo "$FAILS FAIL, $UNKNOWNS UNKNOWN"; exit 1; fi
if [ "$UNKNOWNS" -gt 0 ]; then echo "0 FAIL, $UNKNOWNS UNKNOWN — unread is not passed; check those by hand or rerun with admin rights"; exit 2; fi
echo "all checks PASS (WARNs are judgment calls named above)"
exit 0
