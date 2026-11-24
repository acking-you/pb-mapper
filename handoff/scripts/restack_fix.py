"""Patch a file in some stacked branches' trees, then rebuild the stack above
the first patched one (same trees and messages otherwise) and force-push it.

Usage: python3 -I restack_fix.py <path> <old> <new> <patched,branches> <branch>...
The branches are given in stack order; the first one's parent stays.
"""
import os
import subprocess
import sys
import tempfile

path, old, new, patched_list = sys.argv[1:5]
patched = set(patched_list.split(","))
branches = sys.argv[5:]


def git(*args, stdin=None, env=None):
    return subprocess.run(
        ["git", *args], input=stdin, capture_output=True, text=True, check=True, env=env
    ).stdout.strip()


def patched_tree(tree):
    content = git("show", f"{tree}:{path}") + "\n"
    assert content.count(old) == 1, (tree, content.count(old))
    blob = git("hash-object", "-w", "--stdin", stdin=content.replace(old, new))
    with tempfile.TemporaryDirectory() as tmp:
        env = dict(os.environ, GIT_INDEX_FILE=os.path.join(tmp, "index"))
        git("read-tree", tree, env=env)
        git("update-index", "--cacheinfo", f"100644,{blob},{path}", env=env)
        return git("write-tree", env=env)


parent = git("rev-parse", f"{branches[0]}^")
for branch in branches:
    tree = git("rev-parse", f"{branch}^{{tree}}")
    if branch in patched:
        tree = patched_tree(tree)
    message = git("log", "-1", "--format=%B", branch) + "\n"
    commit = git("commit-tree", tree, "-p", parent, stdin=message)
    git("branch", "-f", branch, commit)
    print(f"{branch} -> {commit[:10]}{' (patched)' if branch in patched else ''}")
    parent = commit
git("push", "-q", "-f", "git@github.com:acking-you/pb-mapper.git", *[f"{b}:refs/heads/{b}" for b in branches])
print("pushed")
