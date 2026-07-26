#!/usr/bin/env bash
#
# selftest.sh — prove the runner's contract, don't just claim it.
#
# The whole value of this package is that a green run means every declared check
# actually executed. That property lives in the runner's exit-code and SKIP
# semantics, so those semantics get tested rather than documented and hoped for.
#
# Each case builds a throwaway repo with a fixture manifest, runs the real
# runner against it, and asserts the exit code and the verdict text.
#
#   ./selftest.sh          run every case
#   ./selftest.sh -v       also print the runner output for each case

set -uo pipefail

HERE="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
RUNNER="$HERE/skills/vibeforge-gate/templates/gate.sh"
VERBOSE=0
[ "${1:-}" = "-v" ] && VERBOSE=1

grn=$'\033[32m'; red=$'\033[31m'; dim=$'\033[2m'; rst=$'\033[0m'
[ -t 1 ] || { grn=""; red=""; dim=""; rst=""; }

PASS=0; FAIL=0
WORK="$(mktemp -d)"
trap 'rm -rf "$WORK"' EXIT

# ok NAME EXPECTED_RC MANIFEST_BODY [EXPECT_TEXT] [KEY=VAL... | --flag...]
# Runs the runner against MANIFEST_BODY in a fresh repo; asserts the exit code
# and, when given, that EXPECT_TEXT appears in the output. Trailing arguments
# beginning with '-' are passed to the runner; the rest are environment
# assignments. Use the --flag=value form so a flag's value is never mistaken
# for an env assignment.
ok() {
  local name="$1" want_rc="$2" body="$3" want_txt="${4:-}"; shift 4 2>/dev/null || shift $#
  local -a envs=() args=() a
  for a in "$@"; do
    case "$a" in -*) args+=("$a") ;; *) envs+=("$a") ;; esac
  done

  local slug repo
  slug="$(printf '%s' "$name" | tr -c 'a-zA-Z0-9' '-')"
  repo="$WORK/$slug"
  mkdir -p "$repo/.vibeforge"
  ( cd "$repo" && git init -q . && git config user.email t@t && git config user.name t )
  printf '%s\n' "$body" > "$repo/.vibeforge/gates.yaml"

  local out rc=0
  out="$( cd "$repo" && env VF_GATE_NO_COLOR=1 VF_GATE_CACHE="$repo/.cache" \
            ${envs[@]+"${envs[@]}"} bash "$RUNNER" ${args[@]+"${args[@]}"} 2>&1 )" || rc=$?

  local bad=""
  [ "$rc" = "$want_rc" ] || bad="exit $rc, wanted $want_rc"
  if [ -n "$want_txt" ] && ! grep -qF -- "$want_txt" <<<"$out"; then
    bad="${bad:+$bad; }missing expected text: $want_txt"
  fi

  if [ -z "$bad" ]; then
    printf '%s  ok  %s%s\n' "$grn" "$rst" "$name"; PASS=$((PASS + 1))
  else
    printf '%s  FAIL%s %s %s(%s)%s\n' "$red" "$rst" "$name" "$dim" "$bad" "$rst"; FAIL=$((FAIL + 1))
    VERBOSE=1
  fi
  [ "$VERBOSE" = 1 ] && sed 's/^/      /' <<<"$out"
  return 0
}

printf 'vibeforge-gate selftest\n\n'

# --- the happy path -----------------------------------------------------------
ok "all checks pass -> exit 0" 0 \
'meta:
  name: t
checks:
  - id: yes
    desc: trivially true
    run: true' \
"GATE PASSED"

# --- a failure blocks ---------------------------------------------------------
ok "a failing check -> exit 1" 1 \
'meta:
  name: t
checks:
  - id: nope
    run: exit 1' \
"GATE FAILED"

# --- the load-bearing rule: a missing tool is NOT a pass ----------------------
ok "missing tool SKIPs and blocks -> exit 2" 2 \
'meta:
  name: t
checks:
  - id: needs-absent-tool
    needs: definitely-not-installed-xyz
    install: "apt install nothing"
    run: true' \
"GATE INCOMPLETE"

ok "SKIP names the missing tool and its install hint" 2 \
'meta:
  name: t
checks:
  - id: needs-absent-tool
    needs: definitely-not-installed-xyz
    install: "apt install nothing"
    run: true' \
"apt install nothing"

# --- the gap can be accepted, but only explicitly ----------------------------
ok "VF_GATE_ALLOW_SKIP=1 accepts the gap -> exit 0" 0 \
'meta:
  name: t
