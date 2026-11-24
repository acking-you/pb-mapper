"""Resolve conflict blocks by keeping ours then theirs; for the blocks named
by index in --close, put "}\\n\\n" between them (ours ends inside a function
whose closing brace follows the block, theirs is a whole new function).

Usage: python3 -I union_fn.py <file> [--close i,j,...]
"""
import re
import sys

path = sys.argv[1]
close = set()
if len(sys.argv) > 3 and sys.argv[2] == "--close":
    close = {int(i) for i in sys.argv[3].split(",")}
t = open(path, encoding="utf-8").read()
pattern = re.compile(r"<<<<<<< [^\n]*\n(.*?)=======\n(.*?)>>>>>>> [^\n]*\n", re.S)
index = 0


def resolve(m):
    global index
    i = index
    index += 1
    return m.group(1) + ("}\n\n" if i in close else "") + m.group(2)


t = pattern.sub(resolve, t)
assert "<<<<<<<" not in t and ">>>>>>>" not in t
open(path, "w", encoding="utf-8").write(t)
print(f"{path}: {index} blocks")
