# Equivalence audit — `gates.yaml` vs. the hand-written `ci-local.sh`

This directory is the extraction's proof of work. `reference-ci-local.sh` is a
verbatim copy of the hand-written gate this package was extracted from — a
238-line bash script, purpose-built for one Go repository, that reproduced five
hosted-CI workflows plus two rules that repo's contributor docs mandated.
`gates.yaml` is that same merge bar re-expressed as a manifest for the generic
runner.

Both were run against the same working tree of that repository, on the same
machine, on the same commit. This file records what matched, what did not, and
why.

## Result

| | hand-written `ci-local.sh` | `gates.yaml` + `gate.sh` |
|---|---|---|
| Exit code | 0 | 0 |
| Checks passed | 7 | 8 |
| Deliberate skips | 1 (`test-race`) | 1 (`test-race`) |
| Blocking skips | 0 | 0 |

The check-count difference is one deliberate split, itemized below. Every
command executed, and every pinned version, is identical.

## Check-by-check

| Hand-written gate | Manifest check | Command | Verdict |
|---|---|---|---|
| `gofmt (CLAUDE.md)` | `gofmt` | `gofmt -l internal/ cmd/`, non-empty output fails | identical |
| `gateway-ci` | `gateway-build` | `go mod verify`, `go build -mod=readonly ./...`, `go vet ./...`, `go test -mod=readonly ./...` | identical |
| `federation-boundary (ADR-006)` | `federation-boundary` | `archcheck.sh reverse-import`, `archcheck.sh decision-point` | identical |
| `no-static-credentials (structural)` | `no-static-credentials` | `go test -tags=architecture ./internal/identity/... -run 'NoStaticCredential\|Unrepresentable'` | identical |
| `gitleaks` | `secret-scan` | `gitleaks detect --source <root> --no-git --config .gitleaks.toml --redact --no-banner`, pinned 8.30.1 | identical command + pin (see difference 2) |
| `dco-check` | `dco` | merge-base against `origin/main`, `git rev-list --no-merges`, `[bot]` authors exempt, `Signed-off-by` regex | identical logic, transcribed |
| `artifact-validation` | `artifact-schemas` + `cedar-policy` | `validate_schemas.py`, `verify_hash_chain.py`, `check_cedar.py` | same three commands, split into two checks (difference 1) |
| `test-race (CLAUDE.md)` | `test-race` | `SACP_POLICY_DIR=<root>/policy/cedar go test -race ./...`, skippable via `SACP_CI_SKIP_RACE=1` | identical, same env-var name |

The `-race` env var kept its original name deliberately: the point of the
extraction is that the people already using this gate do not have to relearn it.

## The four differences, and why each one exists

### 1. `artifact-validation` is split into `artifact-schemas` and `cedar-policy`

The original ran three Python scripts under one gate name, with the Cedar CLI
check nested inside. That nesting produced a genuinely confusing state: when
`cedar` was absent, the gate recorded **both** a `SKIP` for `check_cedar` **and**
a `FAIL` for `artifact-validation` (the `ok=0` on line 178 of
`reference-ci-local.sh` is set before the skip is recorded), so a missing
optional tool was reported as a *failure* of a gate whose other two checks had
passed.

Splitting them makes each report its own honest verdict: schemas pass, cedar
skips. This is a **behavioral difference in the missing-tool case only** —
outcome `1` (fail) becomes outcome `2` (incomplete). Both are non-zero and both
block. The manifest's version is the more accurate description of what happened.

### 2. The version pin is enforced, not just documented

`reference-ci-local.sh` prefers any `gitleaks` on `PATH` and only falls back to
downloading the pinned build (`find_or_fetch_gitleaks`, line 106). So a machine
with gitleaks 8.18 installed would run **8.18**, silently, while the script's
header and the hosted workflow both said 8.30.1 — the local gate would not be
the gate anyone reviewed.

The runner resolves a pinned tool as: cached pinned build → a `PATH` copy *that
reports the pinned version* → fetch the pinned build → SKIP. A version mismatch
is a coverage gap, not a pass. This is stricter than the original; the selftest
case `wrong tool version SKIPs and blocks` covers it.

On this machine both resolve to the same 8.30.1 binary, so the observed runs are
identical.

### 3. Stages exist

The original ran one set, all at once, at pre-push. The manifest marks `gofmt`,
`federation-boundary` and `no-static-credentials` as `stage: commit` — fast,
always-on — and everything else `stage: push`. Because `--stage push` includes
commit checks, **the full bar is unchanged**; the split only adds a faster
pre-commit subset that did not exist before.

### 4. `PATH` handling is declared rather than hardcoded

The original hardcodes `export PATH="$HOME/.cargo/bin:$PATH"` (line 38) so the
cargo-installed `cedar` is found. The manifest states the same thing as data
(`meta.path_prepend: ~/.cargo/bin`). Same effect, and it survives being copied
to a repo where the relevant directory is `./node_modules/.bin` instead.

This one mattered in practice: on the validation machine `cedar` was **not** on
the default `PATH`, and `command -v cedar` reported it missing. `path_prepend`
found it at `~/.cargo/bin/cedar`, and the version check confirmed
`cedar-policy-cli 4.11.2` against the pin.

## What this audit does not establish

- **That either gate matches what the hosted CI service would have done.** Both
  reproduce the *commands* in the workflow files. Neither reproduces the runner
  OS, the toolchain versions the service would have provisioned, the
  `paths:`-filter logic that decides whether a workflow runs at all, or the
  `pull_request`-event context the DCO job used (it read PR commit metadata
  through the forge API; both local versions approximate it with the commits on
  the branch, which is the same set in normal use but not by construction).
- **That the manifest declares the right checks.** It declares the same ones the
  predecessor did. Whether that set is sufficient is a separate judgment, and
  one the manifest makes easier to revisit because each check now carries its
  own `why:`.
- **That this generalizes without thought.** One Go repository was migrated. The
  runner is stack-agnostic and the selftest pins its semantics, but the *mapping*
  from an existing CI config to a manifest is a reading task, not a mechanical
  translation. See `references/importing-existing-ci.md`.

## Reproducing this

```sh
# the predecessor
cd <repo> && SACP_CI_SKIP_RACE=1 bash scripts/ci-local.sh

# the manifest-driven gate, same tree
cd <repo> && SACP_CI_SKIP_RACE=1 VF_GATE_MANIFEST=<this-dir>/gates.yaml \
  bash <package>/skills/vibeforge-gate/templates/gate.sh
```
