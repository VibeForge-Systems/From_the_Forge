---
description: Audit a private repository for public release, or verify one just published
argument-hint: "<OWNER/REPO | path> [inventory|scan|verify-rewrite|settings] — defaults to the full audit"
---

Use the `repo-publication-readiness` skill. Its `SKILL.md` holds the phases and
gates; follow them rather than restating them from memory.

Target and step: `$ARGUMENTS`.

## Default: the full audit (phases 0–2)

1. **Phase 0 first.** Ask whether the code is cleared for release (employer IP,
   client work). If the answer is no or unknown, stop there and say why; offer
   `references/employer-ip.md` as the checklist.
2. **Mirror clone** the target into a working directory outside any repository
   (`git clone --mirror`), so `refs/pull/*` are included.
3. Run `scripts/inventory.sh`, `scripts/scan-secrets.sh` and
   `scripts/sweep-leaks.py` against the mirror, with reports outside it.
4. Report findings grouped by disposition needed: credentials (rotate first),
   identities, internal details, deny-list terms, files, surfaces. Report every
   `INCOMPLETE`, `UNKNOWN` and `NOT RUN` as exactly that — never as clean.
5. If no private deny list exists, say that employer and client names were not
   checked, and offer to help the person write one at
   `~/.config/repo-publication-readiness/denylist` (outside every repository).

Do not rewrite history, change GitHub settings, or push anything as part of the
audit. Those are phase 4 and 8, each confirmed with the person first.

## Single steps

- `inventory` — `scripts/inventory.sh --repo MIRROR --out DIR [--github OWNER/REPO]`
- `scan` — `scripts/scan-secrets.sh` then `scripts/sweep-leaks.py`
- `verify-rewrite` — `scripts/verify-rewrite.sh --old MIRROR --new REWRITTEN`
- `settings` — `scripts/check-github-settings.sh OWNER/REPO` (needs admin for most checks)
