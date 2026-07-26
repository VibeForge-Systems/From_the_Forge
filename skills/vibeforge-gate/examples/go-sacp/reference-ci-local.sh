#!/usr/bin/env bash
#
# scripts/ci-local.sh — run every CI gate on this machine.
#
# WHY THIS EXISTS. This repository's GitHub Actions are unavailable (the account
# does not fund Actions minutes), so the branch-protection gates never run. This
# script reproduces each of the five CI workflows FAITHFULLY — the same commands,
# the same pinned tool versions — as one local gate you run before you merge.
# Green here is what green CI would have said. It is the merge bar.
#
# It runs one gate per .github/workflows/*.yml:
#   1. gateway-ci            go mod verify + build + vet + test        (gateway-ci.yml)
#   2. federation-boundary   ADR-006 archcheck reverse-import + decision-point
#   3. no-static-credentials ADR-015 structural test + gitleaks secret scan
#   4. dco-check             ADR-009 Signed-off-by on every non-merge, non-bot commit
#   5. artifact-validation   JSON-Schema meta-validation + hash-chain reproduction + cedar validate
#
# It ALSO runs the two gates CLAUDE.md mandates before every commit that no
# workflow runs: gofmt and `go test -race`. Set SACP_CI_SKIP_RACE=1 to skip the
# slow race pass (the CI-faithful set does not include it anyway).
#
# HONEST SCOPE — a gate whose optional tool cannot be found is reported SKIPPED,
# loudly, and a skip makes the whole run exit non-zero unless SACP_CI_ALLOW_SKIP=1.
# A skipped gate is NEVER counted as passed: a silent coverage gap — "it went
# green because the check never ran" — is exactly the failure this script guards
# against. If you see SKIP, the local gate is weaker than CI until you install the
# tool it names.
#
# Exit code: 0 iff every gate PASSED (and none SKIPPED, unless SACP_CI_ALLOW_SKIP=1).

set -uo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$ROOT"

# cedar (ADR-003 policy validator, pinned 4.11.2) is installed via cargo and lives
# in ~/.cargo/bin, which is not always on PATH for a non-login shell.
export PATH="$HOME/.cargo/bin:$PATH"

GITLEAKS_VERSION="8.30.1"      # pinned to match no-static-credentials.yml exactly
CACHE_DIR="${XDG_CACHE_HOME:-$HOME/.cache}/sacp-ci"

# --- reporting ---------------------------------------------------------------
bold=$'\033[1m'; red=$'\033[31m'; grn=$'\033[32m'; ylw=$'\033[33m'; rst=$'\033[0m'
declare -a NAMES=() VERDICTS=()
FAILED=0 SKIPPED=0

record() { NAMES+=("$1"); VERDICTS+=("$2"); }
say()    { printf '\n%s==> %s%s\n' "$bold" "$1" "$rst"; }

# run_gate NAME CMD... — runs CMD, streams its output, records PASS/FAIL.
run_gate() {
  local name="$1"; shift
  say "$name"
  if "$@"; then
    printf '%s   PASS%s  %s\n' "$grn" "$rst" "$name"
    record "$name" PASS
  else
    printf '%s   FAIL%s  %s\n' "$red" "$rst" "$name"
    record "$name" FAIL; FAILED=1
  fi
}

# skip_gate NAME REASON — a gate that could NOT run because its tool is missing.
# This is a real coverage gap vs CI, so it blocks (sets SKIPPED) unless the caller
# accepts the gap with SACP_CI_ALLOW_SKIP=1.
skip_gate() {
  printf '%s   SKIP%s  %s — %s\n' "$ylw" "$rst" "$1" "$2"
  record "$1" SKIP; SKIPPED=1
}

# note_skip NAME REASON — a DELIBERATE skip the operator chose (e.g. the -race
# pass, which no CI workflow runs). Reported for visibility but NOT a coverage gap
# relative to CI, so it does not block.
note_skip() {
  printf '%s   SKIP%s  %s — %s (deliberate; not part of the CI-faithful set)\n' "$ylw" "$rst" "$1" "$2"
  record "$1" "skip"
}

