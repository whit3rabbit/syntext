"""Minimal MCP stdio client for benchmarking fff (https://github.com/dmtrKovalenko/fff).

fff ships no one-shot CLI; its only runnable binary is `fff-mcp`, an MCP
server speaking JSON-RPC 2.0 over stdio. MCP's stdio transport frames
messages as newline-delimited JSON objects (one per line) — it does NOT use
LSP-style Content-Length headers.

The server takes a positional base path, scans it eagerly in a background
thread, and exposes `grep` / `find_files` / `multi_grep` tools. There is no
scan-completion notification observable over MCP, so `wait_until_ready` polls
a cheap grep until two consecutive calls return equal, non-zero counts. That
is a stabilization heuristic: a lower bound on full readiness, not a proof.

Used by bench_compare.py; pure stdlib, no third-party dependencies.
"""

from __future__ import annotations

import json
import subprocess
import time
from typing import Any


class FffDriverError(RuntimeError):
    pass


class FffMcpClient:
    PROTOCOL_VERSION = "2024-11-05"

    def __init__(self, binary: str, base_path: str) -> None:
        self.binary = binary
        self.base_path = base_path
        self.proc: subprocess.Popen[str] | None = None
        self._next_id = 0
        self.spawn_time: float | None = None
        self.grep_tool: str | None = None
        self.tool_schemas: dict[str, Any] = {}

    # -- transport ----------------------------------------------------------

    def start(self) -> None:
        self.spawn_time = time.perf_counter()
        self.proc = subprocess.Popen(
            [self.binary, self.base_path],
            stdin=subprocess.PIPE,
            stdout=subprocess.PIPE,
            stderr=subprocess.DEVNULL,
            text=True,
            bufsize=1,
        )

    def _send(self, msg: dict[str, Any]) -> None:
        assert self.proc is not None and self.proc.stdin is not None
        self.proc.stdin.write(json.dumps(msg) + "\n")
        self.proc.stdin.flush()

    def _request(self, method: str, params: dict[str, Any] | None = None,
                 timeout_s: float = 60.0) -> Any:
        assert self.proc is not None and self.proc.stdout is not None
        self._next_id += 1
        req_id = self._next_id
        msg: dict[str, Any] = {"jsonrpc": "2.0", "id": req_id, "method": method}
        if params is not None:
            msg["params"] = params
        self._send(msg)

        deadline = time.monotonic() + timeout_s
        while time.monotonic() < deadline:
            line = self.proc.stdout.readline()
            if not line:
                raise FffDriverError(
                    f"fff-mcp exited (code {self.proc.poll()}) during {method}"
                )
            line = line.strip()
            if not line:
                continue
            try:
                reply = json.loads(line)
            except json.JSONDecodeError:
                continue  # non-JSON noise on stdout
            if reply.get("id") == req_id:
                if "error" in reply:
                    raise FffDriverError(f"{method} failed: {reply['error']}")
                return reply.get("result")
            if "method" in reply and "id" in reply:
                # Server-initiated request (e.g. roots/list): refuse politely
                # rather than deadlocking on an unanswered request.
                self._send({
                    "jsonrpc": "2.0",
                    "id": reply["id"],
                    "error": {"code": -32601, "message": "not supported by bench client"},
                })
            # Notifications and stale replies are ignored.
        raise FffDriverError(f"timeout waiting for {method} response")

    # -- MCP lifecycle ------------------------------------------------------

    def initialize(self) -> None:
        self._request("initialize", {
            "protocolVersion": self.PROTOCOL_VERSION,
            "capabilities": {},
            "clientInfo": {"name": "syntext-bench", "version": "1.0"},
        })
        self._send({"jsonrpc": "2.0", "method": "notifications/initialized"})
        tools = self._request("tools/list").get("tools", [])
        self.tool_schemas = {t["name"]: t.get("inputSchema", {}) for t in tools}
        for candidate in ("grep", "multi_grep"):
            if candidate in self.tool_schemas:
                self.grep_tool = candidate
                break
        if self.grep_tool is None:
            raise FffDriverError(
                "no grep tool exposed by fff-mcp; available tools and schemas: "
                + json.dumps(self.tool_schemas, indent=2)
            )

    # -- search -------------------------------------------------------------

    def _grep_arguments(self, pattern: str) -> dict[str, Any]:
        """Build tool arguments from the live schema; the schema is the contract."""
        schema = self.tool_schemas.get(self.grep_tool or "", {})
        props = schema.get("properties", {})
        for key in ("pattern", "query", "search", "text"):
            if key in props:
                return {key: pattern}
        raise FffDriverError(
            f"cannot map a pattern onto {self.grep_tool} schema: "
            + json.dumps(schema, indent=2)
        )

    def grep(self, pattern: str, timeout_s: float = 120.0) -> tuple[int, float]:
        """Run one grep tool call. Returns (result_count, elapsed_ms)."""
        args = self._grep_arguments(pattern)
        start = time.perf_counter()
        result = self._request(
            "tools/call",
            {"name": self.grep_tool, "arguments": args},
            timeout_s=timeout_s,
        )
        elapsed_ms = (time.perf_counter() - start) * 1000.0
        return self._count_results(result), elapsed_ms

    @staticmethod
    def _count_results(result: Any) -> int:
        """Count result items from a tools/call response.

        fff returns matches as MCP content; prefer structured content when
        present, otherwise count non-empty text lines. These are ranked
        (possibly capped) results, NOT grep-compatible line counts.
        """
        if not isinstance(result, dict):
            return 0
        structured = result.get("structuredContent")
        if isinstance(structured, dict):
            for value in structured.values():
                if isinstance(value, list):
                    return len(value)
        count = 0
        for item in result.get("content", []):
            if item.get("type") == "text":
                count += sum(1 for line in item.get("text", "").splitlines()
                             if line.strip())
        return count

    # -- readiness ----------------------------------------------------------

    def wait_until_ready(self, probe_pattern: str, timeout_s: float = 180.0) -> float:
        """Poll until two consecutive probes return equal, non-zero counts.

        Returns startup-to-ready in ms, measured from process spawn. This is
        fff's analog of an index build: the background scan must complete
        before query results stabilize.
        """
        assert self.spawn_time is not None
        deadline = time.monotonic() + timeout_s
        prev_count = -1
        while time.monotonic() < deadline:
            try:
                count, _ = self.grep(probe_pattern, timeout_s=30.0)
            except FffDriverError:
                raise
            if count > 0 and count == prev_count:
                return (time.perf_counter() - self.spawn_time) * 1000.0
            prev_count = count
            time.sleep(0.25)
        raise FffDriverError(
            f"fff scan did not stabilize within {timeout_s}s "
            f"(probe {probe_pattern!r}, last count {prev_count})"
        )

    # -- teardown -----------------------------------------------------------

    def close(self) -> None:
        if self.proc is None:
            return
        self.proc.terminate()
        try:
            self.proc.wait(timeout=5)
        except subprocess.TimeoutExpired:
            self.proc.kill()
            self.proc.wait()
        self.proc = None
