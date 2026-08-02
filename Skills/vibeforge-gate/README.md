# vibeforge-gate

A merge bar that belongs to your repository, not to a vendor.

`.vibeforge/gates.yaml` declares the checks. `.vibeforge/gate.sh` runs them. A
git hook refuses work that does not pass. No account, no minutes, no network,
no YAML library — bash, coreutils and git.

```
$ .vibeforge/gate.sh

my-repo — local merge bar, stage: push

==> format — sources are formatted
   PASS  format
==> test — the test suite
   PASS  test
==> secret-scan — no credential values in the tree
   SKIP  secret-scan — gitleaks on PATH is not the pinned v8.30.1 (it reports: 8.18.4)

==> SUMMARY
  format       PASS
  test         PASS
  secret-scan  SKIP   gitleaks on PATH is not the pinned v8.30.1

✗ GATE INCOMPLETE — a check was SKIPPED, so this run is weaker than the bar you declared.
```

That last part is the whole point.

## Why

A hosted CI service is a rental, and the gates stop when the rental does — the
minutes run out, the account lapses, the free tier changes shape, you are on a
plane. The checks were catching real defects right up until they stopped, and
nothing about the code got safer at that moment.

There is a quieter version of the same problem: the check you want exists but
sits behind a tier you are not paying for. Secret scanning, static analysis,
dependency review — the underlying tools are open source and run fine on a
laptop. What is being sold is the integration, not the capability.

This package puts the bar back in the repository. It ships the doctrine for
keeping such a gate honest, a manifest-driven runner, git hooks, and a catalog
of ready-to-use checks — including the ones usually behind a paid tier.

## The contract

> **Green means every declared check actually ran and passed.**

Four rules keep that true:

1. **A missing tool is a SKIP, and a SKIP blocks** (exit 2). The failure this
   prevents — "it went green because the check never executed" — is the one that
   actually ships bugs. `VF_GATE_ALLOW_SKIP=1` accepts a gap, but someone has to
   type it.
2. **Pinned versions are enforced.** A tool at the wrong version is treated as
   missing. A gate running a different scanner than the one you pinned is not
   the gate you reviewed.
3. **A deliberate skip is not a coverage gap.** `skip_env:` lets an operator
   skip a slow check; it is reported on screen and does not block. Chosen and
   visible is not the same as never-ran and unknown.
4. **Claim only what you check.** See *Honest limits*.

The runner's own [`selftest.sh`](selftest.sh) asserts all of this — 29 cases
covering the exit codes, the SKIP semantics, version-pin and sha256
enforcement, timeouts, shadow-mode deadlines, stage selection, and parser
strictness.

## Install

