#!/usr/bin/env python3
from __future__ import annotations

import argparse
import json
import os
import shlex
import subprocess
import sys
import textwrap
import tomllib
import urllib.error
import urllib.parse
import urllib.request
from dataclasses import dataclass
from pathlib import Path
from typing import Any


DEFAULT_CONFIG_PATH = "config.toml"
DEFAULT_REMOTE_CONFIG_PATH = "~/jossie/config.toml"
DEFAULT_STATE_PATH = ".jossie-chat-state.json"
REMOTE_JSON_BEGIN = "__JOSSIE_CONFIG_JSON_BEGIN__"
REMOTE_JSON_END = "__JOSSIE_CONFIG_JSON_END__"


@dataclass
class ConnectionInfo:
    base_url: str
    token: str
    source: str


def build_parser() -> argparse.ArgumentParser:
    parser = argparse.ArgumentParser(
        description="Chat with a running Jossie instance from the terminal.",
        formatter_class=argparse.RawDescriptionHelpFormatter,
        epilog=textwrap.dedent(
            """\
            Examples:
              python3 scripts/jossie_chat.py
              python3 scripts/jossie_chat.py ask "What are you working on?"
              python3 scripts/jossie_chat.py --remote-config-host prometheus
              python3 scripts/jossie_chat.py --remote-config-host prometheus ask "Hello Jossie"
              python3 scripts/jossie_chat.py history --limit 20
            """
        ),
    )
    parser.add_argument(
        "--base-url",
        help="Explicit Jossie base URL, for example http://prometheus:3000.",
    )
    parser.add_argument(
        "--token",
        help="Explicit auth token. If omitted, the script tries env vars or config files.",
    )
    parser.add_argument(
        "--config",
        default=DEFAULT_CONFIG_PATH,
        help=f"Local config.toml path to read when --remote-config-host is not used. Default: {DEFAULT_CONFIG_PATH}",
    )
    parser.add_argument(
        "--remote-config-host",
        "--ssh-host",
        dest="remote_config_host",
        help="SSH host to read Jossie's config.toml from.",
    )
    parser.add_argument(
        "--remote-config-path",
        default=DEFAULT_REMOTE_CONFIG_PATH,
        help=f"Remote config.toml path used with --remote-config-host. Default: {DEFAULT_REMOTE_CONFIG_PATH}",
    )
    parser.add_argument(
        "--state-file",
        default=DEFAULT_STATE_PATH,
        help=f"Path for storing the last conversation id per base URL. Default: {DEFAULT_STATE_PATH}",
    )
    parser.add_argument(
        "--conversation-id",
        help="Use this conversation id instead of the stored one.",
    )
    parser.add_argument(
        "--new",
        action="store_true",
        help="Start a new conversation instead of reusing the stored conversation id.",
    )
    parser.add_argument(
        "--json",
        action="store_true",
        help="Emit JSON instead of human-readable output where applicable.",
    )

    subparsers = parser.add_subparsers(dest="command")

    ask_parser = subparsers.add_parser("ask", help="Send one message and print Jossie's reply.")
    ask_parser.add_argument("message", nargs="+", help="Message to send.")

    subparsers.add_parser("repl", help="Interactive chat session. This is the default.")

    list_parser = subparsers.add_parser("list", help="List recent conversations.")
    list_parser.add_argument("--limit", type=int, default=10, help="Maximum number of conversations to print.")

    history_parser = subparsers.add_parser("history", help="Print message history for a conversation.")
    history_parser.add_argument("--limit", type=int, default=20, help="Maximum number of messages to print.")

    subparsers.add_parser("cancel", help="Cancel the current or explicitly selected conversation run.")
    subparsers.add_parser("new", help="Create a fresh local chat context by clearing the stored conversation id.")

    return parser


def normalize_base_url(base_url: str) -> str:
    return base_url.rstrip("/")


def load_local_server_config(path: str) -> dict[str, Any]:
    config_path = Path(path)
    if not config_path.exists():
        return {}
    with config_path.open("rb") as handle:
        data = tomllib.load(handle)
    return data.get("server", {})


