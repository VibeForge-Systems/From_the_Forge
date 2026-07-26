#!/usr/bin/env bash
#
# .vibeforge/gate.sh — this repository's merge bar, owned by this repository.
#
# WHY THIS EXISTS. A hosted CI service that stops running — because the minutes
# ran out, because the account lapsed, because the check you needed sits behind
# a paid tier — takes its gates with it. This runner puts the gates back under
# local control: `.vibeforge/gates.yaml` declares them, this script executes
# them, and a git hook refuses work that does not pass. Nothing here phones
# home, needs an account, or costs anything to run.
#
# CONTRACT. Green here is the merge bar. That claim is only worth something if
# the runner never reports a check as passing when it did not actually execute:
#
#   * A check whose tool is MISSING is reported SKIP, loudly, and a SKIP makes
#     the whole run exit non-zero. A silent coverage gap — "it went green
#     because the check never ran" — is the exact failure this guards against.
#   * A check whose tool is present but at the WRONG VERSION is also a SKIP.
#     A gate that runs a different scanner than the one you pinned is not the
#     gate you reviewed.
#   * A DELIBERATE skip (the operator set the check's skip_env) is reported
#     separately and does NOT block — you chose it, with the choice on screen.
#   * A check declared `enforce: false` is advisory: it runs, a failure is
#     reported WARN, and it does not block. That is shadow mode, for a check
#     you are still tuning. It is a lie only if you forget it is on.
#
# DEPENDENCIES: bash 4+, coreutils, git. No YAML library, no runtime, no
# package manager. The manifest parser accepts a deliberately small YAML subset
# and errors out with a line number on anything outside it, so a manifest can
# never be silently misread.
#
# Exit codes:  0  every selected check passed
#              1  at least one check FAILED
#              2  at least one check was SKIPPED (coverage gap vs. the declared
#                 bar) — override with VF_GATE_ALLOW_SKIP=1
#              3  usage / manifest error
#
# Part of vibeforge-gate. Regenerate or extend via the `vibeforge-gate` skill.

set -uo pipefail

VF_VERSION="1.0.0"

# --- locate repo + manifest ---------------------------------------------------
ROOT="$(git rev-parse --show-toplevel 2>/dev/null || true)"
if [ -z "$ROOT" ]; then
  ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
fi
cd "$ROOT" || exit 3

MANIFEST="${VF_GATE_MANIFEST:-$ROOT/.vibeforge/gates.yaml}"
CACHE_DIR="${VF_GATE_CACHE:-${XDG_CACHE_HOME:-$HOME/.cache}/vibeforge-gate}"

# --- colors -------------------------------------------------------------------
if [ -t 1 ] && [ -z "${NO_COLOR:-}" ] && [ "${VF_GATE_NO_COLOR:-0}" != "1" ]; then
  bold=$'\033[1m'; red=$'\033[31m'; grn=$'\033[32m'; ylw=$'\033[33m'
  cyn=$'\033[36m'; dim=$'\033[2m'; rst=$'\033[0m'
else
  bold=""; red=""; grn=""; ylw=""; cyn=""; dim=""; rst=""
fi

die() { printf '%sgate: %s%s\n' "$red" "$1" "$rst" >&2; exit 3; }
say() { printf '\n%s==> %s%s\n' "$bold" "$1" "$rst"; }

# =============================================================================
# Manifest parser — a strict, small YAML subset.
#
# Accepted, and nothing else:
#   meta:                     a map of scalars at indent 2
#   checks:                   a sequence of maps; `  - key: value` opens an
#                             item, `    key: value` adds to it
#   key: |                    block scalar; following lines indented deeper are
#                             taken verbatim (common indent stripped)
#   # comment                 only when # is the first non-space character
#
# Values may be bare, 'single-quoted', or "double-quoted" (outer quotes are
# stripped; no escape processing). Inline comments are NOT stripped, so a
# command containing # survives intact.
#
# Fields land in F["<index>.<field>"]; N holds the number of checks.
# =============================================================================
declare -A F=()
declare -A META=()
N=0

