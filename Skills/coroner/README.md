# The Coroner Protocol

**Adversarial architecture and code review, as a subagent that cannot edit.**

Most review tooling is optimized to produce a patch. This one is optimized to produce a
**diagnosis** — severity-ranked findings, each bound to a file and line, plus the
pre-mortem exhibits and refactor sequencing that tell you which findings actually matter.

It ships as a read-only subagent (`Read`, `Grep`, `Glob` — no `Edit`, no `Write`, no
`Bash`). That restriction is the design, not a limitation: a reviewer able to edit will
start fixing before it has finished diagnosing, and you end up with three patched
symptoms and no map of the disease. Review is one act. Patching is a separate act, with a
separate invocation, after you have decided what is worth patching.

## What it produces

- A **verdict** and one-paragraph system sketch
- A **System-of-Record map** — which store owns which entity, and by what evidence
- **Severity-ranked findings** (SEV-1…), each citing the file and line it rests on
- **Pre-mortem exhibits** — the specific way this design fails, before it does
- A **strangler refactor plan** — sequenced, so step one is safe to take on a Monday

## Stack focus

The lenses are sharpest on **React · shadcn/ui · Prisma · MongoDB · PostgreSQL +
pgvector**, with a centerpiece interrogation of dual-store (Mongo + Postgres) write
paths: who is authoritative, what keeps the copies coherent, and whether the answer lives
in code or in someone's head.

It degrades honestly outside that stack. §5.1 collapses when only one store is present,
and the protocol detects the actual framework rather than assuming one — a directive it
states plainly as **never review phantom architecture**.

## Usage

```
/coroner:coroner-review                                  # whole repo, standard depth
/coroner:coroner-review src/services triage              # PR-scale
/coroner:coroner-review "entire repository" forensic     # pre-migration audit
```

Or invoke the subagent directly by asking for an architecture review, audit, or
pre-refactor assessment — the `coroner` agent description routes to it.

The command's §0 block is where you **pin** what the agent should not have to
rediscover — deployment target, database topology, house rules, non-goals. Pinning is
worth doing: it stops the agent reviewing architecture that does not exist in your repo.
See the worked example in [`commands/coroner-review.md`](commands/coroner-review.md).

## Depth

| Depth | Covers | Use for |
|---|---|---|
| `triage` | Phases 0–1, verdict, top-ten findings, top pre-mortem | Pull-request scale |
| `standard` | Everything | The default |
| `forensic` | Everything, plus per-module autopsies and requested profiling evidence | Pre-migration, pre-acquisition |

## Tuning

- Reviews reading as theatrical? Drop `PERSONA_INTENSITY` to `0`. The evidentiary
  machinery is persona-independent by design.
- House rules belong in the protocol's §3 doctrine as numbered entries, each with a
  one-line consequence. Stack-specific checks belong in the §5 lenses as bullets.
- **Leave §2 and §7 alone.** They are the calibration and evidence rules — what keeps the
  review honest rather than merely confident.

## Install

See [`INSTALL.md`](INSTALL.md).

## License

Apache-2.0 — see [`LICENSE`](LICENSE). Kept permissive so it can be vendored into
repositories that could not accept a copyleft dependency. Contributions are inbound =
outbound per [`DCO`](DCO); sign off your commits with `git commit -s`.
