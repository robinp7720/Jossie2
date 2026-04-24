#!/usr/bin/env python3
from __future__ import annotations

import argparse
import base64
import hashlib
import json
import os
import shlex
import socket
import ssl
import struct
import subprocess
import sys
import textwrap
import time
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
DEFAULT_CONNECT_TIMEOUT = 15.0
DEFAULT_TURN_TIMEOUT = 180.0
DEFAULT_IDLE_TIMEOUT = 25.0
WS_GUID = "258EAFA5-E914-47DA-95CA-C5AB0DC85B11"


@dataclass
class ConnectionInfo:
    base_url: str
    token: str
    source: str


@dataclass
class TurnResult:
    conversation_id: str
    message: str
    transport: str
    run_id: str | None = None
    partial: bool = False


class TurnError(RuntimeError):
    def __init__(
        self,
        message: str,
        *,
        conversation_id: str | None = None,
        partial_message: str | None = None,
    ) -> None:
        super().__init__(message)
        self.conversation_id = conversation_id
        self.partial_message = partial_message


class WebSocketClient:
    def __init__(self, sock: socket.socket, leftover: bytes = b"") -> None:
        self.sock = sock
        self.buffer = bytearray(leftover)
        self.closed = False

    @classmethod
    def connect(cls, url: str, *, timeout: float) -> "WebSocketClient":
        parsed = urllib.parse.urlparse(url)
        if parsed.scheme not in {"ws", "wss"}:
            raise RuntimeError(f"unsupported websocket scheme: {parsed.scheme}")
        host = parsed.hostname
        if not host:
            raise RuntimeError("websocket URL is missing a host")
        port = parsed.port or (443 if parsed.scheme == "wss" else 80)
        raw_sock = socket.create_connection((host, port), timeout=timeout)
        if parsed.scheme == "wss":
            context = ssl.create_default_context()
            sock = context.wrap_socket(raw_sock, server_hostname=host)
        else:
            sock = raw_sock
        sock.settimeout(timeout)

        key = base64.b64encode(os.urandom(16)).decode("ascii")
        path = parsed.path or "/"
        if parsed.query:
            path = f"{path}?{parsed.query}"

        request = (
            f"GET {path} HTTP/1.1\r\n"
            f"Host: {host}:{port}\r\n"
            "Upgrade: websocket\r\n"
            "Connection: Upgrade\r\n"
            f"Sec-WebSocket-Key: {key}\r\n"
            "Sec-WebSocket-Version: 13\r\n"
            "\r\n"
        ).encode("utf-8")
        sock.sendall(request)

        response = bytearray()
        while b"\r\n\r\n" not in response:
            chunk = sock.recv(4096)
            if not chunk:
                raise RuntimeError("websocket handshake failed: unexpected EOF")
            response.extend(chunk)
            if len(response) > 65536:
                raise RuntimeError("websocket handshake failed: header too large")

        header_bytes, leftover = response.split(b"\r\n\r\n", 1)
        header_text = header_bytes.decode("utf-8", errors="replace")
        lines = header_text.split("\r\n")
        status_line = lines[0] if lines else ""
        if " 101 " not in status_line:
            raise RuntimeError(f"websocket handshake failed: {status_line}")

        headers: dict[str, str] = {}
        for line in lines[1:]:
            if ":" not in line:
                continue
            name, value = line.split(":", 1)
            headers[name.strip().lower()] = value.strip()

        accept = headers.get("sec-websocket-accept")
        expected = base64.b64encode(hashlib.sha1(f"{key}{WS_GUID}".encode("ascii")).digest()).decode("ascii")
        if accept != expected:
            raise RuntimeError("websocket handshake failed: invalid accept key")

        return cls(sock, leftover)

    def close(self) -> None:
        if self.closed:
            return
        try:
            self._send_frame(0x8, b"")
        except OSError:
            pass
        try:
            self.sock.close()
        finally:
            self.closed = True

    def send_text(self, text: str) -> None:
        self._send_frame(0x1, text.encode("utf-8"))

    def recv_text(self, *, timeout: float) -> str | None:
        self.sock.settimeout(timeout)
        message_parts = bytearray()
        current_opcode: int | None = None

        while True:
            first = self._recv_exact(1)
            if first is None:
                return None
            second = self._recv_exact(1)
            if second is None:
                return None

            first_byte = first[0]
            second_byte = second[0]
            fin = bool(first_byte & 0x80)
            opcode = first_byte & 0x0F
            masked = bool(second_byte & 0x80)
            length = second_byte & 0x7F

            if length == 126:
                extended = self._recv_exact(2)
                if extended is None:
                    return None
                length = struct.unpack("!H", extended)[0]
            elif length == 127:
                extended = self._recv_exact(8)
                if extended is None:
                    return None
                length = struct.unpack("!Q", extended)[0]

            mask = self._recv_exact(4) if masked else None
            payload = self._recv_exact(length) if length else b""
            if payload is None:
                return None
            if masked and mask is not None:
                payload = bytes(b ^ mask[i % 4] for i, b in enumerate(payload))

            if opcode == 0x8:
                self.closed = True
                return None
            if opcode == 0x9:
                self._send_frame(0xA, payload)
                continue
            if opcode == 0xA:
                continue
            if opcode in {0x1, 0x2}:
                current_opcode = opcode
                message_parts.extend(payload)
            elif opcode == 0x0:
                if current_opcode is None:
                    raise RuntimeError("received websocket continuation frame without a start frame")
                message_parts.extend(payload)
            else:
                continue

            if fin:
                if current_opcode != 0x1:
                    raise RuntimeError("received non-text websocket message")
                return message_parts.decode("utf-8")

    def _send_frame(self, opcode: int, payload: bytes) -> None:
        if self.closed:
            return
        first_byte = 0x80 | (opcode & 0x0F)
        mask_key = os.urandom(4)
        payload_len = len(payload)
        if payload_len < 126:
            header = bytes([first_byte, 0x80 | payload_len])
        elif payload_len < (1 << 16):
            header = bytes([first_byte, 0x80 | 126]) + struct.pack("!H", payload_len)
        else:
            header = bytes([first_byte, 0x80 | 127]) + struct.pack("!Q", payload_len)
        masked_payload = bytes(byte ^ mask_key[i % 4] for i, byte in enumerate(payload))
        self.sock.sendall(header + mask_key + masked_payload)

    def _recv_exact(self, count: int) -> bytes | None:
        while len(self.buffer) < count:
            try:
                chunk = self.sock.recv(max(4096, count - len(self.buffer)))
            except socket.timeout:
                raise
            if not chunk:
                if not self.buffer:
                    return None
                raise RuntimeError("unexpected EOF while reading websocket frame")
            self.buffer.extend(chunk)
        data = bytes(self.buffer[:count])
        del self.buffer[:count]
        return data


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
        "--profile",
        default=os.environ.get("JOSSIE_CHAT_PROFILE", "default"),
        help="Conversation profile name for isolating stored turn state, for example codex. Default: %(default)s",
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
    parser.add_argument(
        "--transport",
        choices=["ws", "http"],
        default="ws",
        help="Turn transport. `ws` is the reliable turn-based mode using /api/chat/stream. Default: %(default)s",
    )
    parser.add_argument(
        "--connect-timeout",
        type=float,
        default=DEFAULT_CONNECT_TIMEOUT,
        help="Connection timeout in seconds. Default: %(default)s",
    )
    parser.add_argument(
        "--turn-timeout",
        type=float,
        default=DEFAULT_TURN_TIMEOUT,
        help="Maximum wall-clock time for one turn before cancellation. Default: %(default)s",
    )
    parser.add_argument(
        "--idle-timeout",
        type=float,
        default=DEFAULT_IDLE_TIMEOUT,
        help="Maximum seconds without any streaming event before cancellation. Default: %(default)s",
    )
    parser.add_argument(
        "--show-events",
        action="store_true",
        help="Print streaming run events to stderr while waiting for a turn.",
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


def get_profile_state(state: dict[str, Any], base_url: str, profile_name: str) -> dict[str, Any]:
    base_profile = get_profile(state, base_url)
    if "conversation_id" in base_profile and "profiles" not in base_profile:
        legacy = base_profile.pop("conversation_id", None)
        nested = base_profile.setdefault("profiles", {})
        nested["default"] = {"conversation_id": legacy}
    profiles = base_profile.setdefault("profiles", {})
    return profiles.setdefault(profile_name, {})


def resolve_conversation_id(
    args: argparse.Namespace,
    state: dict[str, Any],
    connection: ConnectionInfo,
) -> str | None:
    if args.new:
        return None
    if args.conversation_id:
        return args.conversation_id
    profile_state = get_profile_state(state, connection.base_url, args.profile)
    return profile_state.get("conversation_id")


def set_profile_conversation_id(
    state: dict[str, Any],
    connection: ConnectionInfo,
    profile_name: str,
    conversation_id: str | None,
) -> None:
    profile = get_profile_state(state, connection.base_url, profile_name)
    if conversation_id:
        profile["conversation_id"] = conversation_id
    else:
        profile.pop("conversation_id", None)


def request_json(
    connection: ConnectionInfo,
    method: str,
    path: str,
    payload: dict[str, Any] | None = None,
    *,
    timeout: float = 600.0,
) -> Any:
    url = f"{connection.base_url}{path}"
    data = None
    headers = {"Authorization": f"Bearer {connection.token}"}
    if payload is not None:
        data = json.dumps(payload).encode("utf-8")
        headers["Content-Type"] = "application/json"
    request = urllib.request.Request(url, data=data, headers=headers, method=method)
    try:
        with urllib.request.urlopen(request, timeout=timeout) as response:
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


def websocket_url(connection: ConnectionInfo) -> str:
    parsed = urllib.parse.urlparse(connection.base_url)
    if parsed.scheme not in {"http", "https"}:
        raise RuntimeError(f"unsupported base URL scheme for websocket transport: {parsed.scheme}")
    ws_scheme = "wss" if parsed.scheme == "https" else "ws"
    query = urllib.parse.urlencode({"token": connection.token})
    path = "/api/chat/stream"
    if parsed.path and parsed.path != "/":
        path = f"{parsed.path.rstrip('/')}{path}"
    return urllib.parse.urlunparse((ws_scheme, parsed.netloc, path, "", query, ""))


def latest_assistant_message(messages: list[dict[str, Any]]) -> str | None:
    for message in reversed(messages):
        if message.get("role") == "assistant":
            return str(message.get("content", ""))
    return None


def maybe_cancel_turn(connection: ConnectionInfo, conversation_id: str | None) -> None:
    if not conversation_id:
        return
    try:
        cancel_run(connection, conversation_id)
    except RuntimeError:
        pass


def print_event(event: dict[str, Any]) -> None:
    event_type = str(event.get("type", "unknown"))
    conversation_id = event.get("conversation_id")
    run_id = event.get("run_id")
    if event_type == "assistant_thinking":
        print(
            f"[event] thinking iteration={event.get('iteration')} conversation={conversation_id} run={run_id}",
            file=sys.stderr,
        )
        return
    if event_type == "tool_called":
        print(
            f"[event] tool {event.get('tool')} args={event.get('arguments_preview')}",
            file=sys.stderr,
        )
        return
    if event_type == "tool_finished":
        print(
            f"[event] tool {event.get('tool')} done error={event.get('is_error')}",
            file=sys.stderr,
        )
        return
    if event_type == "error":
        print(f"[event] error {event.get('error')}", file=sys.stderr)
        return
    if event_type in {"run_started", "run_completed", "run_cancelled", "assistant_reset"}:
        print(f"[event] {event_type} conversation={conversation_id} run={run_id}", file=sys.stderr)


def stream_chat_turn(
    args: argparse.Namespace,
    connection: ConnectionInfo,
    message: str,
    conversation_id: str | None,
) -> TurnResult:
    client = WebSocketClient.connect(websocket_url(connection), timeout=args.connect_timeout)
    current_conversation_id = conversation_id
    current_run_id: str | None = None
    final_message: str | None = None
    partial_message = ""
    turn_started = time.monotonic()
    last_event_time = turn_started

    try:
        payload: dict[str, Any] = {"message": message}
        if current_conversation_id:
            payload["conversation_id"] = current_conversation_id
        client.send_text(json.dumps(payload))

        while True:
            now = time.monotonic()
            remaining_turn = args.turn_timeout - (now - turn_started)
            remaining_idle = args.idle_timeout - (now - last_event_time)
            if remaining_turn <= 0:
                maybe_cancel_turn(connection, current_conversation_id)
                raise TurnError(
                    "turn timed out; cancel requested",
                    conversation_id=current_conversation_id,
                    partial_message=final_message or partial_message or None,
                )
            if remaining_idle <= 0:
                maybe_cancel_turn(connection, current_conversation_id)
                raise TurnError(
                    "turn went idle; cancel requested",
                    conversation_id=current_conversation_id,
                    partial_message=final_message or partial_message or None,
                )

            timeout = min(remaining_turn, remaining_idle)
            try:
                raw_event = client.recv_text(timeout=timeout)
            except socket.timeout as exc:
                maybe_cancel_turn(connection, current_conversation_id)
                raise TurnError(
                    "turn stalled waiting for websocket events; cancel requested",
                    conversation_id=current_conversation_id,
                    partial_message=final_message or partial_message or None,
                ) from exc

            if raw_event is None:
                break

            last_event_time = time.monotonic()
            try:
                event = json.loads(raw_event)
            except json.JSONDecodeError:
                continue

            if args.show_events:
                print_event(event)

            event_conversation_id = event.get("conversation_id")
            if event_conversation_id:
                current_conversation_id = str(event_conversation_id)

            event_type = str(event.get("type", ""))
            if event_type == "run_started":
                run_id = event.get("run_id")
                current_run_id = str(run_id) if run_id else None
            elif event_type == "assistant_delta":
                partial_message += str(event.get("content", ""))
            elif event_type == "assistant_reset":
                partial_message = ""
            elif event_type == "message_created":
                msg = event.get("message", {})
                if isinstance(msg, dict) and msg.get("role") == "assistant":
                    final_message = str(msg.get("content", ""))
            elif event_type == "error":
                error_text = str(event.get("error", "unknown websocket error"))
                raise TurnError(
                    error_text,
                    conversation_id=current_conversation_id,
                    partial_message=final_message or partial_message or None,
                )
            elif event_type == "run_cancelled":
                raise TurnError(
                    "run cancelled",
                    conversation_id=current_conversation_id,
                    partial_message=final_message or partial_message or None,
                )
            elif event_type == "run_completed":
                break

        if not current_conversation_id:
            raise TurnError("turn ended without a conversation id")

        if not final_message:
            history = get_history(connection, current_conversation_id, 12)
            final_message = latest_assistant_message(history)
        if final_message is None:
            final_message = partial_message
        if not final_message:
            raise TurnError("turn completed but no assistant message was found", conversation_id=current_conversation_id)

        return TurnResult(
            conversation_id=current_conversation_id,
            message=final_message,
            transport="ws",
            run_id=current_run_id,
            partial=bool(partial_message and final_message != partial_message),
        )
    finally:
        client.close()


def run_turn(
    args: argparse.Namespace,
    connection: ConnectionInfo,
    message: str,
    conversation_id: str | None,
) -> TurnResult:
    if args.transport == "http":
        response = chat_once(connection, message, conversation_id)
        return TurnResult(
            conversation_id=str(response["conversation_id"]),
            message=str(response["message"]),
            transport="http",
        )
    return stream_chat_turn(args, connection, message, conversation_id)


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
    try:
        result = run_turn(args, connection, " ".join(args.message), conversation_id)
    except TurnError as exc:
        if exc.conversation_id:
            set_profile_conversation_id(state, connection, args.profile, exc.conversation_id)
            save_state(args.state_file, state)
        if args.json:
            payload = {
                "error": str(exc),
                "conversation_id": exc.conversation_id,
                "partial_message": exc.partial_message,
                "transport": args.transport,
            }
            print(json.dumps(payload, indent=2))
            return 1
        if exc.partial_message:
            print(exc.partial_message)
            print(file=sys.stderr)
        raise

    set_profile_conversation_id(state, connection, args.profile, result.conversation_id)
    save_state(args.state_file, state)
    if args.json:
        print(
            json.dumps(
                {
                    "conversation_id": result.conversation_id,
                    "message": result.message,
                    "transport": result.transport,
                    "run_id": result.run_id,
                    "partial": result.partial,
                },
                indent=2,
            )
        )
    else:
        print(f"[conversation {result.conversation_id} profile={args.profile} transport={result.transport}]")
        print(result.message)
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
    set_profile_conversation_id(state, connection, args.profile, None)
    save_state(args.state_file, state)
    if args.json:
        print(
            json.dumps(
                {"base_url": connection.base_url, "profile": args.profile, "conversation_id": None},
                indent=2,
            )
        )
    else:
        print(f"cleared stored conversation for {connection.base_url} profile={args.profile}")
    return 0


def print_repl_help() -> None:
    print(
        textwrap.dedent(
            """\
            Commands:
              /help               Show this help
              /new                Start a new conversation
              /show               Show the current conversation id
              /profile            Show the current profile name
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
    print(f"Profile: {args.profile}  Transport: {args.transport}")
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
                set_profile_conversation_id(state, connection, args.profile, None)
                save_state(args.state_file, state)
                print("started a fresh conversation")
                continue
            if command == "/show":
                print(conversation_id or "<none>")
                continue
            if command == "/profile":
                print(args.profile)
                continue
            if command == "/use":
                if len(parts) != 2:
                    print("usage: /use <conversation-id>", file=sys.stderr)
                    continue
                conversation_id = parts[1]
                set_profile_conversation_id(state, connection, args.profile, conversation_id)
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

        try:
            result = run_turn(args, connection, line, conversation_id)
        except TurnError as exc:
            if exc.conversation_id:
                conversation_id = exc.conversation_id
                set_profile_conversation_id(state, connection, args.profile, conversation_id)
                save_state(args.state_file, state)
            if exc.partial_message:
                print(f"jossie[{conversation_id or '?'}]~> {exc.partial_message}")
            print(f"turn error: {exc}", file=sys.stderr)
            continue

        conversation_id = result.conversation_id
        set_profile_conversation_id(state, connection, args.profile, conversation_id)
        save_state(args.state_file, state)
        print(f"jossie[{conversation_id}]> {result.message}")

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
