"""Build one commit per PR whose tree is the integration branch's tree after
that PR's merge, each on the previous one, and a branch for each.

Usage: python3 -I stack_wave.py <wave> <merges.txt> <titles.json> <briefs.json> <parent>
<merges.txt> holds the integration merge commits, oldest first, in the order
of <titles.json>. Prints branch<TAB>commit<TAB>title.
"""
import json
import re
import subprocess
import sys

wave, merges_path, titles_path, briefs_path, parent = sys.argv[1:6]
merges = open(merges_path).read().split()
titles = json.load(open(titles_path))
briefs = {p["name"]: p for p in json.load(open(briefs_path))}
names = list(titles)
assert len(merges) == len(names), (len(merges), len(names))


def git(*args, stdin=None):
    return subprocess.run(["git", *args], input=stdin, capture_output=True, text=True, check=True).stdout.strip()


for k, (merge, name) in enumerate(zip(merges, names), start=1):
    # The merge must be the one for this branch.
    subject = git("log", "-1", "--format=%s", merge)
    assert f"perf2/{name}'" in subject, (subject, name)
    why = re.sub(r"^Why\.\s*", "", briefs[name]["design"].strip().split("\n\n")[0]).strip()
    message = f"{titles[name]}\n\n{why}\n\nCo-Authored-By: Claude Opus 5.5 <noreply@anthropic.com>\n"
    tree = git("rev-parse", f"{merge}^{{tree}}")
    commit = git("commit-tree", tree, "-p", parent, stdin=message)
    branch = f"{wave}/{k:02d}-{name}"
    git("branch", "-f", branch, commit)
    print(f"{branch}\t{commit[:10]}\t{titles[name]}")
    parent = commit
