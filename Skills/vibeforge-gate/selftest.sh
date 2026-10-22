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

  # Inherited VF_GATE_* would poison every case (a VF_GATE_MANIFEST from an
  # enclosing gate run — e.g. under --pristine — would override each fixture's
  # manifest), so the runner starts from a scrubbed environment.
  local out rc=0
  out="$( cd "$repo" && env -u VF_GATE_MANIFEST -u VF_GATE_ALLOW_SKIP \
            VF_GATE_NO_COLOR=1 VF_GATE_CACHE="$repo/.cache" \
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

# --- a flag missing its value must error, never hang -------------------------
# (`shift 2` past the end of $@ shifts nothing, so without the guard the CLI
# loop respins on the same flag forever)
ok "--stage with no value -> exit 3" 3 \
'meta:
  name: t
checks:
  - id: ok
    run: true' \
"needs a value" --stage

ok "--only with no value -> exit 3" 3 \
'meta:
  name: t
checks:
  - id: ok
    run: true' \
"needs a value" --only

# --- fetched tools are verified against a pinned hash ------------------------
sha_ok="$(printf 'fetched-tool\n' | sha256sum -)"; sha_ok="${sha_ok%% *}"
ok "fetch with a matching fetch_sha256 -> exit 0" 0 \
"meta:
  name: t
checks:
  - id: fetched
    tool: not-on-path-fetch-xyz
    tool_version: \"1.0\"
    fetch: |
      printf 'fetched-tool\n' > \"\$VF_TOOL_DEST\"
    fetch_sha256: \"$sha_ok\"
    run: grep -q fetched-tool \"\$VF_TOOL\"" \
"GATE PASSED"

ok "fetch_sha256 mismatch is a blocking SKIP -> exit 2" 2 \
"meta:
  name: t
checks:
  - id: fetched
    tool: not-on-path-fetch-xyz
    tool_version: \"1.0\"
    fetch: |
      printf 'fetched-tool\n' > \"\$VF_TOOL_DEST\"
    fetch_sha256: \"0000000000000000000000000000000000000000000000000000000000000000\"
    run: true" \
"sha256"

# --- a hung check gets a verdict, not an eternity -----------------------------
ok "timeout kills a hung check -> exit 1" 1 \
'meta:
  name: t
checks:
  - id: hung
    timeout: 1
    run: sleep 30' \
"timed out after 1s"

ok "non-integer timeout -> exit 3" 3 \
'meta:
  name: t
checks:
  - id: bad
    timeout: 5m
    run: true' \
"timeout must be an integer"

# --- shadow mode with a deadline ----------------------------------------------
ok "enforce_after in the future stays advisory -> exit 0" 0 \
'meta:
  name: t
checks:
  - id: tuning
    enforce: false
    enforce_after: 2999-01-01
    run: exit 1' \
"advisory until 2999-01-01"

ok "enforce_after in the past blocks -> exit 1" 1 \
'meta:
  name: t
checks:
  - id: overdue
    enforce: false
    enforce_after: 2020-01-01
    run: exit 1' \
"shadow deadline 2020-01-01 has passed"

ok "enforce_after without enforce:false -> exit 3" 3 \
'meta:
  name: t
checks:
  - id: bad
    enforce_after: 2999-01-01
    run: true' \
"needs 'enforce: false'"

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

# --- --pristine checks HEAD, not the working tree -----------------------------
# Needs a committed manifest, so it cannot go through ok(): the check passes
# only in a clean worktree of HEAD, and the stray untracked file would fail it
# in the working tree.
prepo="$WORK/pristine"
mkdir -p "$prepo/.vibeforge"
( cd "$prepo" && git init -q . && git config user.email t@t && git config user.name t )
printf '%s\n' 'meta:
  name: t
checks:
  - id: clean-tree
    run: test ! -e stray.txt && echo no-stray-file' > "$prepo/.vibeforge/gates.yaml"
( cd "$prepo" && git add -A && git commit -qm init )
: > "$prepo/stray.txt"
out="$( cd "$prepo" && env -u VF_GATE_MANIFEST -u VF_GATE_ALLOW_SKIP \
          VF_GATE_NO_COLOR=1 VF_GATE_CACHE="$prepo/.cache" \
          bash "$RUNNER" --pristine 2>&1 )"; rc=$?
if [ "$rc" = 0 ] && grep -qF "no-stray-file" <<<"$out"; then
  printf '%s  ok  %s%s\n' "$grn" "$rst" "--pristine checks HEAD, not the working tree"; PASS=$((PASS + 1))
else
  printf '%s  FAIL%s %s %s(exit %s)%s\n' "$red" "$rst" "--pristine checks HEAD, not the working tree" "$dim" "$rc" "$rst"
  FAIL=$((FAIL + 1)); VERBOSE=1
