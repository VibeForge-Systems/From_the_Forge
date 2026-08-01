# From_the_Forge

Collection of Interesting Tips, Tricks and Reference Cards, Quick Code Snipits and other forgings

## What's here

| Directory | What it is |
|---|---|
| [`Skills/vibeforge-gate/`](Skills/vibeforge-gate/) | A repository-owned merge bar — a local pre-commit/pre-push gate that runs the checks a hosted CI service would have, without needing one. Also a Claude Code plugin and skill. |
| [`Skills/`](Skills/) | Packaged Claude skills (`.skill` / `.zip`) |
| [`Tips_and_Tricks/`](Tips_and_Tricks/) | MCP quick-reference cards |
| [`windows-env-bundle/`](windows-env-bundle/) | PowerShell bundle for setting up a Windows environment |

## Plugin marketplace

This repository is a Claude Code plugin marketplace:

```
/plugin marketplace add VibeForge-Systems/From_the_Forge
/plugin install vibeforge-gate@from-the-forge
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

This repository is **AGPL-3.0** ([`LICENSE`](LICENSE)), with one exception:

- **[`Skills/vibeforge-gate/`](Skills/vibeforge-gate/) is Apache-2.0** — see
  [`Skills/vibeforge-gate/LICENSE`](Skills/vibeforge-gate/LICENSE). It is kept
  under a permissive license so it can be vendored into repositories that could
  not accept a copyleft dependency, and contributions to that directory are
  inbound = outbound under Apache-2.0 per
  [`Skills/vibeforge-gate/DCO`](Skills/vibeforge-gate/DCO).

Sign off every commit (`git commit -s`); the `dco` check verifies it.
