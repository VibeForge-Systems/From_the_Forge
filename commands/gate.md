---
description: Run, set up, or extend this repository's local merge bar
argument-hint: "[run|setup|add <check>|explain|hooks] — defaults to run"
---

Use the `vibeforge-gate` skill for the doctrine and machinery. Its `SKILL.md`
routes to the right reference; do not restate the doctrine from memory.

Requested action: `$ARGUMENTS` (empty means **run**).

## run (default)

If `.vibeforge/gate.sh` exists, run it:

- Whole bar: `.vibeforge/gate.sh`
- Fast subset: `.vibeforge/gate.sh --stage commit`
- One check: `.vibeforge/gate.sh --only <id>`

Report the summary **honestly**, exactly as it came out:

- **Exit 0** — every declared check ran and passed. Say so plainly.
- **Exit 1** — a check failed. Show which, and the relevant output. Offer to fix it.
- **Exit 2** — a check SKIPPED because its tool is missing or is the wrong
  version. This is a **coverage gap, not a pass**. Name the check, name the
  tool, give the install command from the SKIP line. Do not suggest
  `VF_GATE_ALLOW_SKIP=1` as the fix — mention it only if the user has already
  decided to accept the gap.
- **Exit 3** — manifest or usage error. Fix the manifest.

Never describe a run with SKIPs as green.

If `.vibeforge/gate.sh` does not exist, say so and offer **setup**.

## setup

Follow "Standing one up" in `SKILL.md`. In short: detect the stack, harvest the
checks that already exist (hosted-CI config, then Makefile/scripts, then
contributor docs), copy in `templates/gate.sh`, `templates/hooks/*` and a
manifest built from `catalog/`, then run it and show the result — SKIPs included.

Ask before installing the hooks (`git config core.hooksPath .vibeforge/hooks`);
it changes the user's git behavior and replaces any existing `.git/hooks`.

## add

Add one check to `.vibeforge/gates.yaml`. Look in `catalog/` first — it is
probably already written. Carry over the version pin and the `why:`. If the
check would fail on the current codebase, propose it with `enforce: false` and
say when it should be promoted.

## explain

`.vibeforge/gate.sh --explain [id]` — each check carries its own rationale and
provenance. Use `--list` for the inventory.

## hooks

Install: `git config core.hooksPath .vibeforge/hooks`. See
`references/hooks.md`, and state the limits: bypassable with `--no-verify`,
per-clone opt-in, local-machine only.
