"""Open one PR per stacked branch: the first on master, each later one on the
branch before it. Usage: python3 -I open_prs.py <wave> <titles.json> <bodies_dir> <first_base>
Prints branch<TAB>url and writes <bodies_dir>/prs.json."""
import json
import os
import subprocess
import sys

wave, titles_path, bodies, base = sys.argv[1:5]
# Optional: resume at PR number `start` (1-based) on top of `previous_url`.
start = int(sys.argv[5]) if len(sys.argv) > 5 else 1
titles = json.load(open(titles_path))
opened = []
previous_url = sys.argv[6] if len(sys.argv) > 6 else None
for k, (name, title) in enumerate(titles.items(), start=1):
    if k < start:
        continue
    branch = f"{wave}/{k:02d}-{name}"
    body = open(os.path.join(bodies, f"{name}.md")).read()
    note = (
        f"Part of the cross-platform remote desktop optimization ({wave}, {k} of {len(titles)}). "
        + (f"Stacked on {previous_url}: merge that first; this PR then retargets to `master`.\n\n" if previous_url else "Merge first in this stack.\n\n")
    )
    path = os.path.join(bodies, f"{name}.final.md")
    open(path, "w").write(note + body)
    url = subprocess.run(
        ["gh", "pr", "create", "--base", base, "--head", branch, "--title", title, "--body-file", path],
        capture_output=True, text=True, check=True,
    ).stdout.strip().splitlines()[-1]
    print(f"{branch}\t{url}", flush=True)
    opened.append({"name": name, "branch": branch, "base": base, "url": url, "title": title})
    previous_url = url
    base = branch
json.dump(opened, open(os.path.join(bodies, "prs.json"), "w"), indent=1)
