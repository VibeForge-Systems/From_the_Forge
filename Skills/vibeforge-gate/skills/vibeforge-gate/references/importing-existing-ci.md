# Importing an existing CI config

A repository migrating off hosted CI already has a merge bar. Read it once,
transcribe it into the manifest, then cut the dependency. After the import the
CI files are history — the `mirrors:` field records where a check came from, and
nothing reads those files at runtime.

Do this as a **reading task**, not a mechanical translation. The goal is a
manifest a maintainer would recognize as their bar, not a literal transcription
of YAML that happens to execute.

## Where the config lives

| Provider | Files |
|---|---|
| GitHub Actions | `.github/workflows/*.yml` |
| GitLab CI | `.gitlab-ci.yml`, `.gitlab/ci/*.yml` |
| Woodpecker / Drone | `.woodpecker.yml`, `.woodpecker/*.yml`, `.drone.yml` |
| Forgejo / Gitea Actions | `.forgejo/workflows/*.yml`, `.gitea/workflows/*.yml` |
| CircleCI | `.circleci/config.yml` |
| Jenkins | `Jenkinsfile` |
| Azure Pipelines | `azure-pipelines.yml` |
| Buildkite | `.buildkite/pipeline.yml` |

## Mapping

**One check per job**, not per file and not per step. A file with two jobs
becomes two checks — that is what makes the summary tell you *what* broke. A job
with four sequential `run:` steps becomes one check with a four-line `run:`
block, because those steps share a failure meaning.

| In the config | In the manifest |
|---|---|
| job / stage name | `id` + `desc` |
| `run:` / `script:` steps | lines of the `run:` block, in order |
| `working-directory:` / `defaults.run.working-directory` | `dir:` |
| `env:` | `env:` |
| a setup action (`setup-go`, `setup-node`, `setup-python`) | **not a check** — a version pin. See below. |
| a tool-install step | `tool:` + `tool_version:` + `fetch:` + `install:` |
| `if:` conditions on a step | usually drop; if load-bearing, fold into the `run:` block |
| `continue-on-error: true` | `enforce: false` |
| the comment above the job | `why:` — keep it, it is the reasoning |
| the file and job name | `mirrors:` |

### Setup actions are pins, not checks

`actions/setup-go@v5` with `go-version-file: go.mod` does not become a check.
It tells you the toolchain version is pinned by `go.mod`, and that the local
gate should be running that version. Either record it in a check that asserts
the toolchain version, or note it in the manifest header. Do not create a check
that "installs Go."

### Steps that do not survive the move

| Config step | Why | Do this |
|---|---|---|
| `actions/checkout` | you already have the tree | drop |
| caching steps | the runner caches pinned tools itself | drop |
| artifact upload/download | no artifact store | drop, or write to a local path |
| matrix strategy | one machine | run the combination you have; **record that it is one of N** |
| anything using `secrets.*` | you do not have them | leave out and say so |
| deploy / publish / release | not a gate | leave out |
| a step calling the forge API | needs an account and a token | reimplement against local git if the same information is there — see below |

### Reimplementing an API-driven check

Some gates read repository metadata through the provider's API. Frequently the
same information is available from git locally, which makes the check both
portable and faster:

| API-driven check | Local equivalent |
|---|---|
| sign-off (DCO) on PR commits | `git rev-list --no-merges $(git merge-base HEAD origin/main)..HEAD` and check each message |
| commit-message convention | same range, match the subject lines |
| changed-files filters | `git diff --name-only <base>...HEAD` |
| PR size / file-count limits | same diff |
| branch-name policy | `git rev-parse --abbrev-ref HEAD` |

`examples/go-sacp/gates.yaml` has a worked sign-off check built exactly this
way. Note the difference honestly: the branch-vs-base commit range is the same
set as a PR's commit list in normal use, but not by construction — a rewritten
base or an unusual merge history can diverge.

## Preserve the pins exactly

Whatever version the old config pinned, carry that number over verbatim. This is
the single most important part of the import: it is what makes "the gate now
runs locally" true rather than "a similar gate now runs locally." A scanner one
minor version off is a different ruleset.

If a step installed a tool by downloading a release, that download command
becomes `fetch:` almost unchanged — it already produces a binary at a known
path, and the runner supplies `$VF_TOOL_DEST`, `$VF_TOOL_VERSION` and
`$VF_CACHE_DIR`.

## Preserve the reasoning

CI files often carry the only written explanation of why a check exists — a
comment block above the job saying what breaks if it stops running. Move that
into `why:`. It survives, `--explain` surfaces it, and the next person to ask
"can we drop this check?" gets an answer instead of a shrug.

## After importing

1. Run the gate. Expect SKIPs on the first run — those name the tools you have
   not installed yet. That is the correct result and a to-do list.
2. Run the old CI config's checks by hand once, if you still can, and compare.
   Discrepancies are real findings.
3. Write down what did **not** come across: matrix coverage, secret-dependent
   steps, deploys, anything you dropped. Put it in the manifest header where
   people will see it.
4. Only then remove or disable the old config, and only if you want to. Keeping
   it costs nothing while it is not running, and it documents where the checks
   came from.
