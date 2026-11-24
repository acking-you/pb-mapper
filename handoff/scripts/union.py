"""Resolve git conflict blocks in the given files by keeping ours, then theirs."""
import re
import sys

for path in sys.argv[1:]:
    text = open(path, encoding="utf-8").read()
    pattern = re.compile(r"<<<<<<< [^\n]*\n(.*?)(?:\|\|\|\|\|\|\| [^\n]*\n.*?)?=======\n(.*?)>>>>>>> [^\n]*\n", re.S)
    new, n = pattern.subn(lambda m: m.group(1) + m.group(2), text)
    assert "<<<<<<<" not in new and ">>>>>>>" not in new, path
    open(path, "w", encoding="utf-8").write(new)
    print(f"{path}: {n} blocks")
