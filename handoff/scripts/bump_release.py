"""Bump pb-mapper from 0.12.1 to 0.13.0 (UI build 931) in the current checkout.

Run with `python3 -I bump_release.py <repo root> <YYYY-MM-DD>`; afterwards run
`cargo update --workspace` so Cargo.lock follows.
"""
import re
import sys

root, date = sys.argv[1], sys.argv[2]
OLD, NEW, BUILD = "0.12.1", "0.13.0", 931


def edit(path, fn):
    p = f"{root}/{path}"
    s = open(p, encoding="utf-8").read()
    t = fn(s)
    assert t != s, f"no change in {path}"
    open(p, "w", encoding="utf-8").write(t)


def cargo(s):
    # The workspace version and the pb-mapper-* pins only.
    s = s.replace(f'version = "{OLD}"\nauthors', f'version = "{NEW}"\nauthors', 1)
    s, n = re.subn(
        rf'^(pb-mapper[a-z-]* = \{{ [^\n]*version = "){re.escape(OLD)}(")',
        rf"\g<1>{NEW}\g<2>",
        s,
        flags=re.M,
    )
    assert n == 7, n
    return s


edit("Cargo.toml", cargo)
edit("js/package.json", lambda s: s.replace(f'"{OLD}"', f'"{NEW}"'))
edit("js/index.js", lambda s: s.replace(OLD, NEW))
edit(
    "js/bun.lock",
    lambda s: re.sub(
        rf'("pb-mapper-[a-z0-9-]+": "){re.escape(OLD)}(")', rf"\g<1>{NEW}\g<2>", s
    ),
)
edit("ui/pubspec.yaml", lambda s: s.replace(f"version: {OLD}+930", f"version: {NEW}+{BUILD}"))
edit(
    "docs/ui-updates.md",
    lambda s: re.sub(
        r"Release 0\.12\.0 uses 929 and 0\.12\.1 uses 930\.(\s+)The next(\s+)distribution must use at least 931\.",
        r"Release 0.12.0 uses 929, 0.12.1 uses 930 and 0.13.0 uses\g<1>931. The next\g<2>distribution must use at least 932.",
        s,
    ),
)
edit("CHANGELOG.md", lambda s: s.replace("## [Unreleased]", f"## [{NEW}] - {date}", 1))
print("bumped to", NEW, "build", BUILD)
