# Licensing and provenance (Phase 6)

What has to be true about licenses before the first public push. General information, not legal advice.

## Contents

- Choosing a license
- LICENSE and NOTICE
- REUSE: a license for every file
- Vendored and copied code
- Dependencies
- DCO or CLA

## Choosing a license

| License | In one line | Choose it when |
|---|---|---|
| MIT | Permissive, minimal, no explicit patent grant | You want the widest reuse and the least ceremony |
| Apache-2.0 | Permissive with an explicit patent license, patent retaliation, and NOTICE obligations | You want enterprise adoption and patent clarity — the usual choice for tooling meant to be vendored |
| AGPL-3.0 | Strong copyleft that extends to network use (§13: users interacting over a network must be offered the source) | You want modifications offered back, including for hosted use, and accept that some companies will not touch it |

Pick before the first public commit lands: relicensing later needs every contributor's agreement unless your inbound terms (below) already grant it.

## LICENSE and NOTICE

- `LICENSE` at the root holds the full license text. A subdirectory under a different license carries its own `LICENSE` (Apache-2.0 directories inside an AGPL-3.0 repository, for example).
- **Apache-2.0 §4(d):** if the work includes a `NOTICE` file, anyone redistributing a derivative must carry its attribution notices forward. Keep `NOTICE` short — a copyright line and the attributions bundled third-party components require. Never put license text in it.

## REUSE: a license for every file

The FSFE REUSE Specification (version 3.3, November 2024) makes every file's copyright and license machine-readable, so nobody has to guess. Tooling: `reuse` 6.x (`pip install reuse`).

```sh
reuse download Apache-2.0                              # LICENSES/Apache-2.0.txt
reuse annotate --copyright="Your Name" --license=Apache-2.0 --recursive .
reuse lint                                             # must report compliance with REUSE 3.3
```

- Files that cannot carry a comment get a `.license` sidecar, or an entry in `REUSE.toml`. `REUSE.toml` replaced `.reuse/dep5`, which is deprecated (`reuse convert-dep5` migrates it); never keep both.
- Since reuse 6.0, `lint` scans whole files, so text that merely looks like a license header (in documentation, say) may need `REUSE-IgnoreStart` / `REUSE-IgnoreEnd` markers.
- In CI: `fsfe/reuse-action`, pinned to a full commit SHA.

## Vendored and copied code

Dependency scanners see manifests; they do not see code pasted into the tree. Find it before someone else does:

- **ScanCode Toolkit** (`pip install scancode-toolkit`) detects license and copyright text file by file; run it over the tree and read every non-matching result.
- **Copied snippets.** Stack Overflow contributions are CC BY-SA, which is incompatible with a permissive project's license. Search for, and rewrite or remove, anything copied.
- **Compatibility runs one way.** Apache-2.0 code can go into a GPL-3.0 project; GPL, LGPL, AGPL or GPL-2.0-only code cannot go into an Apache-2.0 or MIT project without relicensing the combination. Running a copyleft *tool* (TruffleHog is AGPL-3.0) does not affect your license; vendoring its source would.
- Keep third-party code in a clearly marked directory (`third_party/`), each with its own license file, and list it in `NOTICE` where required.

## Dependencies

Generate an SBOM and check its licenses against a policy:

```sh
syft dir:. -o spdx-json > sbom.spdx.json
grant check sbom.spdx.json          # Anchore grant: exits 1 on anything outside policy
```

grant (v0.6.8 as of October 2026, pre-1.0) denies everything until a policy (`.grant.yaml`: `allow`, `ignore-packages`, `require-license`, `require-known-license`) allows it. **Caveat:** releases up to v0.6.8 treat an SPDX `OR` expression like `AND`, so `MIT OR GPL-3.0-only` is denied even when MIT is allowed; the fix is on grant's main branch, not yet released. Review those by hand. vibeforge-gate carries license and SBOM checks for a standing gate.

## DCO or CLA

| | DCO | CLA |
|---|---|---|
| Mechanism | `Signed-off-by:` trailer per commit (`git commit -s`) | A signed agreement per contributor |
| Grants | A certification that the contributor may submit the work | Whatever it says — often a broad license, sometimes relicensing rights |
| Cost | Nearly none | A signing process and a bot |
| Typical for | Linux, CNCF projects, personal projects | Google, Microsoft, foundation and commercial projects that may relicense |

A DCO alone certifies the right to contribute under the project's license. If you may ever relicense or donate the project, say so in the inbound terms — the From_the_Forge `DCO` files are a worked example, with explicit inbound = outbound Apache-2.0 terms — or use a CLA. Decide before the first outside contribution: the right is lost the moment one merges without it.
