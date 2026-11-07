# eval-harness

Runs the behaviour evals that each skill in this collection ships under its
`evals/evals.json`, the way the skill-creator plugin's loop expects: one
with-skill run and one baseline run per eval, each on a private copy of a
fixture repository, graded by a separate agent against the eval's assertions.

Nothing here runs on its own. An agent (Claude Code with the skill-creator
plugin, or a person with a scratch directory) drives it:

```sh
S=/some/scratch/dir                      # outside every repository
Skills/eval-harness/build-fixtures.sh "$S/fixtures"
python3 -I Skills/eval-harness/build-workspaces.py "$S"
# launch one executor agent per run; each reads <run>/prompt.txt
# after an eval's two runs finish:
SKILL_CREATOR_DIR=~/.claude/plugins/.../skill-creator \
  python3 -I Skills/eval-harness/build-grader-prompts.py "$S"
# launch one grader agent per run; each reads <run>/grader-prompt.txt
# then, from the skill-creator directory:
python -m scripts.aggregate_benchmark "$S/<skill>-workspace/iteration-1" --skill-name <skill>
python eval-viewer/generate_review.py "$S/<skill>-workspace/iteration-1" \
  --skill-name <skill> --benchmark "$S/<skill>-workspace/iteration-1/benchmark.json" --static out.html
```

`write-timing.sh AGENT_ID TOKENS DURATION_MS` records the figures a run's
completion notification carries into that run's `timing.json`, given the
`agent-map.json` the driver keeps.

## What the fixtures plant

`build-fixtures.sh` creates throwaway repositories under the directory you
give it, never inside this repository:

| Fixture | For | Planted |
|---|---|---|
| `ci-node`, `gated-node`, `gated-node-skip` | vibeforge-gate | a hosted-CI workflow with no local gate; a gated repo whose branch carries a token committed then deleted; a gate whose pinned scanner is absent |
| `side-tool` (+ mirror, rewritten clone, empty target) | repo-publication-readiness | an AWS key pair in history, a PAT only on `refs/pull/3/head`, an internal host, a private address, a UNC share, a client name, an employer-domain author |
| `express-prisma`, `next-app` | coroner | SQL injection, no tenant filter, HTTP inside a transaction, a client per request, an N+1, middleware-only auth on a vulnerable Next.js |

Credential-shaped values are generated at run time. The internal-looking
hostnames and addresses are literals, reviewed in `.vibeforge/leak-allow`.

## Lessons from the first iteration (2026-11-07)

Three things to keep in mind when driving it:

- **Write grader prompts after the executors finish.** They contain the
  assertions, and an executor that lists its run directory can read them.
  One did.
- **Baseline runs can find the skill on disk** when the repository is on the
  same machine. Three did. Run baselines from a checkout that excludes
  `Skills/`, or on a separate machine, if the comparison has to be clean.
- **The harness prompt says the user cannot answer**, so assertions that
  require stopping to ask are unsatisfiable as worded. Phrase them as
  "raises X as a blocking precondition and does not do Y while it is
  unresolved."
