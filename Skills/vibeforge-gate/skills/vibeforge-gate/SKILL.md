---
name: vibeforge-gate
description: Use this skill to stand up, extend, or run a repository-owned merge bar — a local pre-commit/pre-push gate that runs the checks a hosted CI service would have run, without depending on one. Trigger when the user says CI is unavailable, unfunded, out of minutes, disabled, or too slow; when they want checks to run before a commit, push, merge, or PR; when they ask to add a check (lint, format, tests, secret scan, SAST, dependency/vulnerability scan, license or SBOM check) to a local gate; when they want to migrate off hosted CI or replicate gates that sit behind a paid tier; when they mention a pre-commit or pre-push hook, a "merge bar", or "green locally means green"; and when a gate reports SKIP, a tool version pin drifts, or a check needs to run in shadow/advisory mode. Also use it to audit an existing local gate for silent coverage gaps. Do not use it to author hosted CI provider config, or for one-off ad-hoc test runs that are not part of a standing gate.
compatibility: Needs bash 4+, coreutils and git in the target repo. No YAML library, language runtime, package manager, network, or hosted-CI account is required.
metadata:
  owner: VibeForge-Systems/vibeforge-gate
  surface: vibeforge-gate
  version: "1"
---

# VibeForge Gate

A merge bar that belongs to the repository, not to a vendor.

`.vibeforge/gates.yaml` declares the checks. `.vibeforge/gate.sh` runs them. A git hook refuses work that does not pass. Nothing phones home, needs an account, or bills per minute — so the gate cannot be switched off by a billing event, and a check does not become unavailable because it sits behind someone's paid tier.

## The contract

**Green means every declared check actually ran and passed.** Everything below exists to keep that sentence true. If you weaken it, the gate stops being worth running — a bar you cannot trust is worse than no bar, because people merge on it.

Four rules make it hold. They are not stylistic:

1. **A missing tool is a SKIP, and a SKIP blocks.** Never let a check quietly not-run. The failure this prevents — "it went green because the check never executed" — is the one that actually ships bugs. The runner exits **2** on a SKIP; `VF_GATE_ALLOW_SKIP=1` accepts the gap, but the operator has to type it.
2. **Pin tool versions, and enforce the pin.** A scanner that silently updates its ruleset fails builds with no diff, or stops catching what it used to. A tool at the wrong version is treated as missing — a SKIP, not a pass. Pinning is also what makes the gate reproducible across machines.
3. **Distinguish a deliberate skip from a coverage gap.** `skip_env:` marks a check the operator may choose to skip (a slow race-detector pass, say). It is reported on screen, and it does not block. A gap you chose and can see is not the same thing as a gap you never knew about.
4. **Claim only what you check.** The gate's docs must say what it does *not* cover. Every overstatement gets discovered at the worst moment.

Read `references/doctrine.md` before making a judgment call about any of these.

## When you are asked to do something

| Ask | Do this |
|---|---|
| "set up a local gate" / "CI is dead, I need checks" | Detect the stack, propose a manifest, install the runner + hooks. See **Standing one up** below. |
| "add a check for X" | Add one block to `.vibeforge/gates.yaml`. Look in `catalog/` first — it may already be written. |
| "run the gate" / "is this mergeable?" | `.vibeforge/gate.sh` (full bar) or `--stage commit` (fast subset). Report the summary honestly, SKIPs included. |
| "we're on hosted CI but it's stopping" | Import the existing config once, then cut the dependency. See `references/importing-existing-ci.md`. |
| "install the hook" | `git config core.hooksPath .vibeforge/hooks`. See `references/hooks.md`. |
| "why is this check here?" | `.vibeforge/gate.sh --explain <id>` — every check carries its own rationale and provenance. |
| a check reports SKIP | Do **not** paper over it. Install the tool, or make the operator's acceptance of the gap explicit. |

## Standing one up

1. **Detect the stack.** Look for `go.mod`, `package.json`, `pyproject.toml`/`requirements.txt`, `Cargo.toml`, `*.csproj`, `Gemfile`. A repo can be several at once — that is normal, and each gets its own checks with a `dir:`.
2. **Harvest the checks that already exist.** In order of authority: an existing hosted-CI config (`.github/workflows/*.yml`, `.gitlab-ci.yml`, `.woodpecker.yml`, `Jenkinsfile`), then the repo's own `Makefile`/`justfile`/`package.json` scripts, then its contributor docs. Do not invent a bar when the repo already states one — the existing checks are the ones people expect to pass.
3. **Add what the vendor was charging for.** `catalog/supply-chain.yaml` holds the checks commonly gated behind a paid tier — secret scanning, SAST, dependency vulnerabilities, licenses, SBOM — with free, pinnable local equivalents. This is usually where a locally-owned gate becomes *better* than what was being rented, not merely equal.
4. **Split by stage.** `stage: commit` = fast and always-on (format, lint, fast unit tests, structural invariants). `stage: push` = the full bar. `stage: manual` = slow or occasional. `--stage push` runs commit checks too, so the full bar is always a superset.
5. **Copy the machinery in.** `templates/gate.sh` → `.vibeforge/gate.sh`, `templates/hooks/*` → `.vibeforge/hooks/`, `templates/gates.yaml` → `.vibeforge/gates.yaml` as the starting point. Then `git config core.hooksPath .vibeforge/hooks`.
6. **Run it, and show the user the summary.** Including the SKIPs. A first run with three SKIPs is a *correct* result and a to-do list; a first run doctored to be green is a lie you will both rely on later.

