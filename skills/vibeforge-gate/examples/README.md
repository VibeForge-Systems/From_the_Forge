# Examples

## `go-sacp/` — a real migration, audited

A Go repository (the Sovereign Agent Control Plane) whose hosted CI stopped
running when its Actions minutes ran out. Before this package existed, someone
hand-wrote a 238-line bash script to reproduce its five workflows. That script
is the reason this package exists — it is where the doctrine came from.

| File | What it is |
|---|---|
| `gates.yaml` | The same merge bar, re-expressed as a manifest |
| `EQUIVALENCE.md` | The audit: what matched, the four deliberate differences, and what neither version establishes |
| `reference-ci-local.sh` | The hand-written predecessor, verbatim |
| `reference-pre-push` | Its pre-push hook, verbatim |

The reference files are **preserved unmodified on purpose** — they are the
evidence the audit is measured against, so the gate excludes them from linting.
Read `reference-ci-local.sh` alongside `gates.yaml` to see what generalized:
`run_gate`/`skip_gate`/`note_skip` became the runner's verdict functions,
`find_or_fetch_gitleaks` became `tool:`/`tool_version:`/`fetch:`, and the
`SACP_CI_ALLOW_SKIP` escape hatch became `VF_GATE_ALLOW_SKIP`.

### Result

Both were run against the same tree, same commit, same machine. Both exit 0.
The hand-written gate reports 7 passes plus one deliberate skip; the manifest
reports 8 plus the same deliberate skip — one gate was split in two, because
the original reported a missing optional tool as a *failure* of a gate whose
other checks had passed.

The other three differences: the version pin is now enforced rather than
documented, stages exist so there is a fast pre-commit subset, and the `PATH`
fixup is declared as data instead of hardcoded. `EQUIVALENCE.md` has the
reasoning for each, including the one that changes behavior.

### Worth noticing

- **The `mirrors:` field is history, not a dependency.** It records which
  workflow a check came from. Nothing reads those files at runtime; the
  repository could delete them tomorrow.
- **Every check kept its `why:`.** The CI files carried the only written
  explanation of why several of these gates existed. That reasoning survived the
  move and is what `--explain` prints.
- **The sign-off check needs no API.** The hosted version read PR commit
  metadata through the forge. The local one uses `git merge-base` and
  `git rev-list` — same set of commits in normal use, no token, no network.

### Run it yourself

```sh
cd <the-go-repo>
SACP_CI_SKIP_RACE=1 VF_GATE_MANIFEST=<here>/gates.yaml \
  bash <package>/skills/vibeforge-gate/templates/gate.sh
```

## This repository's own gate

The other worked example is `.vibeforge/gates.yaml` at the root of this package.
It gates the package with the package: shell syntax, shellcheck against a pinned
fetched build, the 19-case runner selftest, skill frontmatter validity, and a
check that every catalog and example manifest parses with the real parser — so a
broken example cannot ship.

It also has a `templates-in-sync` check, because this repo keeps two copies of
the runner (the shipped template and the one it dogfoods), and two copies drift.