parse_manifest() {
  local file="$1"
  [ -f "$file" ] || die "no manifest at $file (create one, or set VF_GATE_MANIFEST)"

  local lineno=0 state=top idx=-1
  local blk_key="" blk_owner="" blk_indent=-1 blk_val="" in_blk=0
  local line raw indent content key val

  flush_block() {
    [ "$in_blk" -eq 1 ] || return 0
    # strip the single trailing newline a block scalar accumulates
    blk_val="${blk_val%$'\n'}"
    if [ "$blk_owner" = "meta" ]; then META["$blk_key"]="$blk_val"; else F["$blk_owner.$blk_key"]="$blk_val"; fi
    in_blk=0; blk_val=""; blk_indent=-1
  }

  # unquote VALUE
  unq() {
    local v="$1"
    case "$v" in
      \"*\") v="${v#\"}"; v="${v%\"}" ;;
      \'*\') v="${v#\'}"; v="${v%\'}" ;;
    esac
    printf '%s' "$v"
  }

  while IFS= read -r raw || [ -n "$raw" ]; do
    lineno=$((lineno + 1))
    line="${raw%$'\r'}"

    # ---- inside a block scalar --------------------------------------------
    if [ "$in_blk" -eq 1 ]; then
      if [ -z "${line//[[:space:]]/}" ]; then
        blk_val+=$'\n'
        continue
      fi
      local lead="${line%%[! ]*}"
      if [ "$blk_indent" -lt 0 ]; then
        blk_indent="${#lead}"
      fi
      if [ "${#lead}" -ge "$blk_indent" ]; then
        blk_val+="${line:$blk_indent}"$'\n'
        continue
      fi
      flush_block
      # fall through and reprocess this line normally
    fi

    # ---- blank / comment ---------------------------------------------------
    [ -z "${line//[[:space:]]/}" ] && continue
    case "${line#"${line%%[! ]*}"}" in \#*) continue ;; esac

    local lead="${line%%[! ]*}"
    indent="${#lead}"
    content="${line:$indent}"

    # ---- top level ---------------------------------------------------------
    if [ "$indent" -eq 0 ]; then
      case "$content" in
        meta:)   state=meta ;;
        checks:) state=checks ;;
        *) die "line $lineno: expected 'meta:' or 'checks:' at column 0, got: $content" ;;
      esac
      continue
    fi

    # ---- meta --------------------------------------------------------------
    if [ "$state" = meta ]; then
      [ "$indent" -eq 2 ] || die "line $lineno: meta entries must be indented exactly 2 spaces"
      case "$content" in *:*) ;; *) die "line $lineno: expected 'key: value' in meta" ;; esac
      key="${content%%:*}"; val="${content#*:}"; val="${val# }"
      if [ "$val" = "|" ] || [ "$val" = "|-" ]; then
        in_blk=1; blk_key="$key"; blk_owner="meta"; blk_indent=-1; blk_val=""
      else
        META["$key"]="$(unq "$val")"
      fi
      continue
    fi

    # ---- checks ------------------------------------------------------------
    if [ "$state" = checks ]; then
      case "$content" in
        "- "*)
          [ "$indent" -eq 2 ] || die "line $lineno: a check must start at indent 2 with '- '"
          idx=$((idx + 1)); N=$((idx + 1))
          content="${content#- }"
          ;;
        *)
          [ "$indent" -eq 4 ] || die "line $lineno: check fields must be indented exactly 4 spaces"
          [ "$idx" -ge 0 ] || die "line $lineno: field outside any check item"
          ;;
      esac
      case "$content" in *:*) ;; *) die "line $lineno: expected 'key: value' in a check" ;; esac
      key="${content%%:*}"; val="${content#*:}"; val="${val# }"
      if [ "$val" = "|" ] || [ "$val" = "|-" ]; then
        in_blk=1; blk_key="$key"; blk_owner="$idx"; blk_indent=-1; blk_val=""
      else
        F["$idx.$key"]="$(unq "$val")"
      fi
      continue
    fi

    die "line $lineno: content before 'meta:' or 'checks:'"
  done < "$file"
  flush_block

  [ "$N" -gt 0 ] || die "manifest declares no checks"

  # validate required fields + unique ids
  local i id seen=""
  for ((i = 0; i < N; i++)); do
    id="${F[$i.id]:-}"
    [ -n "$id" ] || die "check #$((i + 1)) has no 'id'"
    [ -n "${F[$i.run]:-}" ] || die "check '$id' has no 'run'"
    case " $seen " in *" $id "*) die "duplicate check id: $id" ;; esac
    seen="$seen $id"
  done
}

# =============================================================================
# Reporting
# =============================================================================
declare -a R_ID=() R_VERDICT=() R_NOTE=()
FAILED=0 BLOCKING_SKIP=0

record() { R_ID+=("$1"); R_VERDICT+=("$2"); R_NOTE+=("${3:-}"); }