fi
if [ "$VERBOSE" = 1 ]; then sed 's/^/      /' <<<"$out"; fi

# --- sign-off is matched as a git trailer, not as a line of text --------------
# Cannot go through ok(): needs a `main` branch with commits ahead of it, and the
# distinction under test is between two commit MESSAGES rather than two
# manifests. A Signed-off-by line sitting directly under the subject with no
# blank line before it is not a git trailer — `git interpret-trailers` does not
# see it — but `grep '^Signed-off-by:'` does. Matching with the regex therefore
# accepts a sign-off that git itself reports as absent, which is how one slipped
# through. This pins the distinction so the matcher cannot be "simplified" back.
drepo="$WORK/dco-trailer"
mkdir -p "$drepo/.vibeforge"
( cd "$drepo" && git init -q . && git symbolic-ref HEAD refs/heads/main \
    && git config user.email t@t && git config user.name t )
printf '%s\n' 'meta:
  name: t
checks:
  - id: dco
    run: |
      base="$(git merge-base HEAD main)"
      bad=0
      while read -r sha; do
        [ -z "$sha" ] && continue
        if ! git show -s --format="%(trailers:key=Signed-off-by)" "$sha" | grep -qE "<.+@.+>"; then
          echo "missing Signed-off-by: ${sha:0:8}"
          bad=1
        fi
      done < <(git rev-list --no-merges "$base..HEAD")
      [ "$bad" -eq 0 ] || exit 1
      echo "every commit is signed off"' > "$drepo/.vibeforge/gates.yaml"
( cd "$drepo" && git add -A && git commit -qm "base" && git checkout -q -b topic )

dco_case() {  # NAME COMMIT_MESSAGE EXPECTED_RC EXPECT_TEXT
  ( cd "$drepo" && git commit -q --allow-empty -m "$2" )
  local out rc=0
  out="$( cd "$drepo" && env -u VF_GATE_MANIFEST -u VF_GATE_ALLOW_SKIP \
            VF_GATE_NO_COLOR=1 VF_GATE_CACHE="$drepo/.cache" \
            bash "$RUNNER" 2>&1 )" || rc=$?
  if [ "$rc" = "$3" ] && grep -qF -- "$4" <<<"$out"; then
    printf '%s  ok  %s%s\n' "$grn" "$rst" "$1"; PASS=$((PASS + 1))
  else
    printf '%s  FAIL%s %s %s(exit %s, wanted %s)%s\n' "$red" "$rst" "$1" "$dim" "$rc" "$3" "$rst"
    FAIL=$((FAIL + 1)); VERBOSE=1
  fi
  [ "$VERBOSE" = 1 ] && sed 's/^/      /' <<<"$out"
  ( cd "$drepo" && git reset -q --hard HEAD~1 )
  return 0
}

dco_case "a real Signed-off-by trailer passes" \
  "$(printf 'feat: thing\n\nSigned-off-by: A B <a@b.com>')" 0 "every commit is signed off"
dco_case "Signed-off-by under the subject with no blank line is not a trailer" \
  "$(printf 'feat: thing\nSigned-off-by: A B <a@b.com>')" 1 "missing Signed-off-by"
dco_case "a trailer followed by more prose is not a trailer block" \
  "$(printf 'feat: thing\n\nSigned-off-by: A B <a@b.com>\n\nmore prose here')" 1 "missing Signed-off-by"

# --- outgoing commits: the gate checks what a push PUBLISHES -------------------
# A working-tree check misses a value committed in one commit and deleted in the
# next, and a gate keyed to HEAD misses `git push origin topic` run from main.
# These cases pin $VF_OUTGOING, the pre-push stdin plumbing, and the
# pre-receive computation — each through a real `git push`.
T="$HERE/skills/vibeforge-gate/templates"
check_case() {  # NAME WANT_RC OUTFILE TEXT
  local rc_file="$3.rc" rc; rc="$(cat "$rc_file")"
  if [ "$rc" = "$2" ] && grep -qF -- "$4" "$3"; then
    printf '%s  ok  %s%s\n' "$grn" "$rst" "$1"; PASS=$((PASS + 1))
  else
    printf '%s  FAIL%s %s %s(exit %s, wanted %s; text: %s)%s\n' "$red" "$rst" "$1" "$dim" "$rc" "$2" "$4" "$rst"
    FAIL=$((FAIL + 1)); VERBOSE=1
  fi
  [ "$VERBOSE" = 1 ] && sed 's/^/      /' "$3"
  return 0
}
run_to() {  # OUTFILE CMD...  — run, capture output and exit code
  local out="$1"; shift
  local rc=0
  "$@" > "$out" 2>&1 || rc=$?
  printf '%s\n' "$rc" > "$out.rc"
}
gate_env() { env -u VF_GATE_MANIFEST -u VF_GATE_ALLOW_SKIP -u VF_OUTGOING -u VF_OUTGOING_TIPS \
               -u VF_PUSH_UPDATES -u VF_PUSH_REMOTE VF_GATE_NO_COLOR=1 "$@"; }

