"""Reads the runner's points of interest from an `xctrace export` of the
os-signpost table (trace-run.sh writes NAME-signposts.xml) and prints, per
mark, how many pictures carried it, and the time from each picture's
`Decoded` to its `Commit` end (layer) or its `Consumed` (texture).

The export lists some rows twice; a row is kept once per time, name, kind
and picture. Usage: python3 -I signposts.py NAME-signposts.xml
"""

import statistics
import sys
import xml.etree.ElementTree as ET


def main(path):
    root = ET.parse(path).getroot()
    by_id = {}
    for element in root.iter():
        if "id" in element.attrib:
            by_id[element.attrib["id"]] = element

    def resolve(element):
        return by_id.get(element.attrib["ref"], element) if "ref" in element.attrib else element

    seen = set()
    marks = {}
    for row in root.iter("row"):
        fields = [resolve(child) for child in row]
        time = next(int(f.text) for f in fields if f.tag == "event-time")
        kind = next(f.text for f in fields if f.tag == "event-type")
        name = next(f.text for f in fields if f.tag == "signpost-name")
        metadata = next((f for f in fields if f.tag == "os-log-metadata"), None)
        if metadata is None:
            # An interval's end carries no arguments: its identifier pairs it.
            ident = next(f.text for f in fields if f.tag == "os-signpost-identifier")
            picture = ("id", ident)
        else:
            values = [int(resolve(v).text) for v in metadata if resolve(v).tag == "uint64"]
            picture = tuple(values)
            ident = next(f.text for f in fields if f.tag == "os-signpost-identifier")
        key = (time, name, kind, picture)
        if key in seen:
            continue
        seen.add(key)
        marks.setdefault((name, kind), []).append((time, picture, ident))

    for (name, kind), rows in sorted(marks.items()):
        print(f"{name} {kind}: {len(rows)}")

    decoded = {picture: time for time, picture, _ in marks.get(("Decoded", "Event"), [])}
    spans = []
    if ("Commit", "End") in marks:
        begun = {ident: picture for _, picture, ident in marks.get(("Commit", "Begin"), [])}
        for time, _, ident in marks[("Commit", "End")]:
            picture = begun.get(ident)
            if picture in decoded:
                spans.append(time - decoded[picture])
        label = "decoded to commit end"
    else:
        for time, picture, _ in marks.get(("Consumed", "Event"), []):
            if picture in decoded:
                spans.append(time - decoded[picture])
        label = "decoded to consumed"
    if spans:
        spans.sort()
        p95 = spans[min(len(spans) - 1, int(len(spans) * 0.95))]
        print(
            f"{label}: {len(spans)} pictures, median {statistics.median(spans) / 1e6:.3f} ms, "
            f"p95 {p95 / 1e6:.3f} ms, max {spans[-1] / 1e6:.3f} ms"
        )


if __name__ == "__main__":
    main(sys.argv[1])