verdict_pass()  { printf '%s   PASS%s  %s\n'  "$grn" "$rst" "$1"; record "$1" PASS; }
verdict_fail()  { printf '%s   FAIL%s  %s\n'  "$red" "$rst" "$1"; record "$1" FAIL "${2:-}"; FAILED=1; }
verdict_warn()  { printf '%s   WARN%s  %s — advisory (enforce: false), not blocking\n' "$ylw" "$rst" "$1"; record "$1" WARN "advisory"; }

# A coverage gap against the declared bar. Blocks.
verdict_skip()  {
  printf '%s   SKIP%s  %s — %s\n' "$ylw" "$rst" "$1" "$2"
  record "$1" SKIP "$2"; BLOCKING_SKIP=1
}
# A skip the operator asked for. Reported, does not block.
verdict_noted() {
  printf '%s   SKIP%s  %s — %s %s(deliberate; you asked for this)%s\n' "$ylw" "$rst" "$1" "$2" "$dim" "$rst"
  record "$1" skip "$2"
}

# =============================================================================
# Tool resolution — presence, pin, and fetch
# =============================================================================
VF_TOOL=""
RESOLVE_ERR=""

resolve_tool() {
  local i="$1"
  VF_TOOL=""; RESOLVE_ERR=""

  local tool="${F[$i.tool]:-}"
  [ -n "$tool" ] || return 0

  local ver="${F[$i.tool_version]:-}"
  local cached=""
  [ -n "$ver" ] && cached="$CACHE_DIR/$tool-$ver"

  # 1. the pinned binary we cached earlier
  if [ -n "$cached" ] && [ -x "$cached" ]; then VF_TOOL="$cached"; return 0; fi

  # 2. a copy on PATH — accepted only if it reports the pinned version
  if command -v "$tool" >/dev/null 2>&1; then
    local path; path="$(command -v "$tool")"
    if [ -z "$ver" ]; then VF_TOOL="$path"; return 0; fi
    local vc out
    vc="${F[$i.version_cmd]:-$tool --version}"
    out="$(bash -c "$vc" 2>&1 | head -3 || true)"
    if grep -qF -- "$ver" <<<"$out"; then VF_TOOL="$path"; return 0; fi
    RESOLVE_ERR="$tool on PATH is not the pinned v$ver (it reports: $(head -1 <<<"$out" | tr -d '\n'))"
  fi

  # 3. fetch the pinned build into the cache
  local fetch="${F[$i.fetch]:-}"
  if [ -n "$fetch" ] && [ -n "$cached" ]; then
    mkdir -p "$CACHE_DIR"
    local tmp="$cached.tmp.$$"
    if VF_TOOL_VERSION="$ver" VF_TOOL_DEST="$tmp" VF_CACHE_DIR="$CACHE_DIR" \
         bash -euo pipefail -c "$fetch" >/dev/null 2>&1 && [ -s "$tmp" ]; then
      chmod +x "$tmp" && mv "$tmp" "$cached" && { VF_TOOL="$cached"; return 0; }
    fi
    rm -f "$tmp"
    [ -n "$RESOLVE_ERR" ] || RESOLVE_ERR="could not fetch $tool v$ver (offline?)"
  fi

  [ -n "$RESOLVE_ERR" ] || RESOLVE_ERR="$tool not found${ver:+ (pinned v$ver)}"
  return 1
}

# =============================================================================
# Execution
# =============================================================================
STAGE_RANK() { case "$1" in commit) echo 1 ;; push) echo 2 ;; manual) echo 3 ;; *) echo 0 ;; esac; }