OMAN='meta:
  name: t
checks:
  - id: count
    run: echo "outgoing-count=$(wc -l < "$VF_OUTGOING" | tr -d " ")"
  - id: planted
    run: |
      while read -r c; do git show --format= -p "$c"; done < "$VF_OUTGOING" > "$VF_OUTGOING.patch"
      m="PLANTED"; m="$m-MARKER"   # built at run time so this manifest never matches itself
      if grep -q "$m" "$VF_OUTGOING.patch"; then echo "planted value in an outgoing commit"; exit 1; fi
      echo clean-outgoing'

new_gated_repo() {  # DIR — a repo with the template runner, hooks and OMAN committed on main
  local d="$1"
  mkdir -p "$d/.vibeforge/hooks"
  git -C "$d" init -q -b main
  git -C "$d" config user.email t@t; git -C "$d" config user.name t
  cp "$T/gate.sh" "$d/.vibeforge/gate.sh"; cp "$T/hooks/pre-push" "$T/hooks/pre-commit" "$d/.vibeforge/hooks/"
  chmod +x "$d/.vibeforge/gate.sh" "$d/.vibeforge/hooks/"*
  printf '%s\n' "$OMAN" > "$d/.vibeforge/gates.yaml"
  git -C "$d" add -A; git -C "$d" commit -qm "gate"
}

orepo="$WORK/outgoing"; new_gated_repo "$orepo"
git init -q --bare "$WORK/outgoing-remote.git"
git -C "$orepo" remote add origin "$WORK/outgoing-remote.git"
git -C "$orepo" push -q origin main
( cd "$orepo" && echo a > a && git add a && git commit -qm a && echo b > b && git add b && git commit -qm b )
run_to "$WORK/o1" bash -c "cd '$orepo' && $(declare -f gate_env); gate_env VF_GATE_CACHE='$orepo/.c' bash .vibeforge/gate.sh"
check_case "outgoing = commits no remote-tracking ref has" 0 "$WORK/o1" "outgoing-count=2"

nrepo="$WORK/no-remote"; new_gated_repo "$nrepo"
( cd "$nrepo" && echo a > a && git add a && git commit -qm a )
run_to "$WORK/o2" bash -c "cd '$nrepo' && $(declare -f gate_env); gate_env VF_GATE_CACHE='$nrepo/.c' bash .vibeforge/gate.sh"
check_case "with no remote every commit is outgoing" 0 "$WORK/o2" "outgoing-count=2"

printf 'deadbeef\ncafef00d\nfeedface\n' > "$WORK/given-outgoing"
run_to "$WORK/o3" bash -c "cd '$nrepo' && $(declare -f gate_env); gate_env VF_GATE_CACHE='$nrepo/.c' VF_OUTGOING='$WORK/given-outgoing' bash .vibeforge/gate.sh --only count"
check_case "a VF_OUTGOING supplied by the caller is used as-is" 0 "$WORK/o3" "outgoing-count=3"

# the real pre-push path: push a branch that is NOT checked out, whose tip is clean
# but whose history adds and then deletes the planted value
# hooks are wired per push (-c core.hooksPath) so fixture commits stay ungated
HP="-c core.hooksPath=.vibeforge/hooks"
( cd "$orepo" && git push -q origin main 2>/dev/null; git checkout -q -b topic \
  && echo "PLANTED-MARKER" > leak.txt && git add leak.txt && git commit -qm "add" \
  && git rm -q leak.txt && git commit -qm "remove" && git checkout -q main )
run_to "$WORK/o4" bash -c "cd '$orepo' && $(declare -f gate_env); gate_env VF_GATE_CACHE='$orepo/.c' git $HP push origin topic"
check_case "pre-push gates the pushed ref, history included, not HEAD" 1 "$WORK/o4" "planted value in an outgoing commit"
if git --git-dir="$WORK/outgoing-remote.git" rev-parse -q --verify refs/heads/topic >/dev/null; then
  printf '%s  FAIL%s %s\n' "$red" "$rst" "the refused branch did not reach the remote"; FAIL=$((FAIL + 1))
else
  printf '%s  ok  %s%s\n' "$grn" "$rst" "the refused branch did not reach the remote"; PASS=$((PASS + 1))
