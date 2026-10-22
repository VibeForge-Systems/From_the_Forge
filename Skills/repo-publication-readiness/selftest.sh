#!/usr/bin/env bash
# SPDX-License-Identifier: Apache-2.0
#
# selftest.sh — prove the publication-readiness scripts find what they claim
# to find, and that a verified rewrite really removes it.
#
# Builds a throwaway repository with one planted example of every leak class
# the skill names, then asserts:
#   * sweep-leaks.py reports each class, including the two places history
#     scanners miss (a merge-resolution-only line; a tag message);
#   * a real `git filter-repo --sensitive-data-removal` rewrite, checked by
#     verify-rewrite.sh, passes — and the same check FAILS when old history is
#     smuggled back in;
#   * the sweep is clean on the rewritten repository.
#
# Credential-shaped strings are generated at run time, so no literal that looks
# like a secret is ever committed to the repository that ships this file.
#
#   ./selftest.sh          scanner-independent cases (git, python3 >= 3.11, git-filter-repo)
#   ./selftest.sh -v       with each command's output
#   RPR_SELFTEST_SCANNERS=1 ./selftest.sh
#                          also run scan-secrets.sh end to end (gitleaks >= 8.19, trufflehog v3)

set -uo pipefail
HERE="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
SCRIPTS="$HERE/skills/repo-publication-readiness/scripts"
VERBOSE=0; [ "${1:-}" = "-v" ] && VERBOSE=1
grn=$'\033[32m'; red=$'\033[31m'; ylw=$'\033[33m'; rst=$'\033[0m'
[ -t 1 ] || { grn=""; red=""; ylw=""; rst=""; }
PASS=0 FAIL=0 NOTRUN=0
W="$(mktemp -d)"
# KEEP=1 leaves the fixtures behind for inspection
trap 'if [ "${KEEP:-0}" = 1 ]; then echo "kept: $W"; else rm -rf "$W"; fi' EXIT

ok()  { printf '%s  ok  %s%s\n' "$grn" "$rst" "$1"; PASS=$((PASS + 1)); }
bad() { printf '%s  FAIL%s %s\n' "$red" "$rst" "$1"; FAIL=$((FAIL + 1)); VERBOSE=1; }
show() { [ "$VERBOSE" = 1 ] && sed 's/^/      /' "$1"; return 0; }
expect_rc() {  # NAME WANT_RC LOGFILE -- CMD...
  local name="$1" want="$2" log="$3"; shift 4
  "$@" > "$log" 2>&1; local rc=$?
  if [ "$rc" = "$want" ]; then ok "$name"; else bad "$name (exit $rc, wanted $want)"; fi
  show "$log"
}
has() {  # NAME FILE TEXT
  if grep -qF -- "$3" "$2"; then ok "$1"; else bad "$1 (missing: $3)"; show "$2"; fi
}

for t in git python3 git-filter-repo; do
  command -v "$t" >/dev/null 2>&1 || { echo "selftest needs $t on PATH (git-filter-repo: pip install git-filter-repo)"; exit 2; }
done
python3 -c 'import tomllib' 2>/dev/null || { echo "selftest needs python3 >= 3.11"; exit 2; }

printf 'repo-publication-readiness selftest\n\n'

# --- build the fixture -----------------------------------------------------------
F="$W/private"; mkdir -p "$F"
gitf() { git -C "$F" "$@"; }
gitf init -q -b main
gitf config user.name "Jane Doe"; gitf config user.email "jdoe@initech-corp.com"
gitf config commit.gpgsign false; gitf config tag.gpgsign false

rand() { LC_ALL=C tr -dc "$1" < /dev/urandom | head -c "$2"; }
AKID="AKIA$(rand 'A-Z2-7' 16)"
GHTOK="ghp_$(rand 'A-Za-z0-9' 36)"

printf '# Demo\nBuilt for Initech. Ask jdoe@initech-corp.com.\n' > "$F/README.md"
printf 'deploy:\n  host: 10.42.7.19\n  mirror: build01.corp\n' > "$F/deploy.yaml"
printf 'copy C:\\Users\\jdoe\\keys\\deploy.pem to the server\n' > "$F/notes.txt"
gitf add -A; gitf commit -qm "initial import"

printf 'AWS_ACCESS_KEY_ID=%s\nGITHUB_TOKEN=%s\n' "$AKID" "$GHTOK" > "$F/config.env"
printf 'DB_PASSWORD=hunter2\n' > "$F/.env"
gitf add -A; gitf commit -qm "add config"
gitf rm -q config.env .env; gitf commit -qm "remove config (still in history)"

