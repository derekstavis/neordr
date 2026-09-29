#!/usr/bin/env python3
"""Opt-in diagnostic for Moshi's cached Codex exit-screen detection.

Run after building Neordr. Uses installed moshi-hook and disposable synthetic
chats only. Reports whether clearing an exit marker restores discovery; a stale
result reproduces the bug observed in moshi-hook 0.4.9, not a Neordr failure.
"""
import json
import os
from pathlib import Path
import socket
import subprocess
import tempfile
import time
import uuid

from moshi_smoke import BIN, HOOK, eventually, http, lua, rpc


def session_ids(port):
    status, tree = http(port, "/v1/workspaces?mux=herdr:smoke")
    assert status == 200, (status, tree)
    result = []

    def walk(value):
        if isinstance(value, dict):
            if value.get("agent") == "codex":
                result.append(value.get("sessionId"))
            for child in value.values():
                walk(child)
        elif isinstance(value, list):
            for child in value:
                walk(child)

    walk(tree)
    return result


def main():
    processes = []
    with tempfile.TemporaryDirectory(prefix="neordr-codex-end-", dir="/tmp") as directory:
        root = Path(directory).resolve()
        (root / "bin").mkdir()
        (root / "bin/herdr").symlink_to(BIN)
        env = {**os.environ, "HOME": str(root), "XDG_CONFIG_HOME": str(root / "config"),
               "MOSHI_STATE_DIR": str(root / "state"), "MOSHI_CONFIG_DIR": str(root / "state"),
               "MOSHI_SOCKET_PATH": str(root / "moshi.sock"), "CODEX_HOME": str(root / "codex"),
               "CLAUDE_CONFIG_DIR": str(root / ".claude"), "MOSHI_API_BASE": "http://127.0.0.1:9",
               "MOSHI_HERDR_PATH": str(root / "bin/herdr"), "TMPDIR": str(root),
               "NVIM_LOG_FILE": str(root / "nvim.log"),
               "PATH": str(root / "bin") + os.pathsep + os.environ["PATH"]}
        for key in ["TMUX", "TMUX_PANE", "ZELLIJ", "HERDR_ENV", "HERDR_SESSION",
                    "HERDR_PANE_ID", "HERDR_CONFIG_PATH", "NVIM"]:
            env.pop(key, None)
        for setting in ["always-on-discovery", "usage-collection"]:
            subprocess.run([HOOK, "set", setting, "off"], env=env, check=True,
                           capture_output=True, timeout=10)
        address = root / "nvim.sock"
        endpoint = root / "config/herdr/sessions/smoke/herdr.sock"
        with socket.socket() as reserve:
            reserve.bind(("127.0.0.1", 0))
            port = reserve.getsockname()[1]

        def start(args, name):
            log = open(root / (name + ".log"), "w")
            process = subprocess.Popen(args, env=env, stdin=subprocess.DEVNULL,
                                       stdout=log, stderr=log)
            processes.append((process, log))

        try:
            start(["nvim", "--headless", "-u", "NONE", "-i", "NONE", "--listen", str(address)], "nvim")
            eventually(address.exists)
            start([str(BIN), "--session", "smoke", "serve", "--nvim", str(address)], "neordr")
            eventually(lambda: endpoint.exists() and rpc(endpoint, "ping"))
            start([str(BIN), "router"], "router")
            eventually(lambda: (root / "config/herdr/herdr.sock").exists())
            start([HOOK, "serve", "--gateway-listen", f"127.0.0.1:{port}"], "moshi")
            eventually(lambda: http(port, "/v1/version")[0] == 200)

            session = str(uuid.uuid4())
            path = root / "codex/sessions/2026/09/29" / f"rollout-2026-09-29T00-00-00-{session}.jsonl"
            path.parent.mkdir(parents=True)
            path.write_text(json.dumps({"type": "session_meta", "payload": {
                "id": session, "cwd": str(root), "source": "cli", "originator": "codex_cli_rs"}}) + "\n")
            event = {"hook_event_name": "SessionStart", "session_id": session,
                     "cwd": str(root), "transcript_path": str(path)}
            child = root / "bin/codex"
            child.write_text(
                "import os,subprocess,tty\n"
                f"rollout=open({str(path)!r})\n"
                f"subprocess.run([{HOOK!r},'codex-hook'],input={json.dumps(event)!r},text=True,timeout=10)\n"
                "tty.setraw(0)\n"
                "os.write(1,b'\\x1b[2J\\x1b[HREADY\\r\\n')\n"
                "while True:\n"
                " data=os.read(0,4096)\n"
                " if not data: break\n"
                f" text='codex resume {session}' if b'MARKER' in data else 'CLEARED'\n"
                " os.write(1,('\\x1b[2J\\x1b[H'+text+'\\r\\n').encode())\n")
            pane = lua(env, address, "vim.cmd.enew(); local b=vim.api.nvim_get_current_buf(); "
                       "local e=require('neordr.backend').prepare(b); "
                       "vim.fn.jobstart({'python3'," + json.dumps(str(child)) + "},{term=true,env=e}); "
                       "return e.HERDR_PANE_ID")

            def visible_contains(text):
                return text in rpc(endpoint, "pane.read", {"pane_id": pane, "source": "visible"})["read"]["text"]

            eventually(lambda: visible_contains("READY"))
            before = eventually(lambda: (ids := session_ids(port)) and all(x == session for x in ids) and ids)
            rpc(endpoint, "pane.send_text", {"pane_id": pane, "text": "MARKER"})
            eventually(lambda: visible_contains("codex resume"))
            time.sleep(2.3)  # Negative screen-detection cache lasts two seconds.
            marker = session_ids(port)
            rpc(endpoint, "pane.send_text", {"pane_id": pane, "text": "CLEAR"})
            eventually(lambda: visible_contains("CLEARED"))
            time.sleep(2.3)
            after = session_ids(port)
            context_env = {**env, "HERDR_ENV": "1", "HERDR_SESSION": "smoke", "HERDR_PANE_ID": pane}
            fresh = json.loads(subprocess.check_output([HOOK, "context"], env=context_env, timeout=15))
            fresh_id = fresh.get("agent", {}).get("session")
            assert fresh_id == session, fresh
            print(json.dumps({"before": before, "exit_marker_visible": marker,
                              "after_marker_cleared": after, "fresh_context": fresh_id,
                              "stale_exit_cache_reproduced": bool(after) and all(x is None for x in after)}, indent=2))
        finally:
            for process, _ in reversed(processes):
                process.terminate()
            for process, log in reversed(processes):
                try:
                    process.wait(timeout=5)
                except subprocess.TimeoutExpired:
                    process.kill()
                    process.wait()
                log.close()


if __name__ == "__main__":
    main()
