"""Squash each branch's current tree into one commit, stacked in order on a
parent, keeping the message of the branch's first-parent commit at <ref>^N
(the original stacked commit), then force-push them over SSH.

Usage: python3 -I restack.py <parent> <skip> <branch>...
<skip>: how many commits sit on top of the original stacked commit.
"""
import subprocess
import sys

parent, skip, branches = sys.argv[1], int(sys.argv[2]), sys.argv[3:]


def git(*args, stdin=None):
    return subprocess.run(["git", *args], input=stdin, capture_output=True, text=True, check=True).stdout.strip()


parent = git("rev-parse", parent)
for branch in branches:
    tree = git("rev-parse", f"{branch}^{{tree}}")
    message = git("log", "-1", "--format=%B", f"{branch}~{skip}") + "\n"
    commit = git("commit-tree", tree, "-p", parent, stdin=message)
    git("branch", "-f", branch, commit)
    print(f"{branch} -> {commit[:10]} ({git('log', '-1', '--format=%s', commit)})")
    parent = commit
git("push", "-q", "-f", "git@github.com:acking-you/pb-mapper.git", *[f"{b}:refs/heads/{b}" for b in branches])
print("pushed")