fi
( cd "$orepo" && git checkout -q -b side main && echo s > s && git add s && git commit -qm side && git checkout -q main )
run_to "$WORK/o5" bash -c "cd '$orepo' && $(declare -f gate_env); gate_env VF_GATE_CACHE='$orepo/.c' git $HP push origin side"
check_case "pre-push passes a clean branch and counts only its new commit" 0 "$WORK/o5" "outgoing-count=1"
run_to "$WORK/o6" bash -c "cd '$orepo' && $(declare -f gate_env); gate_env VF_GATE_CACHE='$orepo/.c' git $HP push origin :side"
check_case "a deleted ref publishes nothing" 0 "$WORK/o6" "outgoing-count=0"

# the receiving end: pre-receive computes the new commits where the refs live
srv="$WORK/pr-server.git"; git init -q --bare "$srv"
cp "$T/hooks/pre-receive" "$srv/hooks/pre-receive"; chmod +x "$srv/hooks/pre-receive"
prepo="$WORK/pr-client"; new_gated_repo "$prepo"
git -C "$prepo" remote add origin "$srv"
run_to "$WORK/o7" bash -c "cd '$prepo' && $(declare -f gate_env); gate_env VF_GATE_CACHE='$prepo/.c' git push origin main"
check_case "pre-receive accepts a clean first push and sees its commits" 0 "$WORK/o7" "outgoing-count=1"
( cd "$prepo" && echo "PLANTED-MARKER" > leak.txt && git add leak.txt && git commit -qm add \
  && git rm -q leak.txt && git commit -qm remove )
run_to "$WORK/o8" bash -c "cd '$prepo' && $(declare -f gate_env); gate_env VF_GATE_CACHE='$prepo/.c' git push --no-verify origin main"
check_case "pre-receive refuses history the tip no longer shows" 1 "$WORK/o8" "planted value in an outgoing commit"

# --- catalog checks that read $VF_OUTGOING (the tool-free ones) ---------------
# The catalog files are valid manifests, so the real runner executes each block
# straight from the catalog. Every leak below is added and then deleted, so the
# tip tree is clean: these checks pass only if they read history, not the tree.
C="$HERE/skills/vibeforge-gate/catalog"
crepo="$WORK/catalog"; mkdir -p "$crepo/.vibeforge" "$WORK/home"
( cd "$crepo" && git init -q -b main && git config user.email "jane@corp.invalid" && git config user.name Jane \
  && echo base > README && git add -A && git commit -qm base \
  && printf 'host: 10.42.7.19\n' > deploy.yaml && echo "Initech rollout" > notes.md && echo "DB=x" > .env \
  && git add -A && git commit -qm add && git rm -q deploy.yaml notes.md .env && git commit -qm remove )
cat_run() {  # OUT CATALOG ID [ENV...]
  local out="$1" cat="$2" id="$3"; shift 3
  run_to "$out" bash -c "cd '$crepo' && $(declare -f gate_env); gate_env HOME='$WORK/home' VF_GATE_CACHE='$crepo/.c' VF_GATE_MANIFEST='$C/$cat' $* bash '$RUNNER' --only $id"
}
printf '@users\\.noreply\\.github\\.com$\n' > "$crepo/.vibeforge/allowed-identities"
cat_run "$WORK/c1" supply-chain.yaml identity-check
check_case "identity-check refuses an identity not on the list" 1 "$WORK/c1" "not in .vibeforge/allowed-identities: jane@corp.invalid"
printf '@corp\\.invalid$\n' >> "$crepo/.vibeforge/allowed-identities"
cat_run "$WORK/c2" supply-chain.yaml identity-check
check_case "identity-check passes once the identity is listed" 0 "$WORK/c2" "every outgoing identity is allowed"
cat_run "$WORK/c3" universal.yaml outgoing-files
check_case "outgoing-files catches a .env added and deleted in the push" 1 "$WORK/c3" "credential-bearing file name: .env"
cat_run "$WORK/c4" supply-chain.yaml denylist-scan
check_case "denylist-scan with no list is a blocking SKIP" 2 "$WORK/c4" "probe failed"
cat_run "$WORK/c5" supply-chain.yaml denylist-scan VF_LEAK_DENYLIST_PATTERNS=initech
check_case "denylist-scan catches a term the tip no longer shows" 1 "$WORK/c5" "match the private deny list"
echo initech > "$crepo/denylist-inside"
cat_run "$WORK/c6" supply-chain.yaml denylist-scan VF_LEAK_DENYLIST="$crepo/denylist-inside"
check_case "denylist-scan refuses a list kept inside the repo" 1 "$WORK/c6" "is inside the repository"
rm -f "$crepo/denylist-inside"
cat_run "$WORK/c7" supply-chain.yaml leak-scan
check_case "leak-scan reports a private IP from history (advisory)" 0 "$WORK/c7" "deploy.yaml:host: 10.42.7.19"

# --- summary ------------------------------------------------------------------
printf '\n%s passed, %s failed\n' "$PASS" "$FAIL"
[ "$FAIL" -eq 0 ] || exit 1
printf '%sruntime contract holds%s\n' "$grn" "$rst"