cat > "$F/analysis.ipynb" <<'EOF'
{"cells":[{"cell_type":"code","source":["print(1)"],"outputs":[{"output_type":"stream","name":"stdout","text":["1\n"]}],"metadata":{},"execution_count":1}],"metadata":{},"nbformat":4,"nbformat_minor":5}
EOF
head -c 1572864 /dev/urandom > "$F/blob.bin"
gitf add -A; gitf commit -qm "add analysis" -m "Co-authored-by: Pat Lee <plee@initech-corp.com>"
gitf commit -q --allow-empty -m "backup lives on nas.lan now"

# a line that exists only in a merge resolution
printf 'endpoint: a\n' > "$F/svc.yaml"; gitf add -A; gitf commit -qm "svc"
gitf checkout -q -b left;  printf 'endpoint: left\n'  > "$F/svc.yaml"; gitf commit -qam "left"
gitf checkout -q main;     printf 'endpoint: right\n' > "$F/svc.yaml"; gitf commit -qam "right"
gitf merge -q left >/dev/null 2>&1 || true
printf 'endpoint: 172.16.99.5\n' > "$F/svc.yaml"; gitf add svc.yaml
gitf -c core.editor=true commit -q --no-edit
MERGE="$(gitf rev-parse HEAD)"
gitf tag -a v0.1 -m "release candidate staged on vault.internal"

OLD="$W/old.git"; git clone -q --mirror "$F" "$OLD"
mkdir -p "$W/private-config"
printf '# private: never inside the repo\ninitech\n' > "$W/private-config/denylist"
printf 'noreply@example\\.org$\n' > "$W/private-config/identities"

# --- 1. the sweep finds every planted class ------------------------------------
echo "sweep-leaks.py on the fixture"
expect_rc "sweep exits 1 on findings" 1 "$W/sweep1.log" -- \
  python3 "$SCRIPTS/sweep-leaks.py" --repo "$OLD" --denylist "$W/private-config/denylist" \
          --identities "$W/private-config/identities" --max-blob-mb 1 --json "$W/sweep1.json"
for want in "[identity]" "jdoe@initech-corp.com" "plee@initech-corp.com" \
            "[private-ip]" "10.42.7.19" "[internal-host]" "build01.corp" \
            "[local-path]" '\Users\jdoe' "[denylist]" "Initech" \
            "[sensitive-file]" ".env" "config.env" "[notebook-output]" "[large-blob]" "[email]"; do
  has "sweep reports $want" "$W/sweep1.log" "$want"
done
has "merge-only line is found and attributed to the merge" "$W/sweep1.log" "172.16.99.5  —  svc.yaml @ ${MERGE:0:10}"
has "commit message is swept" "$W/sweep1.log" "nas.lan  —  (commit message)"
has "tag message is swept" "$W/sweep1.log" "vault.internal  —  (tag message) refs/tags/v0.1"

expect_rc "a deny list inside the repo is refused" 3 "$W/sweep-deny-inside.log" -- \
  bash -c "cp '$W/private-config/denylist' '$F/denylist' && python3 '$SCRIPTS/sweep-leaks.py' --repo '$F' --denylist '$F/denylist'"
rm -f "$F/denylist"
expect_rc "a report path inside the repo is refused" 3 "$W/sweep-json-inside.log" -- \
  python3 "$SCRIPTS/sweep-leaks.py" --repo "$F" --json "$F/report.json"

# one deny list serves both tools: the gate's location and inline patterns count too
mkdir -p "$W/home/.config/vibeforge" "$W/empty-home"
printf 'initech\n' > "$W/home/.config/vibeforge/leak-denylist"
env -u RPR_DENYLIST -u VF_LEAK_DENYLIST -u VF_LEAK_DENYLIST_PATTERNS HOME="$W/home" \
  python3 "$SCRIPTS/sweep-leaks.py" --repo "$OLD" > "$W/sweep-gateloc.log" 2>&1
has "sweep reads vibeforge-gate's deny list location" "$W/sweep-gateloc.log" "Initech  —  README.md"
env -u RPR_DENYLIST -u VF_LEAK_DENYLIST HOME="$W/empty-home" VF_LEAK_DENYLIST_PATTERNS=initech \
  python3 "$SCRIPTS/sweep-leaks.py" --repo "$OLD" > "$W/sweep-inline.log" 2>&1
has "sweep reads VF_LEAK_DENYLIST_PATTERNS" "$W/sweep-inline.log" "Initech  —  README.md"

