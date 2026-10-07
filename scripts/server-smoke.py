"""Real-process/socket/restart test; optionally compare against NumericParity.java's TSV.

Build first: cargo build --locked
Run: python3 scripts/server-smoke.py [numeric-reference.tsv]
Only disposable temp data is used; child servers always stop on completion.
"""
import json
from pathlib import Path
import subprocess
import sys
import tempfile
import urllib.request

ROOT = Path(__file__).resolve().parents[1]
BINARY = ROOT / "target" / "debug" / ("strata.exe" if sys.platform == "win32" else "strata")


def start(directory):
    process = subprocess.Popen(
        [str(BINARY), "--data", directory, "--listen", "127.0.0.1:0"],
        stdout=subprocess.PIPE, text=True,
    )
    line = process.stdout.readline()
    if "listening on " not in line:
        process.terminate()
        process.wait(timeout=10)
        raise RuntimeError(f"server failed to start: {line}")
    return process, "http://" + line.split("listening on ")[1].split()[0]


def request(base, method, path, body=None):
    data = None if body is None else json.dumps(body).encode()
    req = urllib.request.Request(base + path, data=data, method=method,
                                 headers={"Content-Type": "application/json"})
    with urllib.request.urlopen(req, timeout=10) as response:
        return json.load(response)


def sort(field, order="asc", missing="last"):
    return [{"field": field, "order": order, "missing": missing}]


CASES = [
    ("long-sort", {"match_all": {}}, sort("n"), ["f", "b", "e", "d", "a", "c"]),
    ("long-range", {"range": {"n": {"gt": 9007199254740992}}}, sort("n"), ["d", "a"]),
    ("double-range", {"range": {"price": {"gte": 0.0, "lte": 1.25}}}, sort("price"), ["b", "d", "f"]),
    ("negative-zero", {"range": {"price": {"gte": -0.0, "lte": -0.0}}}, sort("price"), ["a"]),
    ("filter-keyword", {"bool": {"must": [{"match_all": {}}], "filter": [{"range": {"n": {"gte": -5, "lte": 9007199254740993}}}]}}, sort("tag"), ["d", "b", "e"]),
    ("descending-missing", {"match_all": {}}, sort("price", "desc", "first"), ["c", "d", "f", "b", "a", "e"]),
]


def check(base, reference):
    for name, query, order, expected in CASES:
        if reference is not None:
            expected = reference[name]
        body = {"query": query, "sort": order, "size": 2}
        ids = []
        while True:
            result = request(base, "POST", "/numbers/_search", body)
            assert result["hits"]["total"] == {"value": len(expected), "relation": "eq"}, result
            ids.extend(hit["_id"] for hit in result["hits"]["hits"])
            if result["search_after"] is None:
                break
            body["search_after"] = result["search_after"]
        assert ids == expected, (name, ids, expected)


def main():
    reference = None
    if len(sys.argv) > 1:
        reference = {}
        for line in Path(sys.argv[1]).read_text().splitlines():
            name, total, ids = line.split("\t")
            reference[name] = ids.split(",") if ids else []
            assert len(reference[name]) == int(total)
    with tempfile.TemporaryDirectory(prefix="lucene-server-test-") as directory:
        process, base = start(directory)
        try:
            assert request(base, "GET", "/")["name"] == "strata"
            request(base, "PUT", "/numbers", {"mappings": {"properties": {
                "n": {"type": "long"}, "price": {"type": "double"}, "tag": {"type": "keyword"},
            }}})
            rows = [(2**63 - 1, -0.0, "z"), (-5, 0.0, "b"), None,
                    (9007199254740993, 1.25, "a"), (-5, -2.5, "c"), (-(2**63), 1.25, "b")]
            for name, row in zip("abcdef", rows):
                source = dict(zip(["n", "price", "tag"], row)) if row else {}
                request(base, "PUT", f"/numbers/_doc/{name}", source)
            assert request(base, "POST", "/numbers/_search", {})["hits"]["total"]["value"] == 0
            request(base, "POST", "/numbers/_refresh")
            check(base, reference)
        finally:
            process.terminate()
            process.wait(timeout=10)
        process, base = start(directory)
        try:
            check(base, reference)
            request(base, "DELETE", "/numbers/_doc/a?refresh=true")
            assert request(base, "POST", "/numbers/_search", {})["hits"]["total"]["value"] == 5
        finally:
            process.terminate()
            process.wait(timeout=10)
    print("PASS: six paginated numeric/filter/sort cases, refresh, process restart, delete"
          + ("; Java Lucene reference matched" if reference else ""))


if __name__ == "__main__":
    main()