This package lives at `Skills/vibeforge-gate/` in the
[From_the_Forge](https://github.com/VibeForge-Systems/From_the_Forge) collection
repository, which is also a plugin marketplace.

As a Claude Code plugin, available in every repo you open:

```
/plugin marketplace add VibeForge-Systems/From_the_Forge
/plugin install vibeforge-gate@from-the-forge
```

Or from a local clone:

```sh
git clone https://github.com/VibeForge-Systems/From_the_Forge.git
claude --plugin-dir ./From_the_Forge/Skills/vibeforge-gate
```

Or as a plain skill, no plugin machinery:

```sh
P=From_the_Forge/Skills/vibeforge-gate/skills/vibeforge-gate
cp -r "$P" ~/.claude/skills/      # everywhere
cp -r "$P" .claude/skills/        # one repo
```

Then, in a repo that needs a gate:

```
/vibeforge-gate:gate setup
```

The skill detects the stack, harvests the checks the repo already states, writes
a manifest, and installs the runner. Or do it by hand — see
[`INSTALL.md`](INSTALL.md).

## Use

```sh
.vibeforge/gate.sh                    # the full bar
.vibeforge/gate.sh --stage commit     # the fast subset
.vibeforge/gate.sh --only lint        # one check
.vibeforge/gate.sh --list             # what is declared
.vibeforge/gate.sh --explain dco      # why a check exists, and where it came from
.vibeforge/gate.sh --fetch            # pre-download pinned tools
.vibeforge/gate.sh --pristine         # run in a clean worktree of HEAD

git config core.hooksPath .vibeforge/hooks   # enforce it
```

| Exit | Meaning |
|---|---|
| 0 | every selected check ran and passed |
| 1 | a check FAILED |
| 2 | a check SKIPPED — coverage gap (`VF_GATE_ALLOW_SKIP=1` to accept) |
| 3 | manifest or usage error |

## A check

```yaml
  - id: secret-scan
    desc: no credential values committed anywhere in the tree
    stage: push
    tool: gitleaks
    tool_version: "8.30.1"
    version_cmd: gitleaks version
    install: "go install github.com/gitleaks/gitleaks/v8@v8.30.1"
    why: |
      Keys on the shape and entropy of credential values rather than identifier
      names, so it distinguishes a stored secret from a doc comment naming one.
    run: |
      "$VF_TOOL" detect --source "$VF_ROOT" --no-git --redact --no-banner
```

Adding a check is editing one block. Full field reference:
[`references/manifest.md`](skills/vibeforge-gate/references/manifest.md).

## What's in the box

Everything below is relative to `Skills/vibeforge-gate/`. Two files sit outside
it, at the collection repo's root, because git and the plugin loader both
resolve them per repository: `.claude-plugin/marketplace.json` (points here) and
`.vibeforge/` (the gate this repo runs on itself).

```
.claude-plugin/plugin.json
commands/gate.md                     ->  /vibeforge-gate:gate
selftest.sh                          29 cases pinning the runtime contract
skills/vibeforge-gate/
  SKILL.md                           doctrine + when to use it
  references/
    doctrine.md                      why a local gate, and the rules that keep it honest
    manifest.md                      every field, the parser's subset, exit codes
    adapting-stacks.md               Go / Node / Python / Rust / mixed; where pins come from
    importing-existing-ci.md         migrating off a hosted config
    hooks.md                         install, stages, bypass, limits
  catalog/
    universal.yaml                   sign-off, conflict markers, large files, shellcheck
    languages.yaml                   Go, Node, Python, Rust, containers
    supply-chain.yaml                secret scanning, SAST, vulns, licenses, SBOM
  templates/
    gate.sh                          the runner
    gates.yaml                       annotated starter manifest
    hooks/{pre-commit,pre-push,pre-receive}
  examples/go-sacp/                  a real migration, with its equivalence audit
```

## Honest limits

- **A local hook is a convenience, not an enforced boundary.** Anyone can
  `--no-verify`; a contributor who never sets `core.hooksPath` is never gated,
  and nothing tells you they haven't. Only a check that runs on the receiving
  end is unbypassable. If you have somewhere to run one, point it at the same
  manifest.
- **It reproduces commands, not environments.** One machine, one OS, one
  architecture, whatever toolchain you have. Cross-platform matrices are the
  thing hosted CI genuinely gives you that a laptop cannot.
- **A green gate means the declared checks passed** — not that the manifest
  declares the right ones. The manifest is a claim about what matters, only as
  good as the last time someone thought about it.
- **The parser accepts a small YAML subset** and errors with a line number on
  anything else. It will never silently misread a manifest, but it will reject
  constructs a full YAML library would accept.
- **Validated against one real migration.** `examples/go-sacp/` documents a Go
  repository moved off five hosted workflows, including the four places the new
  gate deliberately differs. The runner is stack-agnostic and its semantics are
  pinned by the selftest; the *mapping* from an existing CI config to a manifest
  is a reading task, not a mechanical one.

## Contributing

Sign off every commit (`git commit -s`) and run `.vibeforge/gate.sh` from the
collection repo's root before opening a PR. Contributions to this directory are
inbound = outbound under Apache-2.0. See [CONTRIBUTING.md](CONTRIBUTING.md) and
[DCO](DCO).

## License

**Apache 2.0** — see [LICENSE](LICENSE). That is this directory's license, and
it governs everything under `Skills/vibeforge-gate/`.

The surrounding From_the_Forge repository is AGPL-3.0. Apache-2.0 is compatible
in that direction, so the collection as a whole may be distributed under AGPL —
but this package on its own remains Apache-2.0, which is what makes it safe to
vendor into repositories that could not accept a copyleft dependency.
