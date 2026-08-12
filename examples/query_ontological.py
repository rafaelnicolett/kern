#!/usr/bin/env python3
"""Drives `kern serve` over the real MCP JSON-RPC stdio protocol and asks
`query_ontological` a question — no mocking, this spawns the actual
compiled binary and speaks the wire protocol directly. Used to produce the
transcripts in the README's Examples section; reproduce them with:

    cargo build --release -p kern-cli
    kern project create demo --path examples/sample-specs
    python3 examples/query_ontological.py target/release/kern demo \\
        "what is the depends_on relation for TASK-002?"
"""
from __future__ import annotations

import json
import subprocess
import sys
import threading


def main() -> None:
    if len(sys.argv) != 4:
        print(f"usage: {sys.argv[0]} <kern-binary> <project> <question>", file=sys.stderr)
        sys.exit(1)
    kern_bin, project, question = sys.argv[1], sys.argv[2], sys.argv[3]

    proc = subprocess.Popen(
        [kern_bin, "serve", "--project", project],
        stdin=subprocess.PIPE,
        stdout=subprocess.PIPE,
        stderr=subprocess.PIPE,
        text=True,
        bufsize=1,
    )

    # kern's own tracing output goes to stderr and can run to hundreds of
    # lines during a real catch-up scan (frontmatter + prose extraction is
    # one real LLM call per candidate) — on a large enough corpus that
    # exceeds the OS pipe buffer (~64KB), and with nothing draining
    # `stderr=subprocess.PIPE`, the child blocks trying to write more of
    # it while this script blocks reading stdout that will never arrive.
    # A background thread that continuously drains stderr avoids that
    # deadlock; the last N lines are kept only for the failure path below.
    stderr_tail: list[str] = []

    def drain_stderr() -> None:
        for line in proc.stderr:
            stderr_tail.append(line)
            del stderr_tail[:-200]

    threading.Thread(target=drain_stderr, daemon=True).start()

    def send(message: dict) -> None:
        proc.stdin.write(json.dumps(message) + "\n")
        proc.stdin.flush()

    def recv() -> dict | None:
        line = proc.stdout.readline()
        return json.loads(line) if line.strip() else None

    send({
        "jsonrpc": "2.0",
        "id": 1,
        "method": "initialize",
        "params": {
            "protocolVersion": "2024-11-05",
            "capabilities": {},
            "clientInfo": {"name": "kern-examples", "version": "0.1.0"},
        },
    })
    recv()
    send({"jsonrpc": "2.0", "method": "notifications/initialized"})

    send({
        "jsonrpc": "2.0",
        "id": 2,
        "method": "tools/call",
        "params": {"name": "query_ontological", "arguments": {"question": question}},
    })
    response = recv()

    proc.terminate()
    try:
        proc.wait(timeout=5)
    except subprocess.TimeoutExpired:
        proc.kill()

    if response is None:
        print("no response — server exited early; last stderr lines:", file=sys.stderr)
        print("".join(stderr_tail), file=sys.stderr)
        sys.exit(1)

    result = json.loads(response["result"]["content"][0]["text"])
    print(json.dumps(result, indent=2))


if __name__ == "__main__":
    main()
