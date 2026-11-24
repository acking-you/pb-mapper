"""Draft PR bodies for a wave from its plan briefs and workflow results.

Usage: python3 -I pr_bodies.py <briefs.json> <result.json> <titles.json> <out_dir>
Writes <out_dir>/<name>.md and prints name<TAB>title per PR.
"""
import json
import os
import re
import subprocess
import sys

briefs = {p["name"]: p for p in json.load(open(sys.argv[1]))}
results = {r["name"]: r for r in json.load(open(sys.argv[2])) if not r.get("failed") and not r.get("skipped")}
titles = json.load(open(sys.argv[3]))
out = sys.argv[4]
os.makedirs(out, exist_ok=True)


def why(design):
    first = design.strip().split("\n\n")[0]
    first = re.sub(r"^Why\.\s*", "", first)
    return first.strip()


def commits(branch, base):
    log = subprocess.run(["git", "log", "--reverse", "--format=- %s", f"{base}..{branch}"], capture_output=True, text=True, check=True)
    return log.stdout.strip()


def short(text, limit=220):
    text = " ".join(text.split())
    return text if len(text) <= limit else text[: limit - 1].rstrip() + "…"


for name, title in titles.items():
    b, r = briefs[name], results[name]
    base = r.get("base", "origin/master")
    if base.startswith("perf2/") is False:
        base = "origin/master"
    verification = r["impl"]["verification"] + r["fix"]["verification"]
    seen, rows = set(), []
    for v in verification:
        key = v["command"].strip()
        if key in seen:
            continue
        seen.add(key)
        rows.append(f"- `{short(key, 160)}`: {short(v['result'])}")
    measured = r["impl"]["measurements"].strip()
    body = f"""## Summary

{why(b['design'])}

Plan items: {', '.join(b['items'])}. Platforms: {', '.join(b['platforms'])}.

## Changes

{commits(r['branch'], base)}

Protocol: {short(b['protocol_changes'], 600)}

## Verification on this Mac

{chr(10).join(rows[:14])}

Two independent reviews (correctness, effect and completeness) ran on the branch; every confirmed finding was fixed{'' if not r['fix']['rejected'] else ' and ' + str(len(r['fix']['rejected'])) + ' finding(s) were rejected with a reason'}.

## Measurements

{measured if measured and measured.lower() != 'none' else 'None made here; see below.'}

## Needs CI or a device

{r['impl']['device_only'].strip()}

🤖 Generated with [Claude Code](https://claude.com/claude-code)
"""
    open(os.path.join(out, f"{name}.md"), "w").write(body)
    print(f"{name}\t{title}\t{len(body)}")
