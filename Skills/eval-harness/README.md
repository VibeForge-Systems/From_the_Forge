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

Two scripts take over the bookkeeping from the second iteration on, so the
driving agent spends its turns launching agents rather than editing JSON:

```sh
export EVAL_SCRATCH="$S" EVAL_ITER=2 SKILL_CREATOR_DIR=~/.claude/plugins/.../skill-creator
python3 -I Skills/eval-harness/build-workspaces.py "$S" 2        # writes runs-iteration-2.json
python3 -I Skills/eval-harness/drive-iteration.py map  AGENT_ID RUN_KEY   # an executor was launched
python3 -I Skills/eval-harness/drive-iteration.py done AGENT_ID TOKENS MS # it finished: writes timing.json
python3 -I Skills/eval-harness/drive-iteration.py next                    # what to launch next
python3 -I Skills/eval-harness/drive-iteration.py gmap AGENT_ID RUN_KEY   # a grader was launched
python3 -I Skills/eval-harness/finalize.py 2 1 "$S"                       # benchmark.json/.md per skill, i2-vs-i1 table
```

`next` writes a run's grader brief only once both of its eval's runs carry
a `timing.json`, which is what keeps the assertions away from executors.
`finalize.py` fills tokens and time from `timing.json` (the aggregator
otherwise reports zero for them), sets one run per configuration, and prints
each with-skill expectation that failed, which is the list to act on.

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

## Lessons from the second iteration (2026-11-07)

Run against the revised evals, with executors told not to read the
repository and grader briefs withheld until both runs finished:

- **The suite now discriminates.** With-skill versus baseline, assertions
  passed: vibeforge-gate 37/38 vs 33/38, repo-publication-readiness 30/33
  vs 23/33, coroner 25/26 vs 8/26. Every remaining with-skill miss pointed
  at a real gap in the skill (a report file holding credential values, a
  missing install path, a missing warning, a missing measurement line) and
  each was fixed, so a third iteration measures the fixes, not the suite.
- **Read the executors' asides.** Two improvements came from remarks an
  executor made in passing rather than from a failed assertion: the
  scanner verdict reading CLEAN after an accepted gap, and the gate runner
  printing "every check ran and passed" above a SKIP row.
- **A proxy that injects credentials makes TruffleHog verification
  meaningless.** In this environment every `ghp_`-shaped fixture value
  verified as live. Fixture tokens are random; treat a "verified" flag from
  such a session as an artifact, and say so in the report.
- **Budget for twenty concurrent agents.** The 38 executors and 38 graders
  of one iteration ran in about ninety minutes of wall time at that limit;
  graders are quick, executors are not, so launch executors first and let
  graders fill the slots as pairs complete.
