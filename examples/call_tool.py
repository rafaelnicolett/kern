#!/usr/bin/env python3
"""Drives `kern serve` over the real MCP JSON-RPC stdio protocol and calls
any one of kern's 6 tools with arbitrary arguments — the general-purpose
sibling of `query_ontological.py` (which only ever calls that one tool).
Used to produce every multi-tool transcript in examples/README.md; no
mocking, this spawns the actual compiled binary and speaks the wire
protocol directly.

    cargo build --release -p kern-cli
    kern project create demo --path examples/sample-specs \\
        --embedding-provider ollama --embedding-model all-minilm \\
        --extraction-provider ollama --extraction-model llama3.2
    python3 examples/call_tool.py target/release/kern demo \\
        query_by_concept '{"concept": "TASK-006"}'

Note: `get_related_entities`, `query_by_concept`, and `explain_relation`
take a real `entity_id` (a UUID), not a name — `query_by_concept` is how
you resolve a human-readable name/description to its real id in the first
place. See examples/README.md for the full two-step flow.
"""
from __future__ import annotations

import json
import subprocess
import sys
import threading


def main() -> None:
    if len(sys.argv) != 5:
        print(
            f"usage: {sys.argv[0]} <kern-binary> <project> <tool-name> <json-args>",
            file=sys.stderr,
        )
        print(
            "  tools: search_hybrid, query_by_concept, get_related_entities, "
            "get_ontology_schema, explain_relation, query_ontological",
            file=sys.stderr,
        )
        sys.exit(1)
    kern_bin, project, tool_name, raw_args = sys.argv[1], sys.argv[2], sys.argv[3], sys.argv[4]
    try:
        arguments = json.loads(raw_args)
    except json.JSONDecodeError as e:
        print(f"invalid JSON in <json-args>: {e}", file=sys.stderr)
        sys.exit(1)

    proc = subprocess.Popen(
        [kern_bin, "serve", "--project", project],
        stdin=subprocess.PIPE,
        stdout=subprocess.PIPE,
        stderr=subprocess.PIPE,
        text=True,
        bufsize=1,
    )

    # See query_ontological.py for why this matters: kern's tracing output
    # on stderr can exceed the OS pipe buffer during a real catch-up scan
    # on a non-trivial corpus, and an undrained `stderr=subprocess.PIPE`
    # deadlocks the child against this script's blocking stdout read.
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
        "params": {"name": tool_name, "arguments": arguments},
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

    if "error" in response:
        print(json.dumps(response["error"], indent=2))
        sys.exit(1)

    result = json.loads(response["result"]["content"][0]["text"])
    print(json.dumps(result, indent=2))


if __name__ == "__main__":
    main()
