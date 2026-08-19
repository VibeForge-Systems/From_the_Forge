---
description: Adversarial architecture review of a path or the whole repo, via the coroner subagent
argument-hint: "[scope path(s) or \"entire repository\"] [triage|standard|forensic] — defaults to whole repo, standard"
---

Invoke the `coroner` subagent (Agent tool, `subagent_type: "coroner"`) against this
repository. The protocol is its system prompt; do not restate or summarize it here, and
do not perform the review yourself in the main context — the isolation is the point.

## Parsing `$ARGUMENTS`

If the final token is `triage`, `standard`, or `forensic`, it is `DEPTH` and everything
before it is `SCOPE`. Otherwise the whole string is `SCOPE` and `DEPTH` defaults to
`standard`. Empty `$ARGUMENTS` means `SCOPE: entire repository`.

- `triage` — Phases 0–1 only. Right for pull-request-scale scope.
- `standard` — everything. The default.
- `forensic` — everything, plus per-module autopsies and the specific profiling evidence
  it would request for each SEV-1/SEV-2 query finding. Right for pre-migration or
  pre-acquisition audits. Expensive; do not reach for it by default.

## The §0 config

Pass the subagent a filled §0 block. Anything you cannot establish from repo evidence,
leave as the placeholder and say so explicitly — the agent resolves it in Phase 0:

```
SCOPE:              <from $ARGUMENTS>
DEPTH:              <from $ARGUMENTS, default standard>
PERSONA_INTENSITY:  1
DEPLOY_TARGET:      serverless | long-lived-node | edge+node | unknown
DB_TOPOLOGY:        mongo+postgres | postgres-only | mongo-only | unknown
KNOWN_CONSTRAINTS:  <team size, deadlines, compliance regime, decisions already locked>
NON_GOALS:          <explicitly out of scope>
```

`DEPLOY_TARGET` inverts several data-layer verdicts — connection pooling, client
lifecycle, cold-start behavior — so a wrong pin is worse than `unknown`. When in doubt,
pass `unknown` and let Phase 0 resolve it from the Dockerfile, platform config, or
adapter setup.

## Pinning, and why

Facts you pin are facts the agent does not spend context rediscovering, and — more
importantly — cannot get wrong. Pin what is stable and load-bearing; leave the rest.
Always instruct it to **flag contradicting evidence rather than trusting the pin**, so a
stale pin surfaces as a finding instead of silently corrupting the review.

Pin especially where a lens would otherwise be applied to architecture that does not
exist. §5.1 (dual-store interrogation) is the big one: it applies only when both MongoDB
and Postgres are present, and reviewing a phantom second store wastes the whole pass.

An example of a well-pinned invocation, from a single-store Express + Prisma repo:

```
DEPLOY_TARGET:  long-lived-node        # Docker containers, not serverless
DB_TOPOLOGY:    postgres-only          # no MongoDB — collapse §5.1 per its own clause

- Frontend is a Vite SPA + react-router. No Next.js, no App Router, no RSC, no SSR;
  do not review a rendering model that does not exist here.
- pgvector is installed but zero vector columns are defined. Verify, then say so in
  one line and skip §5.3 rather than reviewing an absent vector lifecycle.
- Prisma schema is multi-file; cross-file references are deliberately loose FKs with
  app-level integrity only. A documented tradeoff — judge its consequences, not its
  existence.
- Read CLAUDE.md and any nested AGENTS.md first. Many apparent violations are
  documented house rules; cite the rule if you are arguing against one.

NON_GOALS: do not propose migrating off the current stack; do not review generated or
vendored artifacts (dist/, node_modules/, generated docs, migration history).
```

## Handling the report

The subagent is read-only — `Read`, `Grep`, `Glob`. It has no `Bash` and no `Write`, so
it cannot run a test suite, execute a query, or save its own output. It returns the
report as its final message.

- **Relay it in full.** Do not compress a severity-ranked report into a few bullets; the
  evidence citations are the load-bearing part.
- **Do not start fixing.** Review is one act, patching is a separate act with a separate
  invocation. Ask before touching anything the report names.
- Offer to save the report to a file. Do not save it unprompted.