# --- gate 1: gateway-ci (go mod verify + build + vet + test) ------------------
gate_gateway_ci() (
  cd "$ROOT/gateway"
  go mod verify \
    && go build -mod=readonly ./... \
    && go vet ./... \
    && go test -mod=readonly ./...
)

# --- gate 2: federation-boundary (ADR-006 §2 + §5) ---------------------------
gate_federation() (
  cd "$ROOT/gateway"
  bash tools/archcheck.sh reverse-import \
    && bash tools/archcheck.sh decision-point
)

# --- gate 3a: ADR-015 structural credential check ----------------------------
gate_static_cred_structural() (
  cd "$ROOT/gateway"
  go test -tags=architecture ./internal/identity/... -run 'NoStaticCredential|Unrepresentable'
)

# --- gate 3b: gitleaks secret scan -------------------------------------------
# Mirrors no-static-credentials.yml: same binary version, same flags. .claude/ is
# git-ignored agent scratch that a CI checkout never contains, so the .gitleaks
# config allowlists it — a local working tree is otherwise noisier than CI's.
find_or_fetch_gitleaks() {
  if command -v gitleaks >/dev/null 2>&1; then GITLEAKS=gitleaks; return 0; fi
  local cached="$CACHE_DIR/gitleaks-$GITLEAKS_VERSION"
  if [ -x "$cached" ]; then GITLEAKS="$cached"; return 0; fi
  mkdir -p "$CACHE_DIR"
  local url="https://github.com/gitleaks/gitleaks/releases/download/v${GITLEAKS_VERSION}/gitleaks_${GITLEAKS_VERSION}_linux_x64.tar.gz"
  if curl -fsSL --max-time 90 "$url" -o "$CACHE_DIR/gl.tgz" 2>/dev/null \
     && tar xzf "$CACHE_DIR/gl.tgz" -C "$CACHE_DIR" gitleaks 2>/dev/null; then
    mv "$CACHE_DIR/gitleaks" "$cached" && chmod +x "$cached"
    GITLEAKS="$cached"; return 0
  fi
  return 1
}
run_gitleaks() {
  say "no-static-credentials (gitleaks)"
  if ! find_or_fetch_gitleaks; then
    skip_gate "gitleaks" "not installed and could not download v$GITLEAKS_VERSION (offline?); \`go install github.com/gitleaks/gitleaks/v8@v$GITLEAKS_VERSION\`"
    return
  fi
  if "$GITLEAKS" detect --source "$ROOT" --no-git --config "$ROOT/.gitleaks.toml" --redact --no-banner; then
    printf '%s   PASS%s  gitleaks\n' "$grn" "$rst"; record "gitleaks" PASS
  else
    printf '%s   FAIL%s  gitleaks\n' "$red" "$rst"; record "gitleaks" FAIL; FAILED=1
  fi
}

# --- gate 4: DCO (Signed-off-by on every non-merge, non-bot commit) -----------
# github-script in dco-check.yml checks a PR's commits; the local analogue is the
# commits on this branch that are not yet on main. Merge commits and *[bot]
# authors are exempt, exactly as the workflow exempts them.
run_dco() {
  say "dco-check (Signed-off-by)"
  local base ref="main"
  git rev-parse --verify --quiet origin/main >/dev/null 2>&1 && ref="origin/main"
  base="$(git merge-base HEAD "$ref" 2>/dev/null || true)"
  if [ -z "$base" ] || [ "$base" = "$(git rev-parse HEAD)" ]; then
    printf '%s   PASS%s  dco-check (no commits ahead of %s to check)\n' "$grn" "$rst" "$ref"
    record "dco-check" PASS; return
  fi
  local bad=0 sha author body
  while read -r sha; do
    [ -z "$sha" ] && continue
    author="$(git show -s --format='%an' "$sha")"
    case "$author" in *'[bot]') continue;; esac
    body="$(git show -s --format='%B' "$sha")"
    if ! grep -qiE '^Signed-off-by: .+ <.+@.+>' <<<"$body"; then
      printf '%s   missing Signed-off-by:%s %s  %s\n' "$red" "$rst" "${sha:0:8}" "$(git show -s --format='%s' "$sha")"
      bad=1
    fi
  done < <(git rev-list --no-merges "$base..HEAD")
  if [ "$bad" -eq 0 ]; then
    printf '%s   PASS%s  dco-check (%s..HEAD)\n' "$grn" "$rst" "$ref"
    record "dco-check" PASS
  else
    printf '%s   FAIL%s  dco-check — add a sign-off with `git commit -s` / `git rebase --signoff`\n' "$red" "$rst"
    record "dco-check" FAIL; FAILED=1
  fi
}

