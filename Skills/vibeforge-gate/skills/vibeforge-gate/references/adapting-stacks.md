# Adapting to a stack

How to work out what a repository's gate should contain, where its version pins
come from, and what to do when a check will not run locally.

## Detect first, ask second

| Marker | Stack | Tool directory for `path_prepend` |
|---|---|---|
| `go.mod` | Go | `~/go/bin` |
| `package.json` | Node / TS | `./node_modules/.bin` |
| `pyproject.toml`, `requirements.txt`, `setup.cfg` | Python | `./.venv/bin` |
| `Cargo.toml` | Rust | `~/.cargo/bin` |
| `*.csproj`, `*.sln` | .NET | — |
| `Gemfile` | Ruby | `./bin` |
| `pom.xml`, `build.gradle` | JVM | — |
| `Dockerfile`, `compose.yaml` | containers | — |

A repo is often several at once — a Go service with a TypeScript frontend and a
Python sidecar is one repo and three stacks. Give each its own checks with a
`dir:`, and prefix the ids (`api-test`, `web-lint`, `sidecar-test`) so the
summary stays readable.

## Where the bar already exists

Do not invent a merge bar for a repo that already states one. In order of
authority:

1. **An existing hosted-CI config** — the checks people currently expect to
   pass. See `importing-existing-ci.md`.
2. **`Makefile` / `justfile` / `Taskfile` / `package.json` scripts** — a `test`,
   `lint`, `check`, `verify` or `ci` target is the maintainers' own statement of
   the bar. Prefer calling that target over re-deriving its command, so the two
   cannot drift.
3. **`CONTRIBUTING.md`, `CLAUDE.md`, PR templates** — frequently carry rules no
   automation ever enforced ("run gofmt before committing"). These are worth
   real attention: they are the checks the team already agreed on and has been
   enforcing by memory. Automating one is usually the highest-value check you
   can add.
4. **Config files implying a tool** — `.eslintrc`, `ruff.toml`, `.golangci.yml`,
   `rustfmt.toml`. The tool is configured, so someone intended it to run.

## Where version pins come from

Never invent a pin. Take it from what the repo already records:

| Stack | Pin source |
|---|---|
| Go | `go.mod` (`go` directive, tool deps); `go install pkg@vX.Y.Z` in docs or CI |
| Node | `package.json` `devDependencies` + the lockfile; `engines` for the runtime |
| Python | `requirements*.txt`, `poetry.lock`, `uv.lock`, `.pre-commit-config.yaml` `rev:` |
| Rust | `Cargo.toml`, `rust-toolchain.toml` |
| Standalone binaries | whatever the old CI config pinned — carry that number over verbatim |

If a tool was genuinely unpinned before, pin it to the version you are running
now, and say in the commit message that you did. An arbitrary-but-recorded pin
is better than a floating one; the point is that changes become visible.

**Tools installed by the project's own package manager need no `tool_version:`.**
An `eslint` in `node_modules` is already pinned by the lockfile — declare
`path_prepend: ./node_modules/.bin` and call it directly. Reserve `tool:` +
`tool_version:` for binaries that live outside the project's dependency graph.

## Per-stack starting points

Written, copy-ready versions of all of these are in `catalog/`. This is the
reasoning about which to pick.

### Go
`gofmt -l` (empty output required), `go build ./...`, `go vet ./...`,
`go test ./...`, and `go mod verify` with `-mod=readonly` so an unrecorded
dependency fails rather than silently resolving. `go test -race` is valuable and
slow — `stage: push` with a `skip_env`, or `manual`. Add `staticcheck` or
`golangci-lint` for lint, `govulncheck` for vulnerabilities.

### Node / TypeScript
The lockfile is the pin, so use `npm ci` (not `install`) and call binaries from
`node_modules/.bin`. `tsc --noEmit` for types, `eslint`, `prettier --check`,
then the test runner. Keep the install step out of the gate if it is slow —
gate on a stale-lockfile check instead (`npm ci --dry-run`).

### Python
Everything runs inside the project environment, so `path_prepend: ./.venv/bin`
and let the venv's pins do the work. `ruff check` + `ruff format --check` (or
black/flake8), `mypy`, `pytest`. Note that `pytest` exits 5 on "no tests
collected" — that is a real failure to surface, not a pass.

### Rust
`cargo fmt --check`, `cargo clippy -- -D warnings`, `cargo test`. Pin the
toolchain with `rust-toolchain.toml` and let rustup honor it rather than pinning
each tool separately. `cargo audit` for advisories.

### Mixed repos
One check per concern per stack, each with its own `dir:`. Resist a single
"run everything" check — when it fails you lose the ability to see *what*
failed, which is most of the value of a summary.

## Checks worth adding that hosted CI often charged for

`catalog/supply-chain.yaml` has these written out. Briefly:

| Capability | Local, free, pinnable |
|---|---|
| Secret scanning | `gitleaks`, `trufflehog` |
| Static analysis / SAST | `semgrep` (OSS rules), `staticcheck`, `bandit`, `clippy` |
| Dependency vulnerabilities | `osv-scanner`, `govulncheck`, `pip-audit`, `cargo-audit`, `npm audit` |
| SBOM + component scan | `syft`, `grype` |
| License policy | `go-licenses`, `license-checker`, `cargo-deny` |

`osv-scanner` is the broadest single win — it reads lockfiles for most
ecosystems, so one check covers a mixed repo.

## When a check cannot run locally

Some steps genuinely do not reproduce. Be honest about which, and pick the
option that keeps the gate's claim true:

| Situation | What to do |
|---|---|
| Needs a secret or production credential | Leave it out. Say so in the manifest header. A gate that cannot run its own checks is worse than one that never claimed to. |
| Needs a service (database, broker) | `probe:` for it. Present → runs; absent → blocking SKIP naming how to start it. Do not silently pass. |
| Slow (integration, e2e, matrix) | `stage: manual`, or `stage: push` + `skip_env:`. |
| Cross-OS or cross-version matrix | Not reproducible on one machine. Run the one combination you have and **write down that it is one of N**. Do not imply matrix coverage. |
| Container build | Runs if a daemon is present — `probe: docker info`. Absent → SKIP. |
| Publish / deploy | Not a gate. Leave it out. |

The rule in every row: a check that cannot run is a **SKIP**, never a silent
pass and never a deleted line.