def extract_remote_json(stdout: str) -> dict[str, Any]:
    start = stdout.find(REMOTE_JSON_BEGIN)
    end = stdout.find(REMOTE_JSON_END)
    if start == -1 or end == -1 or end <= start:
        raise RuntimeError("failed to parse remote config output")
    payload = stdout[start + len(REMOTE_JSON_BEGIN) : end].strip()
    return json.loads(payload)


def load_remote_server_config(host: str, path: str) -> dict[str, Any]:
    remote_script = (
        "import json, sys, tomllib; "
        "from pathlib import Path; "
        "path = Path(sys.argv[1]).expanduser(); "
        "data = tomllib.loads(path.read_text()); "
        f'print("{REMOTE_JSON_BEGIN}"); '
        'print(json.dumps(data.get("server", {}))); '
        f'print("{REMOTE_JSON_END}")'
    )
    remote_command = f"python3 -c {shlex.quote(remote_script)} {shlex.quote(path)}"
    result = subprocess.run(
        ["ssh", host, remote_command],
        check=False,
        capture_output=True,
        text=True,
    )
    if result.returncode != 0:
        raise RuntimeError(result.stderr.strip() or result.stdout.strip() or "ssh failed")
    return extract_remote_json(result.stdout)


def base_url_from_server_config(server_cfg: dict[str, Any], fallback_host: str | None) -> str | None:
    public_base_url = server_cfg.get("public_base_url")
    if public_base_url:
        return normalize_base_url(str(public_base_url))

    port = int(server_cfg.get("port", 3000))
    host = fallback_host or str(server_cfg.get("host", "127.0.0.1"))
    if host in {"0.0.0.0", "::", ""}:
        host = fallback_host or "127.0.0.1"
    return f"http://{host}:{port}"


def resolve_connection(args: argparse.Namespace) -> ConnectionInfo:
    server_cfg: dict[str, Any] = {}
    source = "explicit arguments"

    if args.remote_config_host:
        server_cfg = load_remote_server_config(args.remote_config_host, args.remote_config_path)
        source = f"remote config {args.remote_config_host}:{args.remote_config_path}"
    elif args.config:
        server_cfg = load_local_server_config(args.config)
        if server_cfg:
            source = f"local config {args.config}"

    token = (
        args.token
        or os.environ.get("JOSSIE_AUTH_TOKEN")
        or os.environ.get("JOSSIE_SERVER_AUTH_TOKEN")
        or server_cfg.get("auth_token")
    )
    base_url = (
        args.base_url
        or os.environ.get("JOSSIE_BASE_URL")
        or base_url_from_server_config(server_cfg, args.remote_config_host)
    )

    if not base_url:
        raise RuntimeError("no Jossie base URL found; pass --base-url or a config source")
    if not token:
        raise RuntimeError("no Jossie auth token found; pass --token or a config source")

    return ConnectionInfo(
        base_url=normalize_base_url(str(base_url)),
        token=str(token),
        source=source,
    )


def load_state(path: str) -> dict[str, Any]:
    state_path = Path(path)
    if not state_path.exists():
        return {"profiles": {}}
    with state_path.open("r", encoding="utf-8") as handle:
        return json.load(handle)


def save_state(path: str, state: dict[str, Any]) -> None:
    state_path = Path(path)
    state_path.parent.mkdir(parents=True, exist_ok=True)
    with state_path.open("w", encoding="utf-8") as handle:
        json.dump(state, handle, indent=2, sort_keys=True)
        handle.write("\n")


def get_profile(state: dict[str, Any], base_url: str) -> dict[str, Any]:
    profiles = state.setdefault("profiles", {})
    return profiles.setdefault(base_url, {})


def resolve_conversation_id(
    args: argparse.Namespace,
    state: dict[str, Any],
    connection: ConnectionInfo,
) -> str | None:
    if args.new:
        return None
    if args.conversation_id:
        return args.conversation_id
    profile = get_profile(state, connection.base_url)
    return profile.get("conversation_id")


def set_conversation_id(state: dict[str, Any], connection: ConnectionInfo, conversation_id: str | None) -> None:
    profile = get_profile(state, connection.base_url)
    if conversation_id:
        profile["conversation_id"] = conversation_id
    else:
        profile.pop("conversation_id", None)