# the shipped example deny list
EX="$HERE/skills/repo-publication-readiness/templates/leak-denylist.example"
expect_rc "the example deny list compiles and never matches its own text" 0 "$W/example.log" -- \
  python3 -c '
import re, sys
text = open(sys.argv[1]).read()
pats = [re.compile(l.strip(), re.I) for l in text.splitlines() if l.strip() and not l.strip().startswith("#")]
assert len(pats) >= 15, f"only {len(pats)} active patterns"
hits = [p.pattern for p in pats for line in text.splitlines() if p.search(line)]
sys.exit(f"self-matching: {hits}") if hits else print(f"{len(pats)} patterns, no self-match")' "$EX"
T="$W/tenant"; mkdir -p "$T"; git -C "$T" init -q -b main
git -C "$T" config user.email t@t; git -C "$T" config user.name t; git -C "$T" config commit.gpgsign false
printf 'instance: https://acmeprod.%s.com/nav_to.do\n' "service-now" > "$T/notes.md"
git -C "$T" add -A; git -C "$T" commit -qm add; git -C "$T" rm -q notes.md; git -C "$T" commit -qm remove
python3 "$SCRIPTS/sweep-leaks.py" --repo "$T" --denylist "$EX" > "$W/sweep-example.log" 2>&1
has "the example catches a SaaS tenant hostname the tip no longer shows" "$W/sweep-example.log" "[denylist]"

cat > "$W/allow.toml" <<'EOF'
[[allow]]
category = "large-blob"
path = "blob.bin"
reason = "selftest: accepted on purpose"
EOF
python3 "$SCRIPTS/sweep-leaks.py" --repo "$OLD" --max-blob-mb 1 --allow "$W/allow.toml" > "$W/sweep-allow.log" 2>&1
if grep -qF "[large-blob]" "$W/sweep-allow.log"; then bad "allow file suppresses an accepted finding"; show "$W/sweep-allow.log"
else ok "allow file suppresses an accepted finding"; fi

# --- 2. a real rewrite, then verification ------------------------------------------
echo; echo "git filter-repo --sensitive-data-removal, then verify-rewrite.sh"
NEW="$W/rewrite"; git clone -q --no-local "$OLD" "$NEW"
cat > "$W/expressions.txt" <<EOF
$AKID==>REMOVED-AWS-KEY
$GHTOK==>REMOVED-GITHUB-TOKEN
regex:(?i)initech-corp\.com==>example.org
regex:(?i)initech==>ExampleCo
10.42.7.19==>192.0.2.10
172.16.99.5==>192.0.2.20
build01.corp==>build.example.org
nas.lan==>backup.example.org
vault.internal==>vault.example.org
C:\Users\jdoe\keys==>C:\Users\USERNAME\keys
EOF
cat > "$W/mailmap" <<'EOF'
Jane Doe <noreply@example.org> <jdoe@initech-corp.com>
Pat Lee <noreply@example.org> <plee@initech-corp.com>
EOF
( cd "$NEW" && git filter-repo --sensitive-data-removal --force \
    --invert-paths --path .env --path config.env --path analysis.ipynb --path blob.bin \
    --replace-text "$W/expressions.txt" --replace-message "$W/expressions.txt" \
    --mailmap "$W/mailmap" ) > "$W/filter-repo.log" 2>&1
if [ $? -eq 0 ]; then ok "filter-repo rewrite succeeded"; else bad "filter-repo rewrite failed"; show "$W/filter-repo.log"; fi
has "filter-repo recorded first-changed commits" "$W/filter-repo.log" "First Changed Commit"
( cd "$NEW" && git reflog expire --expire=now --all && git gc -q --prune=now ) >/dev/null 2>&1

expect_rc "verify-rewrite passes the clean rewrite" 0 "$W/verify-ok.log" -- \
  "$SCRIPTS/verify-rewrite.sh" --old "$OLD" --new "$NEW" --identities "$W/private-config/identities" --out "$W/verify-ok" --no-scan
has "verify-rewrite confirms old commits are absent" "$W/verify-ok.log" "rewritten-away commits are absent"

expect_rc "sweep is clean on the rewritten repo" 0 "$W/sweep2.log" -- \
  python3 "$SCRIPTS/sweep-leaks.py" --repo "$NEW" --denylist "$W/private-config/denylist" \
          --identities "$W/private-config/identities" --max-blob-mb 1

