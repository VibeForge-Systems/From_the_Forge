# AGENTS.md

Guidance for AI coding agents (Claude Code, Codex, Cursor, and others) working
in this repository. Claude Code reads it through [`CLAUDE.md`](CLAUDE.md).

## What this repository is

**From_the_Forge** is a *collection*, not one product. Each top-level directory
is an independent deliverable. Each has its own purpose, tooling, and in some
cases its own license. Treat a task as belonging to exactly one of them unless
it explicitly spans several.

| Directory | What it is | License | How it is verified | Deeper guide |
|---|---|---|---|---|
| `Apps/openmessage-archive/` | Tauri 2 / Rust / Vue 3 desktop app that archives iPhone Messages from local backups | **Apache-2.0** | GitHub Actions (`.github/workflows/openmessage-archive.yml`) | [`Apps/openmessage-archive/AGENTS.md`](Apps/openmessage-archive/AGENTS.md) |
| `Skills/vibeforge-gate/` | Repository-owned merge bar: a bash runner, YAML manifests, and a Claude plugin and skill | **Apache-2.0** | `./Skills/vibeforge-gate/selftest.sh` and the root gate | [`Skills/vibeforge-gate/CLAUDE.md`](Skills/vibeforge-gate/CLAUDE.md) |
| `Skills/coroner/` | Read-only adversarial architecture-review subagent, shipped as a Claude plugin | **Apache-2.0** | The root gate (`plugin-json` check); otherwise prose, reviewed by hand | This file, below |
| `Skills/repo-publication-readiness/` | Audit, clean and publish a private repo as public: scanners, leak sweep, rewrite verifier, settings checker; a Claude plugin and skill | **Apache-2.0** | `./Skills/repo-publication-readiness/selftest.sh` and the root gate | This file, below |
| `Skills/*.zip`, `Skills/*.skill`, `Skills/ballast-operations/` | Packaged Claude skills (build outputs) | AGPL-3.0 | None | This file, below |
| `Tips_and_Tricks/` | MCP reference cards (Markdown) | AGPL-3.0 | None | `Tips_and_Tricks/README.md` |
| `windows-env-bundle/` | PowerShell bundle that sets up a Windows 11 workstation | AGPL-3.0 | Nothing lints or tests PowerShell here | `windows-env-bundle/docs/ARCHITECTURE.md` |

Some things must live at the repository root, because git and GitHub resolve
them per repository:

- **`.vibeforge/`** holds the gate this repo runs on itself: `gates.yaml`,
  `gate.sh`, and the hooks.
- **`.claude-plugin/marketplace.json`** makes this repo a plugin
  marketplace. It lists `vibeforge-gate`, `coroner` and
  `repo-publication-readiness`.
- **`.github/workflows/`** holds one workflow per deliverable, each scoped
  with path filters.

## Rules that apply everywhere

### Licensing

The root is **AGPL-3.0**. Four directories are **Apache-2.0** and carry
their own `LICENSE` and `DCO` files: `Skills/vibeforge-gate/`,
`Skills/coroner/`, `Skills/repo-publication-readiness/`, and
`Apps/openmessage-archive/`.

- Keep those files where they are.
- Never copy code across a license boundary. AGPL code must not go into an
  Apache-2.0 directory.
- Never copy GPL, LGPL, AGPL, or CC BY-SA code into an Apache-2.0
  directory. Each Apache-2.0 directory's `DCO` states its inbound terms.

### Sign-off

Sign off every commit with `git commit -s`. The gate's `dco` check fails on
any non-merge commit ahead of `main` that lacks a `Signed-off-by` trailer.

### Branches, commits, and PRs

- Work on a topic branch off `main`, never on `main` itself. Name it
  `feat/...`, `fix/...`, `docs/...`, `refactor/...` or `chore/...`.
- Write commit subjects as conventional commits: `type(scope): summary`. The
  scope is the package, for example `feat(coroner): ...` or
  `fix(openmessage-archive): ...`.
- PRs are merged with merge commits, so do not squash history that carries
  sign-offs.

### The gate

Run the gate from the repository root before opening a PR:

```sh
.vibeforge/gate.sh                  # full bar (stage: push)
.vibeforge/gate.sh --stage commit   # fast subset
.vibeforge/gate.sh --list           # what is declared
.vibeforge/gate.sh --explain <id>   # why a check exists
```

Exit codes:

| Code | Meaning |
|---|---|
| `0` | Every check ran and passed |
| `1` | A check failed |
| `2` | A check was **SKIPPED**. A SKIP is a coverage gap, not a pass. Report it as a SKIP; never call that run green. |
| `3` | Manifest or usage error |

The gate needs bash 4+. On Windows, run it from Git Bash or WSL, not
PowerShell.

### Data and secrets

- Never commit secrets, credentials, or real personal data. That includes
  real iPhone backups and messages: test fixtures must be synthetic.
- This repository is **public**. The gate checks every commit a push would
  publish — credentials (including merge resolutions and messages), commit
  identities against `.vibeforge/allowed-identities`, internal IPs and
  hostnames, oversized or credential-bearing files, and a private deny list of
  employer and client names that lives outside the repo
  (`~/.config/vibeforge/leak-denylist` or `$VF_LEAK_DENYLIST_PATTERNS`). Never
  add a deny-list term to any file in this repository.
- Generate credential-shaped test values at run time; never commit one, even
  a fake.
- `.claude/` is gitignored on purpose; per-clone agent settings stay local.

### Claims

- **Claim only what you verified.** If you could not run something, say so;
  for example, PowerShell on a Linux runner, or a Windows-only UI.
- **Narrow a coverage claim rather than round it up.** Several READMEs here
  ("This repository gates itself", `compatibility.md`) depend on that
  discipline.

## Per-directory notes

### `Skills/vibeforge-gate/`

Read its `CLAUDE.md` before changing anything. Two rules from it bite most
often:

1. The runner and hooks exist twice. The **template** under
   `skills/vibeforge-gate/templates/` is the source of truth. Edit it, then
   copy it to `.vibeforge/`. The `templates-in-sync` check fails on drift.
2. Any change to runner behaviour needs a `selftest.sh` case.

### `Skills/coroner/`

The product is prose: the subagent prompt in `agents/coroner.md`, and the
command in `commands/coroner-review.md`.

- **The agent is read-only by design.** Its `tools:` list must never gain
  `Edit`, `Write`, `Bash`, or any other mutating tool. The README explains
  why: diagnosis and patching are separate acts.
- When behaviour changes, bump `version` in `.claude-plugin/plugin.json`.
- The `name` in `plugin.json` must match the plugin's entry in the root
  `marketplace.json`. The `plugin-json` gate check enforces this.

### `Skills/repo-publication-readiness/`

- The scripts are read-only toward GitHub and never push; keep it that way.
  Publishing is the one irreversible step, and the skill leaves it to a person.
- Every script refuses report, deny-list and output paths inside the
  repository it inspects. Do not add a mode that writes into it.
- Behaviour changes to a script need a `selftest.sh` case. Planted leaks in the
  selftest are built at run time; the two literal fixture values it does carry
  are listed in `.vibeforge/leak-allow`.
- Claims in `references/` that depend on a tool or platform version are
  recorded in `references/verification-log.md` with how and when they were
  checked. Update the log when you re-verify or change one.

### Packaged skills (`Skills/*.zip`, `Skills/*.skill`, `Skills/ballast-operations/*.zip`)

These are archives built elsewhere; their source is not in this repository.

- Replace them whole when a new build arrives. Never edit or re-zip their
  contents by hand.
- `recall-operations.skill` and `recall-operations.zip` are byte-identical
  copies of the same archive. Update both together, or neither.

### `windows-env-bundle/`

- `bundle.psd1` is the single source of truth, and the scripts stay
  generic. Registry changes must stay annotated and reversible.
- Every downloaded binary is verified by SHA256. A `'REPLACE_WITH_SHA256'`
  value is a deliberate fail-closed placeholder. Fill it only with a hash you
  computed from the real download; never invent one.
- Releases are cut by pushing a `winenv-v*` tag. The profile version in
  `docs/PLATFORM-PROFILE.md` tracks that tag.
- Nothing in this repo lints or runs PowerShell, so changes here are
  unverified unless someone runs them on Windows 11. Say that plainly.

### `Tips_and_Tricks/`

Reference cards. Keep the distinction the folder's README draws: state
clearly what the MCP specification *requires* and what is house *preference*.

## Known gaps

- The root gate does not build or test `Apps/openmessage-archive/`. Only that
  app's GitHub Actions workflow does.
- Nothing lints PowerShell.

These are true today; do not describe either area as gated until a check
exists.
