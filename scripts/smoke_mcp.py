#!/usr/bin/env python3
"""Read-only MCP stdio smoke test for mcp-moonraker.

Usage:
    python3 scripts/smoke_mcp.py [path-to-binary] [--camera]

The server must be able to reach Moonraker. Nothing destructive is called.
"""

from __future__ import annotations

import argparse
import json
import queue
import subprocess
import sys
import threading
import time

PROTOCOL_VERSION = "2025-11-25"


def result_text(message: dict) -> str:
    """Concatenate the text blocks of a tools/call result."""
    parts = []
    for block in message.get("result", {}).get("content", []) or []:
        if block.get("type") == "text":
            parts.append(block.get("text", ""))
    return "\n".join(parts)


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument(
        "binary",
        nargs="?",
        default="target/debug/mcp-moonraker",
        help="path to the mcp-moonraker binary",
    )
    parser.add_argument(
        "--camera",
        action="store_true",
        help="also fetch a camera snapshot (read-only, slower)",
    )
    args = parser.parse_args()

    requests = [
        {
            "jsonrpc": "2.0",
            "id": 1,
            "method": "initialize",
            "params": {
                "protocolVersion": PROTOCOL_VERSION,
                "capabilities": {},
                "clientInfo": {"name": "mcp-moonraker-smoke", "version": "0.1.0"},
            },
        },
        {"jsonrpc": "2.0", "method": "notifications/initialized"},
        {"jsonrpc": "2.0", "id": 2, "method": "tools/list"},
        {"jsonrpc": "2.0", "id": 3, "method": "resources/list"},
        {"jsonrpc": "2.0", "id": 4, "method": "prompts/list"},
        {
            "jsonrpc": "2.0",
            "id": 5,
            "method": "tools/call",
            "params": {"name": "moonraker_printer_status", "arguments": {}},
        },
        {
            "jsonrpc": "2.0",
            "id": 6,
            "method": "tools/call",
            "params": {"name": "moonraker_temperatures", "arguments": {}},
        },
        {
            "jsonrpc": "2.0",
            "id": 7,
            "method": "resources/read",
            "params": {"uri": "moonraker://endstops"},
        },
        {
            "jsonrpc": "2.0",
            "id": 8,
            "method": "tools/call",
            "params": {"name": "moonraker_policy_info", "arguments": {}},
        },
        {
            "jsonrpc": "2.0",
            "id": 9,
            "method": "tools/call",
            "params": {"name": "moonraker_verify_connection", "arguments": {}},
        },
    ]

    expected = 9
    if args.camera:
        requests.extend(
            [
                {
                    "jsonrpc": "2.0",
                    "id": 10,
                    "method": "tools/call",
                    "params": {"name": "moonraker_cameras", "arguments": {}},
                },
                {
                    "jsonrpc": "2.0",
                    "id": 11,
                    "method": "tools/call",
                    "params": {"name": "moonraker_camera_snapshot", "arguments": {}},
                },
            ]
        )
        expected = 11

    try:
        proc = subprocess.Popen(
            [args.binary],
            stdin=subprocess.PIPE,
            stdout=subprocess.PIPE,
            stderr=subprocess.PIPE,
            text=True,
            bufsize=1,
        )
    except FileNotFoundError:
        print(f"FAIL binary not found: {args.binary}")
        return 1

    assert proc.stdin and proc.stdout and proc.stderr

    lines: queue.Queue[str] = queue.Queue()

    def read_stdout() -> None:
        assert proc.stdout is not None
        for line in proc.stdout:
            lines.put(line)

    threading.Thread(target=read_stdout, daemon=True).start()

    for request in requests:
        proc.stdin.write(json.dumps(request) + "\n")
        proc.stdin.flush()

    seen: dict[int, dict] = {}
    deadline = time.time() + 45
    while len(seen) < expected:
        remaining = deadline - time.time()
        if remaining <= 0:
            break
        try:
            line = lines.get(timeout=remaining)
        except queue.Empty:
            break
        try:
            message = json.loads(line)
        except json.JSONDecodeError:
            print("NON-JSON STDOUT:", line[:200])
            continue
        if "id" in message:
            seen[message["id"]] = message

    proc.stdin.close()
    try:
        proc.wait(timeout=10)
    except subprocess.TimeoutExpired:
        proc.kill()
        proc.wait(timeout=5)
    stderr = proc.stderr.read()

    failures: list[str] = []

    def check(condition: bool, label: str) -> None:
        print(("PASS " if condition else "FAIL ") + label)
        if not condition:
            failures.append(label)

    init = seen.get(1, {})
    check(
        init.get("result", {}).get("protocolVersion") == PROTOCOL_VERSION,
        "initialize",
    )
    check(
        init.get("result", {}).get("serverInfo", {}).get("name") == "mcp-moonraker",
        "server identity",
    )

    tools = seen.get(2, {}).get("result", {}).get("tools", [])
    tool_names = {tool.get("name") for tool in tools}
    check("moonraker_server_info" in tool_names, "tools/list contains moonraker_server_info")
    check("moonraker_verify_connection" in tool_names, "tools/list contains verify tool")
    check("moonraker_camera_snapshot" in tool_names, "tools/list contains camera tool")
    check(len(tools) >= 40, f"tools/list count ({len(tools)})")

    resources = seen.get(3, {}).get("result", {}).get("resources", [])
    check(
        any(resource.get("uri") == "moonraker://status" for resource in resources),
        "resources/list",
    )

    prompts = seen.get(4, {}).get("result", {}).get("prompts", [])
    check(len(prompts) == 2, "prompts/list")

    status_text = result_text(seen.get(5, {}))
    check(
        '"print_stats"' in status_text or '"state"' in status_text,
        "printer_status result",
    )
    check('"heaters"' in result_text(seen.get(6, {})), "temperatures result")
    check('"text"' in json.dumps(seen.get(7, {})), "resource read")
    check(
        '"connected"' in result_text(seen.get(9, {})),
        "verify_connection result",
    )
    check('"policy"' in result_text(seen.get(8, {})), "policy_info result")

    if args.camera:
        cameras_text = result_text(seen.get(9, {}))
        check('"cameras"' in cameras_text, "cameras result")
        snapshot = seen.get(10, {}).get("result", {})
        images = [
            block
            for block in snapshot.get("content", []) or []
            if block.get("type") == "image"
        ]
        check(
            bool(images)
            and images[0].get("mimeType", "").startswith("image/")
            and len(images[0].get("data", "")) > 100,
            "camera snapshot image",
        )

    if failures:
        print("\nSTDERR:\n" + stderr[-4000:])
        print(f"\n{len(failures)} check(s) failed.")
        return 1

    print(f"\nMCP smoke test passed ({len(seen)} responses).")
    return 0


if __name__ == "__main__":
    sys.exit(main())