BAD="$W/smuggled"; git clone -q --no-local "$NEW" "$BAD"
cp -r "$NEW/.git/filter-repo" "$BAD/.git/filter-repo"
git -C "$BAD" fetch -q "$OLD" "refs/heads/main:refs/heads/old-main"
expect_rc "verify-rewrite FAILS when old history is fetched back in" 1 "$W/verify-bad.log" -- \
  "$SCRIPTS/verify-rewrite.sh" --old "$OLD" --new "$BAD" --identities "$W/private-config/identities" --out "$W/verify-bad" --no-scan
has "the failure names the surviving commits" "$W/verify-bad.log" "old commit(s) still present"

git -C "$BAD" update-ref refs/original/refs/heads/main "$(git -C "$BAD" rev-parse main)"
"$SCRIPTS/verify-rewrite.sh" --old "$OLD" --new "$BAD" --out "$W/verify-bad2" --no-scan > "$W/verify-bad2.log" 2>&1
has "a leftover refs/original is a FAIL" "$W/verify-bad2.log" "leftover ref(s)"

# --- 3. inventory -----------------------------------------------------------------
echo; echo "inventory.sh"
expect_rc "inventory runs on a mirror" 0 "$W/inv.log" -- "$SCRIPTS/inventory.sh" --repo "$OLD" --out "$W/inv"
has "inventory lists identities" "$W/inv/inventory.txt" "jdoe@initech-corp.com"
has "inventory counts merges" "$W/inv/inventory.txt" "merges:             1"
expect_rc "inventory refuses an --out inside the repo" 3 "$W/inv-bad.log" -- "$SCRIPTS/inventory.sh" --repo "$F" --out "$F/inv"

# --- 4. scanners end to end (opt-in: needs gitleaks >= 8.19 and trufflehog v3) -----
echo; echo "scan-secrets.sh"
expect_rc "scan-secrets refuses a report dir inside the repo" 3 "$W/scan-inside.log" -- \
  "$SCRIPTS/scan-secrets.sh" --repo "$F" --out "$F/reports"
if [ "${RPR_SELFTEST_SCANNERS:-0}" = 1 ]; then
  expect_rc "scan-secrets finds the deleted credentials in history" 1 "$W/scan-old.log" -- \
    "$SCRIPTS/scan-secrets.sh" --repo "$OLD" --out "$W/scan-old" --no-verification
  has "gitleaks history pass reports findings" "$W/scan-old.log" "history: see"
  expect_rc "scan-secrets is clean after the rewrite" 0 "$W/scan-new.log" -- \
    "$SCRIPTS/scan-secrets.sh" --repo "$NEW" --out "$W/scan-new" --no-verification --trust
  # a credential that exists ONLY in a merge resolution: invisible to a plain history scan
  M="$W/merge-only"; mkdir -p "$M"; git -C "$M" init -q -b main
  git -C "$M" config user.email t@t; git -C "$M" config user.name t; git -C "$M" config commit.gpgsign false
  echo "v: base" > "$M/c.yaml"; git -C "$M" add -A; git -C "$M" commit -qm base
  git -C "$M" checkout -q -b side; echo "v: side" > "$M/c.yaml"; git -C "$M" commit -qam side
  git -C "$M" checkout -q main;    echo "v: main" > "$M/c.yaml"; git -C "$M" commit -qam main
  git -C "$M" merge -q side >/dev/null 2>&1 || true
  printf 'v: ghp_%s\n' "$(rand 'A-Za-z0-9' 36)" > "$M/c.yaml"; git -C "$M" add c.yaml
  git -C "$M" -c core.editor=true commit -q --no-edit
  echo "v: clean" > "$M/c.yaml"; git -C "$M" commit -qam clean
  expect_rc "scan-secrets finds a credential that exists only in a merge resolution" 1 "$W/scan-merge.log" -- \
    "$SCRIPTS/scan-secrets.sh" --repo "$M" --out "$W/scan-merge" --no-verification --trust
  has "the finding comes from the merge pass" "$W/scan-merge.log" "merge resolutions: see"
else
  printf '%s  NOT RUN%s scanner end-to-end cases (set RPR_SELFTEST_SCANNERS=1 with gitleaks >= 8.19 and trufflehog on PATH)\n' "$ylw" "$rst"
  NOTRUN=1
fi

printf '\n%s passed, %s failed\n' "$PASS" "$FAIL"
[ "$FAIL" -eq 0 ] || exit 1
[ "$NOTRUN" -eq 0 ] && printf '%sall cases ran and passed%s\n' "$grn" "$rst" \
  || printf 'scanner-independent cases passed; scanner cases NOT RUN (not a pass for them)\n'
exit 0
