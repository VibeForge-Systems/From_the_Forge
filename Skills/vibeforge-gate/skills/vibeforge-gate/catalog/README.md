# Catalog

Written, copy-ready checks. Take a block, paste it into `.vibeforge/gates.yaml`,
adjust the paths, keep the `why:`.

| File | Contents |
|---|---|
| `universal.yaml` | Works in any repo: sign-off (DCO), commit subjects, conflict markers, oversized files, trailing whitespace, CRLF line endings, JSON/TOML validity, spell check (the shadow-mode-with-deadline example), shell syntax, shellcheck, workflow lint for coexisting hosted CI, and a check that the manifest itself still parses |
| `languages.yaml` | Go (format, build, race, lint, mod tidy, codegen freshness), Node/TypeScript, Python, Rust, containers (Dockerfile lint, image build) |
| `supply-chain.yaml` | Secret scanning, SAST, dependency vulnerabilities, license policy, SBOM freshness — the tier most often sold as a paid add-on |

These are not a menu to adopt wholesale. Every check costs runtime on every
push, and a gate people find slow or noisy is a gate people bypass. Take the
ones you can justify.

## Two rules when copying

**Keep the `why:`.** It is what `--explain` shows, and it is the answer when
someone later asks whether the check can be dropped. A check nobody can justify
is one that gets disabled the first time it is inconvenient.

**Check the pin.** Versions here are starting points and will age. Look up the
current release, pin deliberately, and treat a bump as a reviewable one-line
commit. If the repo already pinned that tool somewhere — old CI config, a
lockfile, `.pre-commit-config.yaml` — carry that number over verbatim instead.

## Introducing a check that would fail today

Add it with `enforce: false`. It runs, reports `WARN`, and does not block. Tune
it, fix or accept what it finds, then promote it by deleting that line.

A check introduced in blocking mode on day one, on a codebase that fails it, is
a check someone will disable — usually along with its neighbours. Give any
advisory check a deadline; one that lives there for a year is decoration.