def request_json(
    connection: ConnectionInfo,
    method: str,
    path: str,
    payload: dict[str, Any] | None = None,
) -> Any:
    url = f"{connection.base_url}{path}"
    data = None
    headers = {"Authorization": f"Bearer {connection.token}"}
    if payload is not None:
        data = json.dumps(payload).encode("utf-8")
        headers["Content-Type"] = "application/json"
    request = urllib.request.Request(url, data=data, headers=headers, method=method)
    try:
        with urllib.request.urlopen(request, timeout=600) as response:
            body = response.read().decode("utf-8")
    except urllib.error.HTTPError as exc:
        body = exc.read().decode("utf-8", errors="replace")
        raise RuntimeError(f"{exc.code} {exc.reason}: {body}") from exc
    except urllib.error.URLError as exc:
        raise RuntimeError(f"request to {url} failed: {exc.reason}") from exc

    if not body:
        return None
    return json.loads(body)


def chat_once(connection: ConnectionInfo, message: str, conversation_id: str | None) -> dict[str, Any]:
    payload: dict[str, Any] = {"message": message}
    if conversation_id:
        payload["conversation_id"] = conversation_id
    response = request_json(connection, "POST", "/api/chat", payload)
    if not isinstance(response, dict):
        raise RuntimeError("unexpected response shape from /api/chat")
    return response


def list_conversations(connection: ConnectionInfo) -> list[dict[str, Any]]:
    response = request_json(connection, "GET", "/api/conversations")
    if not isinstance(response, list):
        raise RuntimeError("unexpected response shape from /api/conversations")
    return response


def get_history(connection: ConnectionInfo, conversation_id: str, limit: int) -> list[dict[str, Any]]:
    query = urllib.parse.urlencode({"limit": limit})
    response = request_json(
        connection,
        "GET",
        f"/api/conversations/{conversation_id}/messages?{query}",
    )
    if not isinstance(response, list):
        raise RuntimeError("unexpected response shape from conversation history")
    return response


def cancel_run(connection: ConnectionInfo, conversation_id: str) -> dict[str, Any]:
    response = request_json(connection, "POST", f"/api/conversations/{conversation_id}/cancel", {})
    if not isinstance(response, dict):
        raise RuntimeError("unexpected response shape from cancel endpoint")
    return response


def print_conversations(conversations: list[dict[str, Any]], limit: int) -> None:
    for conversation in conversations[:limit]:
        title = conversation.get("title") or "<untitled>"
        print(f"{conversation.get('id')}  {title}  {conversation.get('updated_at')}")


def format_history_entry(message: dict[str, Any]) -> str:
    role = str(message.get("role", "unknown"))
    name = message.get("name")
    content = str(message.get("content", ""))
    header = role
    if name:
        header = f"{role}:{name}"
    return f"{header}> {content}"


def print_history(messages: list[dict[str, Any]]) -> None:
    for message in messages:
        print(format_history_entry(message))
        print()


def run_ask(args: argparse.Namespace, connection: ConnectionInfo, state: dict[str, Any]) -> int:
    conversation_id = resolve_conversation_id(args, state, connection)
    response = chat_once(connection, " ".join(args.message), conversation_id)
    set_conversation_id(state, connection, response["conversation_id"])
    save_state(args.state_file, state)
    if args.json:
        print(json.dumps(response, indent=2))
    else:
        print(f"[conversation {response['conversation_id']}]")
        print(response["message"])
    return 0


def run_list(args: argparse.Namespace, connection: ConnectionInfo) -> int:
    conversations = list_conversations(connection)
    if args.json:
        print(json.dumps(conversations[: args.limit], indent=2))
    else:
        print_conversations(conversations, args.limit)
    return 0


def run_history(args: argparse.Namespace, connection: ConnectionInfo, state: dict[str, Any]) -> int:
    conversation_id = resolve_conversation_id(args, state, connection)
    if not conversation_id:
        raise RuntimeError("no active conversation id; pass --conversation-id or start chatting first")
    messages = get_history(connection, conversation_id, args.limit)
    if args.json:
        print(json.dumps(messages, indent=2))
    else:
        print(f"[conversation {conversation_id}]")
        print_history(messages)
    return 0