run_check() {
  # Two statements, not one: in a single `local a=$1 b=${x[$a]}`, the second
  # value is expanded before the first assignment takes effect, so it would read
  # the CALLER's i rather than this one.
  local i="$1"
  local id="${F[$i.id]}"
  local desc="${F[$i.desc]:-}"
  local dir="${F[$i.dir]:-.}"
  local enforce="${F[$i.enforce]:-true}"

  say "$id${desc:+ — $desc}"

  # operator-requested skip
  local se="${F[$i.skip_env]:-}"
  if [ -n "$se" ] && [ "${!se:-0}" = "1" ]; then
    verdict_noted "$id" "$se=1"
    return
  fi

  # required binaries
  local needs="${F[$i.needs]:-}" t
  if [ -n "$needs" ]; then
    for t in ${needs//,/ }; do
      if ! command -v "$t" >/dev/null 2>&1; then
        verdict_skip "$id" "'$t' not found — ${F[$i.install]:-install it and re-run}"
        return
      fi
    done
  fi

  # arbitrary readiness probe (a library, a daemon, a mounted path)
  local probe="${F[$i.probe]:-}"
  if [ -n "$probe" ] && ! bash -c "$probe" >/dev/null 2>&1; then
    verdict_skip "$id" "probe failed — ${F[$i.install]:-dependency missing}"
    return
  fi

  # pinned tool
  if ! resolve_tool "$i"; then
    verdict_skip "$id" "$RESOLVE_ERR — ${F[$i.install]:-install the pinned version}"
    return
  fi

  [ -d "$dir" ] || { verdict_skip "$id" "working dir '$dir' does not exist"; return; }

  # env: KEY=VALUE per line. Seeded with the runner's own exports so the array is
  # never empty (an empty "${a[@]}" is an unbound-variable error on bash < 4.4).
  local -a envs=("VF_TOOL=$VF_TOOL" "VF_ROOT=$ROOT" "VF_CACHE_DIR=$CACHE_DIR")
  local envblock="${F[$i.env]:-}" el
  if [ -n "$envblock" ]; then
    while IFS= read -r el; do
      [ -z "${el//[[:space:]]/}" ] && continue
      envs+=("$el")
    done <<< "$envblock"
  fi

  local rc=0
  ( cd "$dir" && env "${envs[@]}" bash -euo pipefail -c "${F[$i.run]}" ) || rc=$?

  if [ "$rc" -eq 0 ]; then
    verdict_pass "$id"
  elif [ "$enforce" = "false" ]; then
    verdict_warn "$id"
  else
    verdict_fail "$id"
  fi
}

# =============================================================================
# CLI
# =============================================================================
usage() {
  cat <<EOF
${bold}vibeforge-gate${rst} v$VF_VERSION — the merge bar, owned by this repo

  gate.sh                    run the full bar (stage: push)
  gate.sh --stage commit     run only the fast pre-commit subset
  gate.sh --all              run every check, including stage: manual
  gate.sh --only a,b         run just these check ids
  gate.sh --list             list declared checks and their stages
  gate.sh --explain [id]     print why each check exists, and its provenance
  gate.sh --fetch            pre-download every pinned tool into the cache

Environment:
  VF_GATE_ALLOW_SKIP=1       accept a coverage gap (exit 0 despite a SKIP)
  VF_GATE_MANIFEST=path      use a different manifest
  VF_GATE_CACHE=path         where pinned tools are cached
  VF_GATE_NO_COLOR=1         plain output
  <check>.skip_env           per-check deliberate skip (see --list)

Manifest: $MANIFEST
EOF
}

STAGE=push; ONLY=""; MODE=run
while [ $# -gt 0 ]; do
  case "$1" in
    --stage) STAGE="${2:-}"; shift 2 || true ;;
    --stage=*) STAGE="${1#*=}"; shift ;;
    --all) STAGE=manual; shift ;;
    --only) ONLY="${2:-}"; shift 2 || true ;;
    --only=*) ONLY="${1#*=}"; shift ;;
    --list) MODE=list; shift ;;
    --explain)
      MODE=explain; shift
      case "${1:-}" in ""|-*) ;; *) ONLY="$1"; shift ;; esac ;;
    --fetch) MODE=fetch; shift ;;
    -h|--help) usage; exit 0 ;;
    *) printf 'unknown argument: %s\n\n' "$1" >&2; usage >&2; exit 3 ;;
  esac
done
case "$STAGE" in commit|push|manual) ;; *) die "unknown stage '$STAGE' (commit|push|manual)" ;; esac

parse_manifest "$MANIFEST"

# meta.path_prepend — colon-separated dirs added to PATH for the whole run, so a
# tool installed somewhere a non-login shell does not look (~/.cargo/bin,
# ./node_modules/.bin, ./.venv/bin) is found rather than reported missing.
if [ -n "${META[path_prepend]:-}" ]; then
  IFS=':' read -r -a _vf_pp <<< "${META[path_prepend]}"
  for _p in "${_vf_pp[@]}"; do
    [ -z "$_p" ] && continue
    _p="${_p/#\~/$HOME}"
    PATH="$_p:$PATH"
  done
  export PATH
fi

