#!/usr/bin/env python3
"""Finalize an iteration: aggregate benchmark.json per skill (tokens/time from timing.json,
one run per configuration), write benchmark.md, and print a per-eval comparison with the
previous iteration.  usage: SKILL_CREATOR_DIR=... finalize.py ITER [PREV_ITER] [SCRATCH_DIR]"""
import json, os, sys
from pathlib import Path
SC = Path(os.environ["SKILL_CREATOR_DIR"])
sys.path.insert(0, str(SC))
from scripts import aggregate_benchmark as ab  # noqa: E402

S = Path(sys.argv[3]) if len(sys.argv) > 3 else Path.cwd()
ROOT = Path(__file__).resolve().parents[2]
SKILLS = {"vibeforge-gate": str(ROOT / "Skills/vibeforge-gate"),
          "repo-publication-readiness": str(ROOT / "Skills/repo-publication-readiness"),
          "coroner": str(ROOT / "Skills/coroner")}
it = sys.argv[1]; prev = sys.argv[2] if len(sys.argv) > 2 else None


def finalize(skill):
    bdir = S / f"{skill}-workspace" / f"iteration-{it}"
    results = ab.load_run_results(bdir)
    for cfg, runs in results.items():
        for r in runs:
            ev = next(p for p in bdir.glob(f"eval-{r['eval_id']}-*") if p.is_dir())
            t = json.loads((ev / cfg / f"run-{r['run_number']}" / "timing.json").read_text())
            r["time_seconds"] = t["total_duration_seconds"]; r["tokens"] = t["total_tokens"]
    bench = ab.generate_benchmark(bdir, skill, SKILLS[skill])
    # generate_benchmark re-loads; overwrite its run figures with the timing-derived ones
    byk = {(r["eval_id"], cfg, r["run_number"]): r for cfg, rs in results.items() for r in rs}
    for run in bench["runs"]:
        r = byk[(run["eval_id"], run["configuration"], run["run_number"])]
        run["result"]["time_seconds"] = r["time_seconds"]; run["result"]["tokens"] = r["tokens"]
    bench["run_summary"] = ab.aggregate_results(results)
    bench["metadata"]["runs_per_configuration"] = 1
    bench["metadata"]["executor_model"] = "claude (session default)"
    bench["metadata"]["analyzer_model"] = "claude (session default)"
    prev_file = bdir / "benchmark.json"
    if prev_file.exists():  # keep analyst notes written after an earlier aggregation
        bench["notes"] = json.loads(prev_file.read_text()).get("notes", [])
    (bdir / "benchmark.json").write_text(json.dumps(bench, indent=2) + "\n")
    (bdir / "benchmark.md").write_text(ab.generate_markdown(bench))
    return bench


def table(bench):
    out = {}
    for run in bench["runs"]:
        out.setdefault(run["eval_id"], {})[run["configuration"]] = run["result"]
    return out


tot = {}
for skill in SKILLS:
    b = finalize(skill); cur = table(b)
    pv = table(json.loads((S / f"{skill}-workspace" / f"iteration-{prev}" / "benchmark.json").read_text())) if prev else {}
    print(f"\n## {skill}  (iteration {it}" + (f" vs {prev})" if prev else ")"))
    print("| eval | with | base | " + (f"i{prev} with | i{prev} base |" if prev else ""))
    sw = sb = tw = tb = 0
    for e in sorted(cur):
        w, bsl = cur[e]["with_skill"], cur[e]["without_skill"]
        sw += w["passed"]; sb += bsl["passed"]; tw += w["total"]; tb += bsl["total"]
        row = f"| {e} | {w['passed']}/{w['total']} | {bsl['passed']}/{bsl['total']} |"
        if prev and e in pv:
            row += f" {pv[e]['with_skill']['passed']}/{pv[e]['with_skill']['total']} | {pv[e]['without_skill']['passed']}/{pv[e]['without_skill']['total']} |"
        print(row)
    rs = b["run_summary"]
    print(f"| total | {sw}/{tw} | {sb}/{tb} |")
    print(f"pass-rate mean: with {rs['with_skill']['pass_rate']['mean']:.2f}  base {rs['without_skill']['pass_rate']['mean']:.2f}  delta {rs['delta']['pass_rate']}")
    print(f"tokens mean: with {rs['with_skill']['tokens']['mean']:.0f}  base {rs['without_skill']['tokens']['mean']:.0f}; time mean: with {rs['with_skill']['time_seconds']['mean']:.0f}s  base {rs['without_skill']['time_seconds']['mean']:.0f}s")
    tot[skill] = (sw, tw, sb, tb)
    print("failed expectations (with_skill):")
    for run in b["runs"]:
        if run["configuration"] == "with_skill":
            for x in run["expectations"]:
                if not x["passed"]:
                    print(f"  e{run['eval_id']}: {x['text'][:140]}")
print("\n", tot)
