"""Shared deterministic workloads for sn PGO training and measurement."""

from __future__ import annotations

import gzip
import json
import os
import subprocess
import threading
from dataclasses import dataclass
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer
from pathlib import Path
from typing import Self
from urllib.parse import parse_qs, urlsplit

REPOSITORY_ROOT = Path(__file__).resolve().parent.parent
SENSITIVE_ENV_MARKERS = ("API_KEY", "CREDENTIAL", "PASSWORD", "SECRET", "TOKEN")
OK = '"ok":true'
SYS_ID = "9d385017c611228701d22104cc95c371"
TOTAL = 5000


def record(number: int) -> dict[str, object]:
    return {
        "sys_id": f"{number:032x}",
        "sys_class_name": "incident",
        "number": f"INC{number:07d}",
        "short_description": f"Users on floor {number % 40} cannot reach the VPN gateway",
        "description": 'The gateway answers with error 800.\r\nIt started at 08:15. "Quoted", <b>marked</b>, naïve text.',
        "state": "In Progress",
        "priority": "2 - High",
        "impact": "2 - Medium",
        "urgency": "1 - High",
        "category": "network",
        "assigned_to": "Ada Lovelace",
        "assignment_group": "Network",
        "caller_id": "Grace Hopper",
        "opened_at": "2026-01-01 10:00:00",
        "sys_created_on": "2026-01-01 10:00:00",
        "sys_updated_on": "2026-01-02 11:30:00",
        "active": "true",
        "work_notes": "",
        "comments": "",
    }


def answer(path: str, query: dict[str, list[str]]) -> tuple[object, dict[str, str]]:
    if path.startswith("/api/now/ui/user/current_user"):
        user = {
            "user_sys_id": SYS_ID,
            "user_name": "ada",
            "user_display_name": "Ada Lovelace",
        }
        return {"result": user}, {}
    if path.startswith("/api/now/stats/"):
        groups = [
            {
                "stats": {"count": str(n * 7)},
                "groupby_fields": [{"field": "priority", "value": str(n)}],
            }
            for n in range(1, 40)
        ]
        return {"result": groups}, {}
    parts = path.strip("/").split("/")
    if len(parts) == 5 and parts[:3] == ["api", "now", "table"]:
        return {"result": record(1)}, {}
    offset = int(query.get("sysparm_offset", ["0"])[0])
    limit = int(query.get("sysparm_limit", ["20"])[0])
    if "numberSTARTSWITH" in query.get("sysparm_query", [""])[0]:
        limit = 1
    fields = [f for f in query.get("sysparm_fields", [""])[0].split(",") if f]
    rows = [record(n) for n in range(offset + 1, min(offset + limit, TOTAL) + 1)]
    if fields:
        rows = [{name: row.get(name, "") for name in fields} for row in rows]
    return {"result": rows}, {"x-total-count": str(TOTAL)}


class _MockInstanceHandler(BaseHTTPRequestHandler):
    protocol_version = "HTTP/1.1"

    def _reply(self) -> None:
        length = int(self.headers.get("content-length", "0"))
        if length:
            self.rfile.read(length)
        url = urlsplit(self.path)
        body, headers = answer(url.path, parse_qs(url.query))
        encoded = json.dumps(body, separators=(",", ":"), ensure_ascii=False).encode()
        if "gzip" in self.headers.get("accept-encoding", ""):
            encoded = gzip.compress(encoded, compresslevel=1, mtime=0)
            headers["content-encoding"] = "gzip"
        self.send_response(201 if self.command == "POST" else 200)
        self.send_header("content-type", "application/json")
        self.send_header("content-length", str(len(encoded)))
        for name, value in headers.items():
            self.send_header(name, value)
        self.end_headers()
        self.wfile.write(encoded)

    do_GET = _reply
    do_POST = _reply
    do_PATCH = _reply

    def log_message(self, format: str, *args: object) -> None:
        del format, args


class MockInstance:
    """A loopback-only server that answers like a ServiceNow instance."""

    def __init__(self) -> None:
        self._server = ThreadingHTTPServer(("127.0.0.1", 0), _MockInstanceHandler)
        self._thread = threading.Thread(target=self._server.serve_forever, daemon=True)

    @property
    def base_url(self) -> str:
        host, port = self._server.server_address[:2]
        return f"http://{host}:{port}"

    def __enter__(self) -> Self:
        self._thread.start()
        return self

    def __exit__(self, *exc_info: object) -> None:
        self._server.shutdown()
        self._server.server_close()
        self._thread.join(timeout=5)


@dataclass(frozen=True, slots=True)
class Workload:
    """One process invocation and its expected output."""

    name: str
    arguments: tuple[str, ...]
    expected_output: str
    repetitions: int = 1
    # Talks to the mock server; the benchmark takes fewer samples of these.
    network: bool = False