selected() {
  local i="$1"
  local id="${F[$i.id]}"
  if [ -n "$ONLY" ]; then
    case ",$ONLY," in *",$id,"*) return 0 ;; *) return 1 ;; esac
  fi
  local cs; cs="${F[$i.stage]:-${META[default_stage]:-push}}"
  [ "$(STAGE_RANK "$cs")" -le "$(STAGE_RANK "$STAGE")" ]
}

case "$MODE" in
  list)
    printf '%s%s%s — %s checks\n\n' "$bold" "${META[name]:-$(basename "$ROOT")}" "$rst" "$N"
    for ((i = 0; i < N; i++)); do
      printf '  %-28s %s%-7s%s %s\n' "${F[$i.id]}" "$cyn" "${F[$i.stage]:-${META[default_stage]:-push}}" "$rst" "${F[$i.desc]:-}"
      [ -n "${F[$i.skip_env]:-}" ] && printf '  %-28s %sskip with %s=1%s\n' "" "$dim" "${F[$i.skip_env]}" "$rst"
      [ "${F[$i.enforce]:-true}" = "false" ] && printf '  %-28s %sadvisory — runs but does not block%s\n' "" "$dim" "$rst"
    done
    exit 0 ;;
  explain)
    for ((i = 0; i < N; i++)); do
      [ -n "$ONLY" ] && [ "${F[$i.id]}" != "$ONLY" ] && continue
      printf '\n%s%s%s — %s\n' "$bold" "${F[$i.id]}" "$rst" "${F[$i.desc]:-}"
      [ -n "${F[$i.mirrors]:-}" ]      && printf '  %sprovenance:%s %s\n' "$dim" "$rst" "${F[$i.mirrors]}"
      [ -n "${F[$i.tool]:-}" ]         && printf '  %stool:%s %s%s\n' "$dim" "$rst" "${F[$i.tool]}" "${F[$i.tool_version]:+ (pinned ${F[$i.tool_version]})}"
      [ -n "${F[$i.why]:-}" ]          && printf '%s\n' "${F[$i.why]}" | sed 's/^/  /'
      printf '  %srun:%s\n' "$dim" "$rst"; printf '%s\n' "${F[$i.run]}" | sed 's/^/    /'
    done
    exit 0 ;;
  fetch)
    say "pre-fetching pinned tools into $CACHE_DIR"
    for ((i = 0; i < N; i++)); do
      [ -n "${F[$i.tool]:-}" ] || continue
      if resolve_tool "$i"; then
        printf '%s   OK%s    %-20s %s\n' "$grn" "$rst" "${F[$i.tool]}" "$VF_TOOL"
      else
        printf '%s   MISS%s  %-20s %s\n' "$ylw" "$rst" "${F[$i.tool]}" "$RESOLVE_ERR"
      fi
    done
    exit 0 ;;
esac

# --- run ----------------------------------------------------------------------
printf '%s%s%s — local merge bar, stage: %s%s%s\n' \
  "$bold" "${META[name]:-$(basename "$ROOT")}" "$rst" "$cyn" "$STAGE" "$rst"

ran=0
for ((i = 0; i < N; i++)); do
  selected "$i" || continue
  ran=$((ran + 1))
  run_check "$i"
done

if [ "$ran" -eq 0 ]; then
  die "no checks selected (stage: $STAGE${ONLY:+, only: $ONLY})"
fi

# --- summary ------------------------------------------------------------------
say "SUMMARY"
for i in "${!R_ID[@]}"; do
  v="${R_VERDICT[$i]}"; c="$grn"
  case "$v" in FAIL) c="$red" ;; SKIP|skip|WARN) c="$ylw" ;; esac
  printf '  %-30s %s%-5s%s %s%s%s\n' "${R_ID[$i]}" "$c" "$v" "$rst" "$dim" "${R_NOTE[$i]}" "$rst"
done

if [ "$FAILED" -ne 0 ]; then
  printf '\n%s✗ GATE FAILED%s — do not merge.\n' "$red" "$rst"
  exit 1
fi
if [ "$BLOCKING_SKIP" -ne 0 ] && [ "${VF_GATE_ALLOW_SKIP:-0}" != "1" ]; then
  printf '\n%s✗ GATE INCOMPLETE%s — a check was SKIPPED, so this run is weaker than the bar you declared.\n' "$ylw" "$rst"
  printf '   Install the tool it named, or accept the gap explicitly with VF_GATE_ALLOW_SKIP=1.\n'
  exit 2
fi
printf '\n%s✓ GATE PASSED%s — every declared check for stage '\''%s'\'' ran and passed.\n' "$grn" "$rst" "$STAGE"