Then tell the user, in plain terms, what the gate does and does not cover.

## Writing a check

```yaml
  - id: secret-scan                 # unique; what --only and the summary use
    desc: no credential values in the tree
    stage: push                     # commit | push | manual
    dir: .                          # working directory, relative to repo root
    tool: gitleaks                  # resolved and version-checked before running
    tool_version: "8.30.1"          # the pin. Enforced, not decorative.
    version_cmd: gitleaks version   # how to ask (default: <tool> --version)
    install: "go install github.com/gitleaks/gitleaks/v8@v8.30.1"
    fetch: |                        # optional: fetch the pinned build if absent
      ...produce an executable at $VF_TOOL_DEST...
    why: |                          # shown by --explain. Say why it earns its runtime.
      Keys on the shape and entropy of credential values, not on identifier
      names, so it distinguishes a stored secret from a doc comment naming one.
    run: |
      "$VF_TOOL" detect --source "$VF_ROOT" --no-git --redact
```

Also available: `needs:` (comma-separated binaries that must exist), `probe:` (any command whose success means "ready" — use it for a library, e.g. `python3 -c 'import jsonschema'`), `env:` (`KEY=VALUE` per line), `skip_env:` (name of an env var that deliberately skips this check), `enforce: false` (advisory — runs, reports WARN on failure, never blocks), `enforce_after:` (the shadow-mode deadline — advisory until the date, blocking after), `timeout:` (seconds before a hung `run:` is killed and reported FAIL), `fetch_sha256:` (the bytes the fetched tool must hash to), `mirrors:` (provenance).

`run:` is bash, executed with `-euo pipefail` in `dir:`. `$VF_TOOL` is the resolved pinned binary, `$VF_ROOT` the repo root. Full field reference: `references/manifest.md`.

**Shadow mode.** A new check that would fail today goes in with `enforce: false`, gets tuned against real runs, and is promoted by deleting that line. Introducing a check in blocking mode on day one is how gates get disabled wholesale. Give it `enforce_after: YYYY-MM-DD` — the runner keeps it advisory until that date and blocks after it, so the promotion cannot be forgotten indefinitely.

## Resources

| File | Read it when |
|---|---|
| `references/doctrine.md` | Deciding anything about SKIPs, pins, coverage claims, or what belongs in a gate |
| `references/manifest.md` | Writing or debugging a manifest — every field, the parser's accepted subset, exit codes |
| `references/adapting-stacks.md` | Working out the checks and version pins for Go, Node, Python, Rust, or a mixed repo |
| `references/importing-existing-ci.md` | Migrating off a hosted CI config, including steps that cannot run locally |
| `references/hooks.md` | Installing, staging, or bypassing the git hooks |
| `catalog/*.yaml` | Adding a check — copy a written one rather than composing from scratch |
| `examples/go-sacp/` | A full worked example, plus its equivalence audit against a hand-written predecessor |

## Honest scope

Say all of this plainly when you install a gate; do not let the user discover it later.

- **A local hook is a convenience, not an enforced boundary.** Anyone can `--no-verify`, and a contributor who never installs the hook is never gated. Only a server-side check that runs on receipt is unbypassable. If the repo has somewhere that can run one — a self-hosted forge, a shared runner, a post-receive hook — say so.
- **The gate reproduces *commands*, not an environment.** It runs on your machine, your OS, your toolchain, your architecture. A matrix across OS or language versions, a container build, a deployment step, or anything needing credentials the repo does not have, is not reproduced. Declare those `stage: manual` or leave them out and *say* they are out.
- **A green gate means the declared checks passed.** It does not mean the manifest declares the right checks. The manifest is a claim about what matters, and it is only as good as the last time someone thought about it.
- **The runner parses a small YAML subset**, and errors with a line number on anything outside it. It will never silently misread a manifest — but a construct you expect from a full YAML library may simply be rejected.
