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
        print("no response — server exited early; stderr:", file=sys.stderr)
        print(proc.stderr.read(), file=sys.stderr)
        sys.exit(1)

    result = json.loads(response["result"]["content"][0]["text"])
    print(json.dumps(result, indent=2))


if __name__ == "__main__":
    main()
