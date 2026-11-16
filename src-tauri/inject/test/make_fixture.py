#!/usr/bin/env python3
# SPDX-License-Identifier: AGPL-3.0-only
"""Builds inject/test/fixture.html: runs inject/postwarden.js against synthetic
Outlook-like DOM fixtures (no personal data) with a mocked Tauri IPC.

Usage (from src-tauri/):  python3 inject/test/make_fixture.py
Then open inject/test/fixture.html in Safari; the title shows "ALL PASS" or "FAILURES".
"""
import json
import pathlib
import subprocess

ROOT = pathlib.Path(__file__).resolve().parents[2]
out = subprocess.run(
    ["cargo", "test", "--quiet", "dump_default_config", "--", "--ignored", "--nocapture"],
    cwd=ROOT, capture_output=True, text=True, check=True,
).stdout
cfg = json.loads(next(l for l in out.splitlines() if l.startswith("PWCONFIG"))[len("PWCONFIG"):])
cfg["app_hosts"] = []  # the fixture runs from file://
script = (ROOT / "inject/postwarden.js").read_text()
# the fixture runs from file://: drop the scheme and host gates (exact lines, checked)
for gate in ("if (location.protocol !== 'https:') return;\n",):
    assert gate in script, gate
    script = script.replace(gate, "")
lines = script.split("\n")
host_gate = [l for l in lines if "test(location.hostname)) return;" in l]
assert len(host_gate) == 1, host_gate
script = "\n".join(l for l in lines if l not in host_gate)
template = (ROOT / "inject/test/fixture.template.html").read_text()
html = template.replace("__CFG__", json.dumps(cfg)).replace("__SCRIPT__", script)
(ROOT / "inject/test/fixture.html").write_text(html)
print("wrote", ROOT / "inject/test/fixture.html")
