#!/usr/bin/env python3
"""Example OmniCLI plugin exposing date and time tools."""

import json
import sys
from datetime import datetime, timezone


def send(value: dict) -> None:
    print(json.dumps(value, ensure_ascii=False), flush=True)


def make_response(request_id, result) -> dict:
    return {
        "jsonrpc": "2.0",
        "id": request_id,
        "result": result,
    }


def make_error(request_id, code: int, message: str) -> dict:
    return {
        "jsonrpc": "2.0",
        "id": request_id,
        "error": {"code": code, "message": message},
    }


def handle_initialize(params, request_id) -> dict:
    return make_response(request_id, {
        "name": "datetime",
        "version": "0.1.0",
        "description": "Provides current date and time tools",
    })


def handle_tools_list(params, request_id) -> dict:
    return make_response(request_id, [
        {
            "name": "now",
            "description": "Return the current UTC timestamp",
            "input_schema": {
                "type": "object",
                "additionalProperties": False,
                "properties": {
                    "format": {
                        "type": "string",
                        "enum": ["iso", "rfc2822", "unix"],
                    }
                },
            },
        },
        {
            "name": "date_info",
            "description": "Return information about a given ISO date",
            "input_schema": {
                "type": "object",
                "additionalProperties": False,
                "required": ["date"],
                "properties": {
                    "date": {
                        "type": "string",
                        "description": "ISO 8601 date string",
                    }
                },
            },
        },
    ])


def handle_tools_call(params, request_id) -> dict:
    tool = params.get("tool")
    arguments = params.get("arguments", {})

    if tool == "now":
        fmt = arguments.get("format", "iso")
        now = datetime.now(timezone.utc)
        if fmt == "unix":
            stdout = str(int(now.timestamp()))
        elif fmt == "rfc2822":
            stdout = now.strftime("%a, %d %b %Y %H:%M:%S %z")
        else:
            stdout = now.isoformat()
        return make_response(request_id, {
            "success": True,
            "stdout": stdout,
            "stderr": "",
            "truncated": False,
            "metadata": {"format": fmt},
        })

    if tool == "date_info":
        date_str = arguments.get("date", "")
        try:
            parsed = datetime.fromisoformat(date_str)
            return make_response(request_id, {
                "success": True,
                "stdout": f"year={parsed.year} month={parsed.month} day={parsed.day} weekday={parsed.weekday()}",
                "stderr": "",
                "truncated": False,
                "metadata": {"iso": parsed.isoformat()},
            })
        except ValueError as exc:
            return make_response(request_id, {
                "success": False,
                "stdout": "",
                "stderr": f"invalid date: {exc}",
                "truncated": False,
                "metadata": {},
            })

    return make_error(request_id, -32601, f"unknown tool: {tool}")


def main() -> None:
    if len(sys.argv) < 2 or sys.argv[1] != "serve":
        print("usage: datetime.py serve", file=sys.stderr)
        sys.exit(1)

    for line in sys.stdin:
        line = line.strip()
        if not line:
            continue
        try:
            request = json.loads(line)
        except json.JSONDecodeError as exc:
            send(make_error(None, -32700, f"parse error: {exc}"))
            continue

        request_id = request.get("id")
        method = request.get("method", "")
        params = request.get("params", {})

        if method == "initialize":
            send(handle_initialize(params, request_id))
        elif method == "tools/list":
            send(handle_tools_list(params, request_id))
        elif method == "tools/call":
            send(handle_tools_call(params, request_id))
        else:
            send(make_error(request_id, -32601, f"method not found: {method}"))


if __name__ == "__main__":
    main()