def run_cancel(args: argparse.Namespace, connection: ConnectionInfo, state: dict[str, Any]) -> int:
    conversation_id = resolve_conversation_id(args, state, connection)
    if not conversation_id:
        raise RuntimeError("no active conversation id to cancel")
    response = cancel_run(connection, conversation_id)
    if args.json:
        print(json.dumps(response, indent=2))
    else:
        print(f"cancel requested for {response['conversation_id']}")
    return 0


def run_new(args: argparse.Namespace, connection: ConnectionInfo, state: dict[str, Any]) -> int:
    set_conversation_id(state, connection, None)
    save_state(args.state_file, state)
    if args.json:
        print(json.dumps({"base_url": connection.base_url, "conversation_id": None}, indent=2))
    else:
        print(f"cleared stored conversation for {connection.base_url}")
    return 0


def print_repl_help() -> None:
    print(
        textwrap.dedent(
            """\
            Commands:
              /help               Show this help
              /new                Start a new conversation
              /show               Show the current conversation id
              /use <uuid>         Switch to a specific conversation
              /list [limit]       List recent conversations
              /history [limit]    Show recent messages for the current conversation
              /cancel             Cancel the current run
              /quit               Exit
            """
        ).strip()
    )


def run_repl(args: argparse.Namespace, connection: ConnectionInfo, state: dict[str, Any]) -> int:
    conversation_id = resolve_conversation_id(args, state, connection)
    print(f"Connected to {connection.base_url} via {connection.source}")
    if conversation_id:
        print(f"Using conversation {conversation_id}")
    else:
        print("Starting without a stored conversation")
    print("Type /help for commands.")

    while True:
        try:
            line = input("you> ").strip()
        except EOFError:
            print()
            break
        except KeyboardInterrupt:
            print()
            break

        if not line:
            continue

        if line.startswith("/"):
            parts = shlex.split(line)
            command = parts[0]
            if command in {"/quit", "/exit"}:
                break
            if command == "/help":
                print_repl_help()
                continue
            if command == "/new":
                conversation_id = None
                set_conversation_id(state, connection, None)
                save_state(args.state_file, state)
                print("started a fresh conversation")
                continue
            if command == "/show":
                print(conversation_id or "<none>")
                continue
            if command == "/use":
                if len(parts) != 2:
                    print("usage: /use <conversation-id>", file=sys.stderr)
                    continue
                conversation_id = parts[1]
                set_conversation_id(state, connection, conversation_id)
                save_state(args.state_file, state)
                print(f"switched to {conversation_id}")
                continue
            if command == "/list":
                limit = int(parts[1]) if len(parts) > 1 else 10
                print_conversations(list_conversations(connection), limit)
                continue
            if command == "/history":
                if not conversation_id:
                    print("no active conversation", file=sys.stderr)
                    continue
                limit = int(parts[1]) if len(parts) > 1 else 20
                print_history(get_history(connection, conversation_id, limit))
                continue
            if command == "/cancel":
                if not conversation_id:
                    print("no active conversation", file=sys.stderr)
                    continue
                cancel_run(connection, conversation_id)
                print(f"cancel requested for {conversation_id}")
                continue

            print(f"unknown command: {command}", file=sys.stderr)
            continue

        response = chat_once(connection, line, conversation_id)
        conversation_id = response["conversation_id"]
        set_conversation_id(state, connection, conversation_id)
        save_state(args.state_file, state)
        print(f"jossie[{conversation_id}]> {response['message']}")

    return 0


def main() -> int:
    parser = build_parser()
    args = parser.parse_args()

    try:
        connection = resolve_connection(args)
        state = load_state(args.state_file)

        command = args.command or "repl"
        if command == "ask":
            return run_ask(args, connection, state)
        if command == "list":
            return run_list(args, connection)
        if command == "history":
            return run_history(args, connection, state)
        if command == "cancel":
            return run_cancel(args, connection, state)
        if command == "new":
            return run_new(args, connection, state)
        if command == "repl":
            return run_repl(args, connection, state)

        raise RuntimeError(f"unsupported command: {command}")
    except RuntimeError as exc:
        print(f"error: {exc}", file=sys.stderr)
        return 1


if __name__ == "__main__":
    raise SystemExit(main())
