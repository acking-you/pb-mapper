"""Resolve the single conflict block in a file as ours + glue + theirs.
Usage: python3 -I glue.py <file> <glue, with \\n escapes>"""
import re
import sys

path, glue = sys.argv[1], sys.argv[2].encode().decode("unicode_escape")
t = open(path, encoding="utf-8").read()
pattern = re.compile(r"<<<<<<< [^\n]*\n(.*?)=======\n(.*?)>>>>>>> [^\n]*\n", re.S)
t, n = pattern.subn(lambda m: m.group(1) + glue + m.group(2), t)
assert n == 1 and "<<<<<<<" not in t, n
open(path, "w", encoding="utf-8").write(t)
print(path, "ok")
