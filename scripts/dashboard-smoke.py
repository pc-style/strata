"""Exercise the embedded dashboard with agent-browser and disposable server data.

Run after cargo build. Optional first argument: screenshot output directory.
"""
import json
import os
from pathlib import Path
import socket
import subprocess
import sys
import tempfile
import time
import urllib.request

root = Path(__file__).resolve().parents[1]
artifacts = Path(sys.argv[1]).resolve() if len(sys.argv) > 1 else None
env = {**os.environ, "AGENT_BROWSER_SESSION": "strata-dashboard-smoke"}


def browser(*args):
    return subprocess.check_output(["agent-browser", *args], env=env, text=True)


def check(expression):
    try:
        browser("wait", "--fn", expression)
    except subprocess.CalledProcessError:
        print(browser("snapshot"))
        raise


def screenshot(name):
    if artifacts:
        artifacts.mkdir(parents=True, exist_ok=True)
        browser("screenshot", str(artifacts / f"{name}.png"), "--full")


with tempfile.TemporaryDirectory(prefix="strata-dashboard-") as data:
    with socket.socket() as sock:
        sock.bind(("127.0.0.1", 0))
        port = sock.getsockname()[1]
    base = f"http://127.0.0.1:{port}"
    server = subprocess.Popen([str(root / "target/debug/strata"), "--data", data, "--listen", f"127.0.0.1:{port}"], stdout=subprocess.DEVNULL)

    def api(path, body, method="POST"):
        req = urllib.request.Request(base + path, data=json.dumps(body).encode(), method=method, headers={"Content-Type": "application/json"})
        with urllib.request.urlopen(req) as response:
            return json.load(response)

    try:
        for _ in range(100):
            try:
                urllib.request.urlopen(base + "/_health").close()
                break
            except OSError:
                time.sleep(0.05)
        browser("open", base + "/_dashboard")
        check("document.body.textContent.includes('No indexes yet')")
        screenshot("empty")
        api("/books", {"mappings": {"properties": {"title": {"type": "text"}, "price": {"type": "double"}, "tag": {"type": "keyword"}}}}, "PUT")
        titles = ["Rust in Action", "Programming Rust", "Rust for Rustaceans"] + [f"Search handbook {i}" for i in range(9)]
        for i, title in enumerate(titles):
            api(f"/books/_doc/{i:02}?refresh=true", {"title": title, "price": 15 + i * 3, "tag": "programming"}, "PUT")
        browser("find", "role", "button", "click", "--name", "Reload indexes")
        check("document.body.textContent.includes('12 searchable documents')")
        browser("find", "label", "Query", "fill", "rust")
        browser("press", "Enter")
        check("document.querySelectorAll('article').length === 3")
        screenshot("search")
        browser("find", "label", "Dev mode", "check")
        check("document.querySelectorAll('article').length === 3 && document.body.textContent.includes('HTTP 200')")
        screenshot("dev")
        browser("set", "viewport", "390", "844")
        check("document.documentElement.scrollWidth <= innerWidth")
        screenshot("mobile")
        browser("set", "viewport", "1280", "900")
        browser("focus", "input[type=search]")
        browser("eval", "document.querySelector('input[type=search]').select()")
        browser("press", "Backspace")
        check("document.querySelector('input[type=search]').value === ''")
        browser("press", "Enter")
        check("document.querySelectorAll('article').length === 10")
        browser("find", "role", "button", "click", "--name", "Next page")
        check("document.querySelectorAll('article').length === 2 && document.querySelector('article').textContent.includes('10')")
        browser("find", "label", "JSON query", "check")
        browser("find", "label", "Search body", "fill", '{"query":{"range":{"price":{"gte":21,"lt":28}}},"sort":[{"field":"price","order":"desc"}],"size":2}')
        browser("find", "role", "button", "click", "--name", "Search", "--exact")
        check("document.querySelectorAll('article').length === 2 && document.querySelector('article h2').textContent === 'Search handbook 1'")
        browser("find", "label", "Search body", "fill", '{"query":{"invalid":{}}}')
        browser("find", "role", "button", "click", "--name", "Search", "--exact")
        check("document.querySelector('[role=alert]') && document.body.textContent.includes('HTTP 400')")
        screenshot("server-error")
        browser("find", "label", "Search body", "fill", '{')
        browser("find", "role", "button", "click", "--name", "Search", "--exact")
        check("document.querySelector('[role=alert]') && !document.body.textContent.includes('HTTP 400')")
        browser("find", "label", "JSON query", "click")
        browser("find", "label", "Query", "fill", "nonexistentword", "--exact")
        browser("press", "Enter")
        check("document.body.textContent.includes('No documents match')")
        screenshot("no-results")
        browser("find", "label", "Dev mode", "click")
        browser("find", "label", "Query", "fill", "rust")
        browser("press", "Tab")
        check("document.activeElement.textContent === 'Search' && getComputedStyle(document.activeElement).outlineStyle !== 'none'")
        browser("press", "Enter")
        check("document.querySelectorAll('article').length === 3")
        api("/numbers", {"mappings": {"properties": {"count": {"type": "long"}}}}, "PUT")
        api("/numbers/_doc/a?refresh=true", {"count": 9007199254740992}, "PUT")
        api("/numbers/_doc/b?refresh=true", {"count": 9007199254740993}, "PUT")
        browser("find", "role", "button", "click", "--name", "Reload indexes")
        check("document.querySelector('option[value=numbers]') !== null")
        browser("select", ".toolbar select", "numbers")
        browser("find", "label", "Dev mode", "check")
        browser("find", "label", "JSON query", "check")
        browser("find", "label", "Search body", "fill", '{"query":{"range":{"count":{"gte":9007199254740993}}}}')
        browser("find", "role", "button", "click", "--name", "Search", "--exact")
        check("document.querySelectorAll('article').length === 1 && document.querySelector('article h2').textContent === 'b' && document.querySelector('article').textContent.includes('9007199254740993')")
        errors = browser("errors")
        assert not errors.strip(), errors
        print("PASS: empty catalog, search, dev stats, mobile overflow, pagination, JSON range/sort, server/JSON errors, no results, keyboard search, exact 64-bit integers")
    finally:
        browser("close")
        server.terminate()
        server.wait(timeout=10)
