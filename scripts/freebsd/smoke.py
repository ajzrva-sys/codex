#!/usr/bin/env python3
"""Exercise native Codex shell and patch tools against a local mock Responses API."""

import argparse
import json
import os
import platform
import subprocess
import tempfile
import threading
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer
from pathlib import Path


def main() -> None:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("codex", type=Path)
    parser.add_argument(
        "--code-mode",
        action="store_true",
        help="run tools through the native V8 helper",
    )
    args = parser.parse_args()
    binary = args.codex.resolve(strict=True)
    requests = []

    class Handler(BaseHTTPRequestHandler):
        def log_message(self, *_args):
            pass

        def do_POST(self):
            request = json.loads(self.rfile.read(int(self.headers["Content-Length"])))
            requests.append(request)
            index = len(requests)
            if index == 1:
                item = {
                    "type": "function_call",
                    "call_id": "native-shell",
                    "name": "exec_command",
                    "arguments": json.dumps(
                        {
                            "cmd": "uname -s; printf 'native shell works\\n' > shell.txt",
                            "yield_time_ms": 1000,
                            "max_output_tokens": 1000,
                        }
                    ),
                }
            elif index == 2:
                item = {
                    "type": "custom_tool_call",
                    "call_id": "native-patch",
                    "name": "apply_patch",
                    "input": "*** Begin Patch\n*** Add File: patch.txt\n+native patch works\n*** End Patch",
                }
            else:
                item = {
                    "type": "message",
                    "role": "assistant",
                    "id": "message-done",
                    "content": [{"type": "output_text", "text": "FREEBSD_SMOKE_OK"}],
                }
            if args.code_mode and index <= 2:
                argument = item.get("arguments") or json.dumps(item["input"])
                item = {
                    "type": "custom_tool_call",
                    "call_id": item["call_id"],
                    "name": "exec",
                    "input": f"text(await tools.{item['name']}({argument}));",
                }
            response_id = f"response-{index}"
            events = [
                {"type": "response.created", "response": {"id": response_id}},
                {"type": "response.output_item.done", "item": item},
                {
                    "type": "response.completed",
                    "response": {
                        "id": response_id,
                        "usage": {
                            "input_tokens": 0,
                            "output_tokens": 0,
                            "total_tokens": 0,
                        },
                    },
                },
            ]
            body = "".join(
                f"data: {json.dumps(event)}\n\n" for event in events
            ).encode()
            self.send_response(200)
            self.send_header("Content-Type", "text/event-stream")
            self.send_header("Content-Length", str(len(body)))
            self.end_headers()
            self.wfile.write(body)

    server = ThreadingHTTPServer(("127.0.0.1", 0), Handler)
    threading.Thread(target=server.serve_forever, daemon=True).start()
    try:
        with tempfile.TemporaryDirectory(prefix="codex-native-smoke-") as temp:
            root = Path(temp)
            home = root / "home"
            home.mkdir()
            workspace = root / "workspace"
            workspace.mkdir()
            config = f"""
model = "gpt-5.4"
model_provider = "local_test"
check_for_update_on_startup = false
[model_providers.local_test]
name = "Local smoke test"
base_url = "http://127.0.0.1:{server.server_port}/v1"
wire_api = "responses"
requires_openai_auth = false
supports_websockets = false
"""
            if args.code_mode:
                config += "\n[features]\ncode_mode = true\ncode_mode_only = true\ncode_mode_host = true\n"
            (home / "config.toml").write_text(config)
            result = subprocess.run(
                [
                    str(binary),
                    "exec",
                    "--skip-git-repo-check",
                    "--sandbox",
                    "danger-full-access",
                    "--json",
                    "Run the native shell and patch smoke test.",
                ],
                cwd=workspace,
                env={**os.environ, "CODEX_HOME": str(home)},
                text=True,
                capture_output=True,
                timeout=90,
                check=False,
            )
            if result.returncode:
                raise RuntimeError(result.stdout + result.stderr)
            assert (workspace / "shell.txt").is_file(), result.stdout + result.stderr
            assert (workspace / "patch.txt").is_file(), result.stdout + result.stderr
            assert (workspace / "shell.txt").read_text() == "native shell works\n", (
                result.stdout
            )
            assert (workspace / "patch.txt").read_text() == "native patch works\n", (
                result.stdout
            )
            assert "FREEBSD_SMOKE_OK" in result.stdout, result.stdout
            assert len(requests) == 3, len(requests)
            outputs = [
                item
                for request in requests[1:]
                for item in request["input"]
                if item.get("type")
                in ("function_call_output", "custom_tool_call_output")
                and item.get("call_id") == "native-shell"
            ]
            assert any(platform.system() in json.dumps(item) for item in outputs), (
                outputs
            )
            print(
                f"PASS: {platform.system()} shell execution, file writes, apply_patch, and Responses round trip (code mode: {args.code_mode})"
            )
    finally:
        server.shutdown()
        server.server_close()


if __name__ == "__main__":
    main()
