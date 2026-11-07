# From_the_Forge

Collection of Interesting Tips, Tricks and Reference Cards, Quick Code Snipits and other forgings

## What's here

| Directory | What it is |
|---|---|
| [`Skills/`](Skills/) | Claude Code plugins, skills and the harness that evaluates them. See **Skills** below. |
| [`Apps/openmessage-archive/`](Apps/openmessage-archive/) | OpenMessage Archive — a local-first Tauri 2 / Rust / Vue 3 desktop app for archiving iPhone Messages from local backups (Windows 11 and Linux). Milestone 3 (the extraction check) implemented; its sign-off is pending. |
| [`Tips_and_Tricks/`](Tips_and_Tricks/) | MCP quick-reference cards: tools, prompts and resources, plus one deeper guide on MCP tool design. |
| [`windows-env-bundle/`](windows-env-bundle/) | PowerShell bundle that sets up a Windows 11 workstation to the VibeForge profile: one manifest, SHA256-verified downloads, annotated and reversible registry changes. |

## Skills

Three skills are developed here and ship as Claude Code plugins (each is also
usable as a plain skill). Each carries behaviour evals under `evals/evals.json`
and a trigger-query set; the first full eval loop ran on 2026-11-07 (see the
pull request history for the numbers).

| Plugin | Version | What it does | How it is verified |
|---|---|---|---|
| [`vibeforge-gate`](Skills/vibeforge-gate/) | 1.1.3 | A repository-owned merge bar: `.vibeforge/gates.yaml` declares checks, `gate.sh` runs them, git hooks refuse work that does not pass. Pinned tool versions, a missing tool is a SKIP that blocks, shadow mode with deadlines, and checks that judge the commits a push publishes rather than the working tree. Ships a catalog of ready checks, including the ones hosted CI sells as a paid tier. | 52-case `selftest.sh`; this repository's own gate; 7 behaviour evals, 24 trigger queries |
| [`coroner`](Skills/coroner/) | 1.2.0 | The VibeForge Systems AI Code Coroner: adversarial architecture review as a read-only subagent. Severity-ranked findings in a fixed evidence block (now with a Steelman line), pre-mortem exhibits, a System-of-Record map, a strangler refactor plan and the Reprieve. It diagnoses and never edits, even when asked to fix as it goes. | Root gate (`plugin-json`); 5 behaviour evals, 16 trigger queries |
| [`repo-publication-readiness`](Skills/repo-publication-readiness/) | 1.1.1 | Take a private GitHub repository public without leaking: a nine-phase procedure with a gate per phase, scripts for inventory, two-scanner secret scanning over every ref including `refs/pull/*`, a leak sweep with a private deny list shared with vibeforge-gate, a rewrite verifier and a settings checker. Protect first, push second, and the push stays with a person. | 46-case `selftest.sh` (41 without scanners); root gate; 7 behaviour evals, 22 trigger queries |

Two more skills are packaged here as build outputs; their source lives elsewhere:

| Skill | What it does |
|---|---|
| [`recall-operations`](Skills/) (`.skill` and `.zip`, byte-identical) | Routes an agent to the right shipped resource when working with Recall, VibeForge's structured memory layer for AI-assisted project work on OBTO. |
| [`ballast-operations`](Skills/ballast-operations/) | Routes an agent to the right shipped resource when working with Ballast, VibeForge's MCP-native execution layer for spec-driven development on OBTO. |

[`Skills/eval-harness/`](Skills/eval-harness/) builds the throwaway fixture
repositories, workspaces and grader briefs the skill-creator eval loop needs to
run the three plugins' evals against a baseline. Nothing runs it automatically;
its README says how an agent drives it and what the first iteration taught.

AI coding agents: start with [`AGENTS.md`](AGENTS.md) (Claude Code reads it via
[`CLAUDE.md`](CLAUDE.md)). Subdirectories with their own conventions carry their
own guide.

## Plugin marketplace

This repository is a Claude Code plugin marketplace:

```
/plugin marketplace add VibeForge-Systems/From_the_Forge
/plugin install vibeforge-gate@from-the-forge
/plugin install coroner@from-the-forge
/plugin install repo-publication-readiness@from-the-forge
```

## This repository gates itself

`.vibeforge/` holds the merge bar that runs on this repository, built with the
package in `Skills/vibeforge-gate/`. It lives at the root rather than inside that
directory because git resolves `core.hooksPath` per repository, not per
directory — a gate nested in a subdirectory could never be wired to a hook.

```sh
.vibeforge/gate.sh                           # the full bar
.vibeforge/gate.sh --list                    # what is declared
.vibeforge/gate.sh --explain <id>            # why a check exists
git config core.hooksPath .vibeforge/hooks   # enforce it (per clone, opt-in)
```

A green run means every declared check ran and passed. A SKIP is a coverage gap,
not a pass, and exits non-zero. The hooks are bypassable (`--no-verify`) and
opt-in per clone, so they are a convenience rather than an enforced boundary.

## Licensing

This repository is **AGPL-3.0** ([`LICENSE`](LICENSE)), with four exceptions:

- **[`Skills/vibeforge-gate/`](Skills/vibeforge-gate/) is Apache-2.0** — see
  [`Skills/vibeforge-gate/LICENSE`](Skills/vibeforge-gate/LICENSE). It is kept
  under a permissive license so it can be vendored into repositories that could
  not accept a copyleft dependency, and contributions to that directory are
  inbound = outbound under Apache-2.0 per
  [`Skills/vibeforge-gate/DCO`](Skills/vibeforge-gate/DCO).
- **[`Skills/coroner/`](Skills/coroner/) is Apache-2.0** — see
  [`Skills/coroner/LICENSE`](Skills/coroner/LICENSE). Permissive for the same
  reason: it is meant to be vendored into the repositories it reviews, including
  ones that could not accept copyleft. Contributions to that directory are
  inbound = outbound under Apache-2.0 per
  [`Skills/coroner/DCO`](Skills/coroner/DCO).

- **[`Skills/repo-publication-readiness/`](Skills/repo-publication-readiness/) is
  Apache-2.0** — see [`Skills/repo-publication-readiness/LICENSE`](Skills/repo-publication-readiness/LICENSE).
  Permissive so its scripts can be dropped into any repository being prepared
  for release; contributions are inbound = outbound under Apache-2.0 per
  [`Skills/repo-publication-readiness/DCO`](Skills/repo-publication-readiness/DCO).

- **[`Apps/openmessage-archive/`](Apps/openmessage-archive/) is Apache-2.0** — see
  [`Apps/openmessage-archive/LICENSE`](Apps/openmessage-archive/LICENSE). It is a
  standalone application meant to be distributed as permissively licensed
  open source; contributions to that directory are inbound = outbound under
  Apache-2.0 per [`Apps/openmessage-archive/DCO`](Apps/openmessage-archive/DCO).

Sign off every commit (`git commit -s`); the `dco` check verifies it.
