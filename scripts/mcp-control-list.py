#!/usr/bin/env python3
"""从真实 MCP stdio 服务生成控制面清单。

只驱动 initialize / tools/list，不做任何有副作用的 tools/call。输出：
  .神经总账/控制面.json  供人/agent 直接读取的行动目录
  .神经总账/控制面.证据.json 带哈希的生成证据
"""

from __future__ import annotations

import hashlib
import json
import subprocess
import sys
from datetime import datetime, timezone
from pathlib import Path


ROOT = Path(__file__).resolve().parent.parent
BIN = ROOT / "zunzhe-core" / "target" / "release" / "zunzhe-core"
OUT_DIR = ROOT / ".神经总账"


def rpc_request(line: str) -> dict:
    return json.loads(line)


def main() -> int:
    if not BIN.is_file():
        print(f"找不到 MCP 二进制: {BIN}", file=sys.stderr)
        return 1

    requests = [
        {
            "jsonrpc": "2.0",
            "id": 1,
            "method": "initialize",
            "params": {
                "protocolVersion": "2024-11-05",
                "capabilities": {},
                "clientInfo": {"name": "zunzhe-control-list", "version": "1"},
            },
        },
        {"jsonrpc": "2.0", "id": 2, "method": "tools/list", "params": {}},
    ]
    payload = "\n".join(json.dumps(r, ensure_ascii=False) for r in requests) + "\n"
    proc = subprocess.run(
        [str(BIN), "mcp"],
        input=payload,
        text=True,
        capture_output=True,
        cwd=ROOT,
        timeout=30,
        check=False,
    )
    if proc.returncode != 0:
        print(proc.stderr, file=sys.stderr)
        return proc.returncode or 1

    tools = []
    server = None
    for line in proc.stdout.splitlines():
        try:
            message = rpc_request(line)
        except json.JSONDecodeError:
            continue
        if message.get("id") == 1:
            server = message.get("result", {}).get("serverInfo")
        if message.get("id") == 2:
            tools = message.get("result", {}).get("tools", [])
    if not tools:
        print("MCP 没有返回 tools/list", file=sys.stderr)
        return 1

    catalog = {
        "schema": "zunzhe.control-list.v1",
        "generated_at": datetime.now(timezone.utc).isoformat(),
        "server": server,
        "transport": "stdio",
        "command": [str(BIN), "mcp"],
        "tool_count": len(tools),
        "tools": [
            {
                "name": tool.get("name", ""),
                "description": tool.get("description", ""),
                "input_schema": tool.get("inputSchema", {}),
                "risk": "read" if tool.get("name", "").endswith(("_list", "_status", "_get")) or tool.get("name") in {"doctor", "config_schema_tool", "notes_list", "email_status"} else "write",
            }
            for tool in tools
        ],
    }
    encoded = json.dumps(catalog, ensure_ascii=False, sort_keys=True, indent=2).encode()
    digest = hashlib.sha256(encoded).hexdigest()
    OUT_DIR.mkdir(parents=True, exist_ok=True)
    catalog_path = OUT_DIR / "控制面.json"
    evidence_path = OUT_DIR / "控制面.证据.json"
    catalog_path.write_bytes(encoded + b"\n")
    evidence_path.write_text(
        json.dumps(
            {
                "schema": "zunzhe.control-list.evidence.v1",
                "generated_at": catalog["generated_at"],
                "catalog_sha256": digest,
                "source": "real MCP initialize + tools/list over stdio",
                "tool_count": len(tools),
            },
            ensure_ascii=False,
            indent=2,
        )
        + "\n",
        encoding="utf-8",
    )
    print(f"控制面已生成: {catalog_path} ({len(tools)} tools, sha256={digest})")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