def prepare_fixture(root: Path, base_url: str) -> tuple[Path, dict[str, str]]:
    """Create an isolated config directory and point the CLI at the mock."""

    home = root / "home"
    project = root / "project"
    home.mkdir(parents=True, exist_ok=True)
    project.mkdir(parents=True, exist_ok=True)
    environment = {
        key: value
        for key, value in os.environ.items()
        if not key.startswith("SN_") and not any(marker in key.upper() for marker in SENSITIVE_ENV_MARKERS)
    }
    environment.update(
        {
            "HOME": str(home),
            "USERPROFILE": str(home),
            "XDG_CONFIG_HOME": str(home / ".config"),
            "APPDATA": str(home / "AppData" / "Roaming"),
            "SN_CONFIG_DIR": str(home / "sn"),
            "SN_INSTANCE": base_url,
            "SN_USERNAME": "ada",
            "SN_PASSWORD": "pgo-password",
            "SN_NO_UPDATE_NOTIFIER": "1",
            "NO_COLOR": "1",
            "NO_PROXY": "127.0.0.1,localhost",
            "no_proxy": "127.0.0.1,localhost",
        }
    )
    return project, environment


def network(name: str, *arguments: str, expected: str = OK, repetitions: int = 3) -> Workload:
    return Workload(name, arguments, expected, repetitions, network=True)


def training_workloads() -> tuple[Workload, ...]:
    return (
        Workload("version", ("--version",), "sn", repetitions=6),
        Workload("help", ("--help",), "Usage:", repetitions=4),
        Workload("list_help", ("table", "list", "--help"), "Usage:", repetitions=3),
        Workload(
            "api_help",
            ("api", "change-management", "list-conflict", "--help"),
            "Usage:",
            repetitions=3,
        ),
        Workload("api_search", ("api", "search", "attachment", "--agent"), OK, repetitions=3),
        Workload("api_list", ("api", "list", "--agent"), OK, repetitions=2),
        Workload("introspect", ("introspect", "--agent"), OK, repetitions=2),
        Workload("completions", ("completions", "zsh"), "sn", repetitions=2),
        network(
            "list_agent",
            "table",
            "list",
            "incident",
            "-n",
            "1000",
            "--agent",
            repetitions=6,
        ),
        network("list_small", "table", "list", "incident", "--agent", repetitions=6),
        network(
            "list_table",
            "table",
            "list",
            "incident",
            "-n",
            "200",
            "-f",
            "number,short_description,state",
            "-o",
            "table",
            expected="INC0000001",
        ),
        network(
            "list_csv",
            "table",
            "list",
            "incident",
            "-n",
            "500",
            "-o",
            "csv",
            expected="INC0000001",
        ),
        network(
            "list_select",
            "table",
            "list",
            "incident",
            "-n",
            "500",
            "--select",
            "number,state",
            "--agent",
        ),
        network(
            "list_jq",
            "table",
            "list",
            "incident",
            "-n",
            "300",
            "--jq",
            ".[].number",
            expected="INC0000001",
        ),
        network(
            "list_all",
            "table",
            "list",
            "incident",
            "--all",
            "--paginate",
            "offset",
            "-o",
            "jsonl",
            expected="INC0000001",
            repetitions=2,
        ),
        network("record", "table", "get", "incident", SYS_ID, "--agent"),
        network("incident", "incident", "show", SYS_ID, "--agent"),
        network("aggregate", "aggregate", "incident", "--group-by", "priority", "--agent"),
        network("update", "table", "update", "incident", SYS_ID, "-F", "state=2", "--agent"),
        network("raw", "raw", "GET", "/api/now/table/incident", "--agent"),
        network(
            "api_call",
            "api",
            "table",
            "list",
            "--table-name",
            "incident",
            "--agent",
        ),
    )


def held_out_workloads() -> tuple[Workload, ...]:
    return (
        Workload("startup_help", ("cicd", "updateset", "--help"), "Usage:"),
        Workload(
            "catalog_describe",
            ("api", "describe", "attachment", "create-file", "--agent"),
            OK,
        ),
        network("problem_list", "table", "list", "problem", "-n", "1000", "--agent"),
        network(
            "change_table",
            "table",
            "list",
            "change_request",
            "-n",
            "300",
            "-f",
            "number,priority,opened_at",
            "-o",
            "table",
            expected="INC0000001",
        ),
        network("task_detail", "table", "get", "task", SYS_ID, "--agent"),
    )


def run_workload(
    binary: Path,
    workload: Workload,
    *,
    cwd: Path,
    environment: dict[str, str],
    profile_directory: Path | None = None,
) -> subprocess.CompletedProcess[str]:
    child_environment = environment.copy()
    if profile_directory is not None:
        child_environment["LLVM_PROFILE_FILE"] = str(profile_directory / f"sn-{workload.name}-%m-%p.profraw")
    completed = subprocess.run(
        [str(binary), *workload.arguments],
        cwd=cwd,
        env=child_environment,
        input="",
        text=True,
        encoding="utf-8",
        capture_output=True,
        check=False,
        timeout=60,
    )
    output = completed.stdout + completed.stderr
    if completed.returncode != 0:
        raise RuntimeError(f"{workload.name} returned {completed.returncode}: {output.strip()[:2000]}")
    if workload.expected_output not in output:
        raise RuntimeError(f"{workload.name} did not contain {workload.expected_output!r}: {output.strip()[:2000]}")
    return completed


def executable_size(path: Path) -> tuple[int, int]:
    data = path.read_bytes()
    return len(data), len(gzip.compress(data, compresslevel=9, mtime=0))
