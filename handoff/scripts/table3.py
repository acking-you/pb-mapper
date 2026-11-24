"""Resolve conflict blocks made only of markdown table rows, row by row, with
the merge base: a row theirs changed from the base takes theirs, else ours;
rows only one side has are kept. Blocks with other lines are left alone and
listed. Usage: python3 -I table3.py <file>   (during a merge: stages 1-3)"""
import re
import subprocess
import sys

path = sys.argv[1]


def stage(n):
    return subprocess.run(["git", "show", f":{n}:{path}"], capture_output=True, text=True, check=True).stdout


def key(line):
    return line.split("|")[1].strip()


base_rows = {}
for line in stage(1).splitlines():
    if line.startswith("| "):
        base_rows.setdefault(key(line), line)

t = open(path, encoding="utf-8").read()
pattern = re.compile(r"<<<<<<< [^\n]*\n(.*?)=======\n(.*?)>>>>>>> [^\n]*\n", re.S)
left = []


def resolve(m):
    ours, theirs = m.group(1).splitlines(), m.group(2).splitlines()
    if not all(line.startswith("| ") for line in ours + theirs):
        left.append(m.group(0)[:200])
        return m.group(0)
    o = {key(line): line for line in ours}
    th = {key(line): line for line in theirs}
    out = []
    for line in ours:
        k = key(line)
        if k in th and th[k] != base_rows.get(k):
            out.append(th[k])
        elif k in th or k not in base_rows or True:
            out.append(line)
    # Rows theirs added (not in base, not in ours), after the row before them.
    for i, line in enumerate(theirs):
        k = key(line)
        if k in o or k in base_rows:
            continue
        before = key(theirs[i - 1]) if i else None
        at = next((j + 1 for j, row in enumerate(out) if before and key(row) == before), len(out))
        out.insert(at, line)
    # Rows ours dropped that theirs changed would be lost: report them.
    for k in th:
        if k not in o and k in base_rows and th[k] != base_rows[k]:
            left.append(f"theirs changed a row ours removed: {k}")
    return "\n".join(out) + "\n"


t = pattern.sub(resolve, t)
open(path, "w", encoding="utf-8").write(t)
print(f"{path}: unresolved {len(left)}")
for item in left:
    print("  ", item.replace("\n", " ")[:200])
