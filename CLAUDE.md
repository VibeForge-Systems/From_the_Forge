# CLAUDE.md

The shared, tool-neutral guide is [`AGENTS.md`](AGENTS.md), imported below. Edit
that file for anything every agent should know; keep this file to Claude Code
specifics.

@AGENTS.md

## Claude Code specifics

- **Nested guides load on demand.** When you work under these directories, Claude
  Code reads their guides as well:
  - `Apps/openmessage-archive/CLAUDE.md`
  - `Skills/vibeforge-gate/CLAUDE.md`
- **Which file to edit.** In every directory, exactly one of the two guide
  files holds the content and the other only points to it:
  - At the root and in `Apps/openmessage-archive/`, `AGENTS.md` is canonical
    and `CLAUDE.md` imports it.
  - In `Skills/vibeforge-gate/`, `CLAUDE.md` is canonical and `AGENTS.md`
    points to it.

  Edit the canonical file, and never paste content into the pointer.
- **This repository is a plugin marketplace.** The plugins it lists are
  `vibeforge-gate` and `coroner`. To exercise them as a user would:

  ```
  /plugin marketplace add VibeForge-Systems/From_the_Forge
  /plugin install vibeforge-gate@from-the-forge
  /plugin install coroner@from-the-forge
  ```

  Changes to a plugin directory show up only after reinstalling from a
  marketplace that includes the change.
- **Architecture reviews.** For an architecture review, audit or pre-refactor
  assessment, use the coroner subagent (`/coroner:coroner-review <scope>
  <depth>`). It is read-only; apply its findings in a separate step.
- **Shell.** The gate and the selftest need bash 4+. Use the Bash tool, never
  PowerShell, even on Windows hosts.
- **`.claude/` is gitignored.** Shared settings, hooks or skills cannot be
  committed under `.claude/` unless `.gitignore` is changed deliberately, for
  example to `.claude/*` with `!.claude/settings.json`. Propose that change
  rather than working around it.
