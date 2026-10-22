# Identities and internal details

What `scripts/sweep-leaks.py` looks for beyond credentials, why each class matters, and how to keep the lists that drive it.

## Contents

- Identities
- The private deny list
- Internal details
- Files that are findings by name
- Binary content, notebooks, images
- The allow file
- Writing documentation that never matches

## Identities

Every author, committer and tagger email, and every address in a `Signed-off-by`, `Co-authored-by` or similar trailer, is published with the commit. The usual accident is not the personal address you chose to use — it is the **employer or client address** a work-configured machine stamped on a side-project commit.

- **GitHub noreply.** `ID+USERNAME@users.noreply.github.com` for accounts created after 18 July 2017 (older accounts get the ID form by re-selecting "Keep my email addresses private"). Only the ID form survives a username change. Set it per repository: `git config user.email ID+USERNAME@users.noreply.github.com`.
- **GitHub's push block is narrow.** "Block command line pushes that expose my email" checks only the **most recent commit** of a push, and only against your own private addresses. It is no substitute for checking every outgoing commit.
- **The publishable list.** Give `--identities` a file of extended regexes for identities that may appear — usually GitHub noreply plus any address you have deliberately made public. Everything else is a finding. The file lists only already-public identities, so it is safe to keep with the project (vibeforge-gate's `identity-check` reads `.vibeforge/allowed-identities` the same way).
- **Fix** with `--mailmap` during the rewrite (`history-rewrite.md`); a committed `.mailmap` only changes display.
- **Prevent** the next one: switch identity by directory in `~/.gitconfig`, so a personal checkout can never commit with the work address:

  ```
  [includeIf "gitdir:~/personal/"]
      path = ~/.gitconfig-personal      # sets user.email to the noreply address
  ```

## The private deny list

The names you must not publish — your employer, its clients, internal product and project codenames, internal domains — are the findings no tool can guess. Start from the shipped example and write them down:

```sh
mkdir -p ~/.config/vibeforge
cp templates/leak-denylist.example ~/.config/vibeforge/leak-denylist
$EDITOR ~/.config/vibeforge/leak-denylist        # fill in section 1
```

The example's section 1 is yours to fill in (employer, clients, codenames, internal domains). The rest is generic and active: the classic document confidentiality and privilege markings, SaaS tenant hostnames that carry a customer's name (ServiceNow, Salesforce, Atlassian Cloud, SharePoint, Entra ID, Snowflake, Azure DevOps), AWS ARNs and Azure subscription paths, and — commented out — ServiceNow record numbers. Across one real repository's full history (every ref, binaries included) the active generic set raised no false positives.

- **One list for both tools.** vibeforge-gate's `denylist-scan` and `sweep-leaks.py` read the same format and the same places: `~/.config/vibeforge/leak-denylist`, `$VF_LEAK_DENYLIST`, and newline-separated `$VF_LEAK_DENYLIST_PATTERNS` (for agent environments that hold secrets in variables). The sweep also reads `$RPR_DENYLIST` and `~/.config/repo-publication-readiness/denylist`, and with `--denylist` reads only the files named. Its output names every source it loaded; with none, it says these names were **not** checked.
- **Write portable patterns.** One extended regex per line, case-insensitive, `#` for comments, in the subset both `grep -E` and Python's `re` accept: `[0-9]` not `\d`, `(^|[^a-z0-9])word([^a-z0-9]|$)` not `\bword\b`, no lookarounds, no `(?i)`. Write a literal dot as `[.]` and a separator as `[ _-]` — they also keep a pattern from matching its own text if a copy of the list is ever scanned.
- **It never lives inside a repository.** Committing it publishes exactly the names it exists to protect. Both tools refuse a deny list inside the repository they check.
- **Reports name matched terms** (they are local files outside the repository). The gate's `denylist-scan` prints only a count and the command to see them, because gate output can end up in shared logs.

## Internal details

| Class | Pattern (summary) | Why it matters |
|---|---|---|
| Private and CGNAT IPv4 | RFC 1918 (10/8, 172.16/12, 192.168/16) and RFC 6598 (100.64/10, used by Tailscale and carrier NAT) | A map of internal topology; often next to a port and a service name |
| Unique-local IPv6 | fc00::/7 | Same, for IPv6 |
| Internal hostnames | a name ending in `.lan`, `.internal`, `.intranet`, `.corp`, `.localdomain`, `.home.arpa`, `.local` | Names hosts that should not be on the internet's radar |
| UNC shares | `\\server\share`-shaped paths | Names file servers and their shares |
| Home paths | `C:\Users\<name>`, `/home/<name>`, `/Users/<name>` | Names a real account and the machine layout |
| Emails in content | addresses in added lines | Contact details of colleagues and clients |

Conventions everyone shares are skipped: `host.docker.internal`, `cluster.local` service names, `localhost.localdomain`, `\\server\share`-style placeholders, generic home names (`user`, `runner`, `ubuntu`…), RFC 2606/6761 reserved domains (`example.com`, `.example`, `.test`, `.invalid`, `.localhost`), and file names that look like addresses (`icon@2x.png`). Attribution files (`NOTICE`, `AUTHORS`, `LICENSE`…) and lockfiles are exempt from the email class: they exist to carry third-party contacts.

## Files that are findings by name

Added anywhere in history: `.env` and `*.env` (examples, samples and templates exempt), `id_rsa`/`id_ed25519` and other private keys (`.pub` exempt), `*.pem`, `*.key`, `*.p12`, `*.pfx`, `*.jks`, `*.keystore`, `*.kdbx`, `*.ovpn`, `*.tfstate`, `*.har` (HTTP archives carry cookies and bearer tokens), `*.pcap`, `credentials*.json`, service-account JSON, `.netrc`, `.pypirc`, `.htpasswd`, `.git-credentials`. Remove them with `--invert-paths` even if their content scans clean — the content scan may simply lack the rule.

## Binary content, notebooks, images

- **Binary blobs** (git's own test: a NUL byte in the first 8000 bytes) up to `--binary-cap-mb` are swept as extracted printable strings, so a SQLite fixture with a real address or a private IP is found. Fonts, media and compressed archives are skipped as noise. Strings from binaries run fields together (`user@example.invalidiMessage`); the sweep judges every shorter candidate too.
- **Office documents** (`.docx`, `.xlsx`, `.pptx`, `.odt`…) are opened as the zip they are: text parts are swept, and authorship metadata (`dc:creator`, `cp:lastModifiedBy`, `Company`, `Manager`) is reported — the classic way a document names its author's employer.
- **JPEGs** with an EXIF GPS block are reported (a heuristic over the APP1 segment). Confirm with `exiftool`, strip with `exiftool -all=` before the rewrite, or remove the image.
- **Notebooks** committed with cell outputs carry data, printed tokens and local paths. Clear them (`nbstripout`, or `jupyter nbconvert --clear-output --inplace`) in a commit, and remove the dirty versions from history.

## The allow file

Reviewed findings that are fine to publish go in a TOML file passed with `--allow`, each with a reason:

```toml
[[allow]]
category = "large-blob"
path = "assets/fonts/*.otf"
reason = "OFL-licensed fonts, attributed in NOTICE"

[[allow]]
category = "email"
path = "tests/*"
match = "@b\\.com$"
reason = "synthetic address in a unit test"
```

`category` is one of the bracketed headings the sweep prints (or `*`); `path` is a glob; `match` (optional) is a regex tested against the finding. An entry without a `reason` is rejected. Keep the allow file with the project — it documents decisions, not secrets.

## Writing documentation that never matches

Use the addresses and names reserved for documentation, and examples will never collide with the sweep or with anyone's real network:

- IPv4: 192.0.2.0/24, 198.51.100.0/24, 203.0.113.0/24 (RFC 5737). IPv6: 2001:db8::/32 (RFC 3849).
- Domains: `example.com`, `example.org`, `example.net`, and anything under `.example`, `.test` or `.invalid` (RFC 2606).
- Paths: `C:\Users\USERNAME`, `/home/user`.
