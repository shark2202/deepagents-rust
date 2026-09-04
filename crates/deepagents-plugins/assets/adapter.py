#!/usr/bin/env python3
"""JSON-RPC 2.0 over stdio adapter bridge.

This is a v0 stub Python adapter embedded into the `deepagents-plugins`
crate via `rust-embed`. The Rust host (`PluginTransport`) spawns this script
through `python3` and communicates over stdin/stdout using newline-delimited
JSON-RPC 2.0 messages, consistent with LSP/MCP transports.

The script reads one JSON-RPC request object per line from stdin, dispatches
to a registered method, and writes a single JSON-RPC response object per line
to stdout. Each response line is flushed immediately so the host can read it
without buffering delay.

Methods:
    - `initialize`: returns adapter capabilities.
    - `shutdown`: returns `null` and is a no-op (the host closes stdin/kill).
    - `invoke`: dispatches to a tool defined by the plugin manifest.

Note (migration): Python native extensions built via PyO3 are not portable
across the pure-Rust build; this adapter is therefore a thin, dependency-free
bridge rather than a compiled extension.
"""

from __future__ import annotations

import json
import sys

JSONRPC_VERSION = "2.0"


def _make_response(req_id, result=None, error=None):
    resp = {"jsonrpc": JSONRPC_VERSION, "id": req_id}
    if error is not None:
        resp["error"] = error
    else:
        resp["result"] = result
    return resp


def _initialize(params):
    capabilities = {
        "protocolVersion": JSONRPC_VERSION,
        "adapter": "deepagents-plugins-python-bridge",
        "version": "0.1.0",
        "capabilities": {"tools": {"listChanged": False}},
    }
    return capabilities


def _invoke(params):
    # v0: echo back the invoked tool name and arguments.
    if not isinstance(params, dict):
        return {"ok": False, "error": "invalid params"}
    tool = params.get("tool") or params.get("name")
    args = params.get("args") or params.get("arguments") or {}
    return {"ok": True, "tool": tool, "args": args, "output": None}


METHODS = {
    "initialize": _initialize,
    "invoke": _invoke,
    "shutdown": lambda _p: None,
}


def handle_line(line: str) -> str | None:
    line = line.strip()
    if not line:
        return None
    try:
        req = json.loads(line)
    except json.JSONDecodeError as exc:
        # Parse error: -32700
        return json.dumps(
            _make_response(
                None,
                error={
                    "code": -32700,
                    "message": f"parse error: {exc}",
                },
            )
        )
    req_id = req.get("id")
    method = req.get("method")
    params = req.get("params")
    fn = METHODS.get(method)
    if fn is None:
        return json.dumps(
            _make_response(
                req_id,
                error={
                    "code": -32601,
                    "message": f"method not found: {method}",
                },
            )
        )
    try:
        result = fn(params)
        return json.dumps(_make_response(req_id, result=result))
    except Exception as exc:  # noqa: BLE001
        return json.dumps(
            _make_response(
                req_id,
                error={
                    "code": -32603,
                    "message": f"internal error: {exc}",
                },
            )
        )


def main() -> None:
    for raw in sys.stdin:
        out = handle_line(raw)
        if out is None:
            continue
        sys.stdout.write(out + "\n")
        sys.stdout.flush()


if __name__ == "__main__":
    main()