checks:
  - id: needs-absent-tool
    needs: definitely-not-installed-xyz
    run: true' \
"GATE PASSED" VF_GATE_ALLOW_SKIP=1

# --- a version pin that is not met is also a coverage gap --------------------
mkdir -p "$WORK/fakebin"
printf '#!/bin/sh\necho "faketool 1.0.0"\n' > "$WORK/fakebin/faketool"
chmod +x "$WORK/fakebin/faketool"
ok "wrong tool version SKIPs and blocks -> exit 2" 2 \
'meta:
  name: t
checks:
  - id: pinned
    tool: faketool
    tool_version: "9.9.9"
    version_cmd: faketool --version
    run: "$VF_TOOL"' \
"is not the pinned v9.9.9" PATH="$WORK/fakebin:$PATH"

ok "matching tool version runs the check -> exit 0" 0 \
'meta:
  name: t
checks:
  - id: pinned
    tool: faketool
    tool_version: "1.0.0"
    version_cmd: faketool --version
    run: "$VF_TOOL"' \
"PASS" PATH="$WORK/fakebin:$PATH"

# --- a probe failure is a gap too --------------------------------------------
ok "failing probe SKIPs and blocks -> exit 2" 2 \
'meta:
  name: t
checks:
  - id: needs-lib
    probe: python3 -c "import definitely_not_a_module_xyz"
    run: true' \
"probe failed"

# --- a skip the operator asked for does NOT block ----------------------------
ok "deliberate skip_env does not block -> exit 0" 0 \
'meta:
  name: t
checks:
  - id: slow
    skip_env: SKIP_SLOW
    run: exit 1' \
"deliberate" SKIP_SLOW=1

# --- shadow mode: advisory checks report but never block ---------------------
ok "enforce:false failure is advisory -> exit 0" 0 \
'meta:
  name: t
checks:
  - id: tuning
    enforce: false
    run: exit 1' \
"WARN"

# --- stage selection ----------------------------------------------------------
ok "--stage commit excludes push checks -> exit 0" 0 \
'meta:
  name: t
  default_stage: push
checks:
  - id: fast
    stage: commit
    run: echo fast-ran
  - id: slow
    stage: push
    run: exit 1' \
"fast-ran" --stage=commit

ok "push stage includes commit checks" 1 \
'meta:
  name: t
checks:
  - id: fast
    stage: commit
    run: echo fast-ran
  - id: slow
    stage: push
    run: exit 1' \
"fast-ran"

ok "manual checks are excluded from the default bar" 0 \
'meta:
  name: t
checks:
  - id: ok
    stage: push
    run: true
  - id: heavy
    stage: manual
    run: exit 1' \
"GATE PASSED"

# --- block scalars must survive parsing byte-for-byte ------------------------
ok "multi-line run block keeps quoting, pipes and #" 0 \
'meta:
  name: t
checks:
  - id: fidelity
    run: |
      v="a b  c"        # inline hash must not be stripped
      printf "%s\n" "$v" | grep -q "a b  c"
      case "$v" in *"b  c") echo matched ;; esac' \
"matched"

# --- a manifest the parser cannot represent must error, never guess ----------
ok "unparseable manifest -> exit 3" 3 \
'meta:
  name: t
checks:
  - id: bad
      weird_indent: 1
    run: true' \
"line"

ok "duplicate check id -> exit 3" 3 \
'meta:
  name: t
checks:
  - id: same
    run: true
  - id: same
    run: true' \
"duplicate check id"

ok "check with no run -> exit 3" 3 \
'meta:
  name: t
checks:
  - id: empty
    desc: nothing to run' \
"has no 'run'"

# --- working directory --------------------------------------------------------
ok "dir: scopes the check to a subdirectory" 0 \
'meta:
  name: t
checks:
  - id: subdir
    dir: .vibeforge
    run: test -f gates.yaml && echo in-subdir' \
"in-subdir"

ok "missing dir SKIPs and blocks -> exit 2" 2 \
'meta:
  name: t
checks:
  - id: nowhere
    dir: no/such/place
    run: true' \
"does not exist"

# --- summary ------------------------------------------------------------------
printf '\n%s passed, %s failed\n' "$PASS" "$FAIL"
[ "$FAIL" -eq 0 ] || exit 1
printf '%sruntime contract holds%s\n' "$grn" "$rst"