# --- gate 5: artifact-validation (schemas + hash chain + cedar) --------------
run_artifacts() {
  say "artifact-validation (schemas + hash chain + cedar)"
  local ok=1
  if ! python3 -c 'import jsonschema' 2>/dev/null; then
    skip_gate "artifact-validation" "python jsonschema missing (\`pip install 'jsonschema>=4,<5'\`)"
    return
  fi
  python3 scripts/validate_schemas.py   || ok=0
  python3 scripts/verify_hash_chain.py  || ok=0
  if command -v cedar >/dev/null 2>&1; then
    python3 scripts/check_cedar.py      || ok=0
  else
    ok=0
    printf '%s   cedar CLI missing%s — `cargo install cedar-policy-cli --version 4.11.2 --locked`; check_cedar not run\n' "$ylw" "$rst"
    skip_gate "check_cedar" "cedar CLI not found"
    [ "$ok" -eq 1 ] && return
  fi
  if [ "$ok" -eq 1 ]; then
    printf '%s   PASS%s  artifact-validation\n' "$grn" "$rst"; record "artifact-validation" PASS
  else
    printf '%s   FAIL%s  artifact-validation\n' "$red" "$rst"; record "artifact-validation" FAIL; FAILED=1
  fi
}

# --- CLAUDE.md pre-commit extras: gofmt + race -------------------------------
gate_gofmt() (
  cd "$ROOT/gateway"
  out="$(gofmt -l internal/ cmd/)"
  if [ -n "$out" ]; then echo "gofmt needed on:"; echo "$out"; exit 1; fi
  echo "gofmt: clean"
)
gate_race() (
  cd "$ROOT/gateway"
  SACP_POLICY_DIR="$ROOT/policy/cedar" go test -race ./...
)

# ---------------------------------------------------------------------------
printf '%sSACP local CI gate%s — reproducing every .github/workflows check\n' "$bold" "$rst"

run_gate "gofmt (CLAUDE.md)"                 gate_gofmt
run_gate "gateway-ci"                        gate_gateway_ci
run_gate "federation-boundary (ADR-006)"     gate_federation
run_gate "no-static-credentials (structural)" gate_static_cred_structural
run_gitleaks
run_dco
run_artifacts
if [ "${SACP_CI_SKIP_RACE:-0}" = "1" ]; then
  say "test-race (CLAUDE.md)"
  note_skip "test-race (CLAUDE.md)" "SACP_CI_SKIP_RACE=1"
else
  run_gate "test-race (CLAUDE.md)"           gate_race
fi

# --- summary -----------------------------------------------------------------
say "SUMMARY"
for i in "${!NAMES[@]}"; do
  v="${VERDICTS[$i]}"; c="$grn"
  [ "$v" = FAIL ] && c="$red"
  case "$v" in SKIP|skip) c="$ylw";; esac
  printf '  %-34s %s%s%s\n' "${NAMES[$i]}" "$c" "$v" "$rst"
done

if [ "$FAILED" -ne 0 ]; then
  printf '\n%s✗ CI-LOCAL FAILED%s — do not merge.\n' "$red" "$rst"
  exit 1
fi
if [ "$SKIPPED" -ne 0 ] && [ "${SACP_CI_ALLOW_SKIP:-0}" != "1" ]; then
  printf '\n%s✗ CI-LOCAL INCOMPLETE%s — a gate was SKIPPED, so this is weaker than CI.\n' "$ylw" "$rst"
  printf '   Install the tool it named, or re-run with SACP_CI_ALLOW_SKIP=1 to accept the gap.\n'
  exit 2
fi
printf '\n%s✓ ALL GATES PASSED%s — equivalent to green CI.\n' "$grn" "$rst"
