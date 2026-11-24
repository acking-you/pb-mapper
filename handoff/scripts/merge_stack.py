"""Squash-merge stacked PRs in order and check master after each.

Usage: python3 -I merge_stack.py <prs.json> <squash.json> <first_pr_number> <last_pr_number>
Each merge's subject is "<title> (#N)" and its body the prose from squash.json.
After each merge, origin/master's tree must equal the PR branch's tree.
"""
import json
import subprocess
import sys
import time

prs = json.load(open(sys.argv[1]))
squash = json.load(open(sys.argv[2]))
first, last = int(sys.argv[3]), int(sys.argv[4])


def run(*args, check=True):
    return subprocess.run(list(args), capture_output=True, text=True, check=check)


for pr in prs:
    number = int(pr["url"].rsplit("/", 1)[1])
    if not first <= number <= last:
        continue
    branch = pr["branch"]
    tree = run("git", "rev-parse", f"{branch}^{{tree}}").stdout.strip()
    # Put the PR's verified tree straight on the current master, so the
    # squash takes exactly that tree; the tree already passed CI.
    run("git", "fetch", "-q", "origin", "master")
    if run("git", "rev-parse", f"{branch}^").stdout.strip() != run("git", "rev-parse", "origin/master").stdout.strip():
        message = run("git", "log", "-1", "--format=%B", branch).stdout
        commit = subprocess.run(
            ["git", "commit-tree", tree, "-p", "origin/master"], input=message, capture_output=True, text=True, check=True
        ).stdout.strip()
        run("git", "branch", "-f", branch, commit)
        run("git", "push", "-q", "-f", "git@github.com:acking-you/pb-mapper.git", f"{branch}:refs/heads/{branch}")
        print(f"#{number}: {branch} now {commit[:10]} on master", flush=True)
    # Wait for GitHub to retarget the PR to master and compute mergeability.
    for _ in range(60):
        view = json.loads(run("gh", "pr", "view", str(number), "--json", "baseRefName,mergeable,state").stdout)
        if view["state"] != "OPEN":
            sys.exit(f"#{number} is {view['state']}")
        if view["baseRefName"] == "master" and view["mergeable"] == "MERGEABLE":
            break
        time.sleep(5)
    else:
        sys.exit(f"#{number} not mergeable on master: {view}")
    body = squash[pr["name"]] + "\n\nCo-Authored-By: Claude Opus 5.5 <noreply@anthropic.com>"
    run("gh", "pr", "merge", str(number), "--squash", "--subject", f"{pr['title']} (#{number})", "--body", body)
    run("git", "fetch", "-q", "origin", "master")
    master_tree = run("git", "rev-parse", "origin/master^{tree}").stdout.strip()
    sha = run("git", "rev-parse", "--short", "origin/master").stdout.strip()
    same = master_tree == tree
    print(f"#{number} merged as {sha}; master tree {'matches' if same else 'DIFFERS from'} {branch}", flush=True)
    if not same:
        sys.exit("stopping: master differs from the verified tree")
