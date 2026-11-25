"""Builds crates/sn-core/catalog/catalog.json from ServiceNow's REST reference.

Usage:
    git clone --depth 1 --branch brazil https://github.com/ServiceNow/ServiceNowDocs
    uv run scripts/build_catalog.py ServiceNowDocs/markdown/api-reference/rest-apis

Every "URL format" section is one operation. The script stops when it cannot
read one, so the catalog never loses endpoints silently.
"""

# /// script
# requires-python = ">=3.11"
# ///

import json
import re
import sys
from enum import StrEnum
from html.parser import HTMLParser
from pathlib import Path
from typing import NotRequired, TypedDict


class Place(StrEnum):
    PATH = "path"
    QUERY = "query"
    BODY = "body"


class Param(TypedDict):
    name: str
    place: Place
    type: NotRequired[str]
    required: NotRequired[bool]
    text: NotRequired[str]
    fields: NotRequired[list["Param"]]


class Operation(TypedDict):
    id: str
    method: str
    path: str
    summary: str
    params: list[Param]
    content: NotRequired[str]
    writes: NotRequired[bool]


class Api(TypedDict):
    name: str
    title: str
    text: str
    doc: str
    operations: list[Operation]


OUTPUT: Path = Path(__file__).parent.parent / "crates/sn-core/catalog/catalog.json"
# Pages whose file name is not a good command name.
NAMES: dict[str, str] = {
    "Clotho-Time-Series-API": "metricbase",
    "appointment-open-api": "appointment-open",
    "service-catalog-open-api": "service-catalog-open",
    "agent-intelligence-api": "predictive-intelligence",
    "agent-api": "awa-agent",
    "actsub-api": "activity-subscriptions",
    "cust_central-api": "customer-central",
    "order_csm-api": "csm-order",
    "tmf-party-mgmt-API-v1": "party-management-v1",
    "tmf-party-mgmt-API-v2": "party-management-v2",
    "tmf622_product_ordering-api": "product-order",
    "ts-qualification-api": "technical-service-qualification",
    "usage-insight-data-exp-api": "uxa-data-export",
    "wsd_extra_serv_req-api": "wsd-extra-service-request",
    "wsd_grp_reserv-api": "wsd-group-reservation",
    "wsd_multi_reserv-api": "wsd-multi-reservation",
    "wsd_recur_reserv-api": "wsd-recurring-reservation",
    "ppm-api": "project-portfolio",
    "agnt_clnt_cll-api": "agent-client-collector",
    "applications-api": "cdm-applications",
    "attachment_csm-api": "csm-attachment",
    "auto-api": "automation-center",
    "bot-api": "virtual-agent-bot",
    "c_PerformanceAnalyticsAPI": "scorecards",
    "cccif-api": "cccif-media",
    "changesets-api": "cdm-changesets",
    "cicd-api": "cicd",
    "cs-catalog-api": "cloud-services-catalog",
    "editor-api": "cdm-editor",
    "event_open-api": "event-notification",
    "ext-user-mapping-api": "ai-search-user-mapping",
    "integrations-api": "install-base-item",
    "policies-api": "cdm-policies",
    "push-metrics-MID-server": "push-metrics",
    "queue-api": "awa-routing",
    "sam_soft_us_int-api": "software-usage",
    "shared_libraries-api": "cdm-shared-libraries",
    "snapshot-api": "cdm-snapshot",
    "synth-async-api": "synthetics-async",
    "versions-api": "cdm-versions",
}


# The "Default URL" of these sections repeats another section's; their own
# heading is right. Taking the URL would publish instead of unpublish.
PATHS: dict[tuple[str, str], str] = {
    (
        "wsd_reservation_invitees-api",
        "GET /api/sn_wsd_rsv/wsd_reservation_invitees/reservation/{sys_id}",
    ): "/api/sn_wsd_rsv/wsd_reservation_invitees/reservation/{sys_id}",
    (
        "agnt_clnt_cll-api",
        "GET /agents/policy/deactivate/{policy_id}",
    ): "/api/sn_agent/agents/policy/deactivate/{policy_id}",
    ("scim-api", "GET /scim/Companies/{company_id}"): "/api/now/scim/Companies/{company_id}",
    (
        "service-catalog-open-api",
        "GET /api/sn_tmf_api/catalogmanagement/serviceCategory/{id}",
    ): "/api/sn_tmf_api/catalogmanagement/serviceCategory/{id}",
    (
        "tmf-party-mgmt-API-v1",
        "GET /api/sn_tmf_api/v1/party/individual/{id}",
    ): "/api/sn_tmf_api/v1/party/individual/{id}",
    (
        "tmf-party-mgmt-API-v1",
        "PATCH/api/sn_tmf_api/v1/party/individual/{id}",
    ): "/api/sn_tmf_api/v1/party/individual/{id}",
    ("versions-api", "PUT /sn_cdm/versions/unpublish"): "/api/sn_cdm/versions/unpublish",
}
# One path with several messages: the summaries name them.
IDS: dict[str, list[str]] = {
    "custom-connector-gateway-api": [
        "add-document",
        "map-user",
        "delete-all-documents",
        "delete-all-user-mappings",
        "delete-documents-by-query",
        "delete-document",
        "delete-user-mapping",
    ],
}
# GET operations that change something.
CHANGES: re.Pattern[str] = re.compile(
    r"^(Activates|Deactivates|Publishes|Disables|Enables data|Runs|Stops|Restarts|Requests the log)"
)


class Cells(HTMLParser):
    """Reads the rows of tables; nested lists become text."""

    def __init__(self) -> None:
        super().__init__(convert_charrefs=True)
        self.tables: list[tuple[str, list[list[str]]]] = []
        self.depth: int = 0
        self.cell: list[str] | None = None

    def handle_starttag(self, tag: str, attrs: list[tuple[str, str | None]]) -> None:
        if tag == "table":
            self.depth += 1
            if self.depth == 1:
                self.tables.append((dict(attrs).get("class") or "", []))
        elif self.depth == 1 and tag == "tr":
            self.tables[-1][1].append([])
        elif self.depth == 1 and tag in ("td", "th"):
            self.cell = []

    def handle_endtag(self, tag: str) -> None:
        if tag == "table":
            self.depth -= 1
        elif self.depth == 1 and tag in ("td", "th") and self.cell is not None:
            if self.tables[-1][1]:
                self.tables[-1][1][-1].append("".join(self.cell))
            self.cell = None

    def handle_data(self, data: str) -> None:
        if self.cell is not None:
            self.cell.append(data)


def pipes_to_html(text: str) -> str:
    """Markdown tables as HTML, so that all tables keep their order."""

    def convert(match: re.Match[str]) -> str:
        rows: list[str] = []
        for line in match.group(0).strip().splitlines():
            if re.fullmatch(r"\|[-| :]+\|?", line.strip()):
                continue
            cells: list[str] = re.split(r"(?<!\\)\|", line.strip().strip("|"))
            rows.append("<tr>" + "".join(f"<td>{c}</td>" for c in cells) + "</tr>")
        return "\n<table>" + "".join(rows) + "</table>\n"

    # A table can start right behind "</table>" on the same line.
    text = text.replace("</table>|", "</table>\n|")
    return re.sub(r"(?:^\|.*\|[ ]*$\n?)+", convert, text, flags=re.MULTILINE)


def plain(text: str) -> str:
    """Markdown as plain text on one line."""
    text = re.sub(r"!\[[^\]]*\]\([^)]*\)", "", text)
    text = re.sub(r"\[([^\]]+)\]\([^)]*\)", r"\1", text)
    text = re.sub(r"\\(.)", r"\1", text)
    text = text.replace("&lt;", "<").replace("&gt;", ">").replace("&amp;", "&")
    text = re.sub(r"\*\*|`", "", text)
    return re.sub(r"\s+", " ", text).strip()


def shorten(text: str, limit: int) -> str:
    if len(text) <= limit:
        return text
    cut: str = text[:limit]
    stop: int = cut.rfind(". ")
    return cut[: stop + 1] if stop > limit // 3 else cut.rsplit(" ", 1)[0] + "…"


def read_param(cells: list[str], place: Place) -> Param | None:
    if len(cells) < 2:
        return None
    name: str = plain(cells[0])
    text: str = plain(cells[1])
    # "None", headers of the table, and prose in the name column.
    if not name or name.lower() in ("name", "none", "element", "parameter") or " " in name.split("<")[0].strip():
        return None
    param: Param = {"name": name, "place": place}
    kind = re.search(r"Data type: ?([A-Za-z]+(?: of [A-Za-z]+)*)", text)
    if kind:
        param["type"] = kind.group(1).strip()
    if place is Place.PATH or text.startswith("Required"):
        param["required"] = True
    if re.match(r"Optional\b", text) and "required" in param:
        del param["required"]
    text = re.sub(r"^(Required|Optional)\. ?", "", text)
    text = re.sub(r" ?(Data type|Table): ?[^.]{0,120}$", "", text)
    if text:
        param["text"] = shorten(text, 260)
    return param


def body_tree(params: list[Param]) -> list[Param]:
    """Nests `parent.child` names under `parent`."""
    roots: list[Param] = []
    known: dict[str, Param] = {}
    for param in params:
        full: str = param["name"]
        known.setdefault(full, param)
        parent, _, leaf = full.rpartition(".")
        if parent in known:
            param["name"] = leaf
            known[parent].setdefault("fields", []).append(param)
        else:
            roots.append(param)
    return roots


# Last path words that say what the call does.
ACTION: re.Pattern[str] = re.compile(
    r"^(accept|activate|add|apply|approve|assign|authenticate|back_?out|book|bulk|calculate|cancel|check|checkout|clear|"
    r"clone|close|commit|complete|copy|create|delete|deny|disable|enable|execute|export|find|generate|get|import|ingest|"
    r"initiate|install|invoke|move|notify|order_?now|preview|process|publish|query|re_?assign|re_?validate|refresh|register|"
    r"reject|remove|replay|request|reserve|reset|resolve|retrieve|rollback|run|search|send|start|stop|store|submit|sync|"
    r"transfer|transform|un_?publish|unregister|update|upload|validate|verify|wrap_?up)(?![a-z])"
)


def kebab(text: str) -> str:
    text = re.sub(r"([a-z0-9])([A-Z])", r"\1-\2", text)
    return re.sub(r"[^a-z0-9]+", "-", text.lower()).strip("-")


def operation_id(method: str, path: str, taken: set[str], api: str, gets: set[str]) -> str:
    """The action in the path, or get/list/create/... plus the words that tell it apart."""
    segments: list[str] = [s for s in path.split("/") if s][1:]
    words: list[str] = [kebab(s) for s in segments if not s.startswith("{")]
    # The namespace and the API's own name repeat what the command says.
    own: str = api.replace("-", "")
    words = [w for w in words[1:] if w.replace("-", "") not in (own, own + "s", own.removesuffix("s"))]
    by_id: bool = bool(segments) and segments[-1].startswith("{")
    # GET /table/{tableName} lists when GET /table/{tableName}/{sys_id} exists.
    if method == "GET" and any(
        other.startswith(path + "/{") and other.count("/") == path.count("/") + 1 for other in gets
    ):
        by_id = False
    last: str = next((s for s in reversed(segments) if not s.startswith("{")), "")
    snake: str = re.sub(r"([a-z0-9])([A-Z])", r"\1_\2", last).lower()
    if method in ("POST", "PUT") and words and ACTION.match(snake):
        verb: str = words.pop()
    else:
        verbs: dict[str, str] = {"POST": "create", "PUT": "replace", "PATCH": "update", "DELETE": "delete"}
        verb = verbs.get(method) or ("get" if by_id else "list")
    params: list[str] = [kebab(s) for s in segments if s.startswith("{")]
    names: list[str] = ["-".join([verb, *words[len(words) - count :]]) for count in range(len(words) + 1)]
    # A bare verb is only clear when nothing else of the path is left out.
    if words and verb in ("get", "list", "create", "replace", "update", "delete"):
        names = names[1:]
    names += [f"{names[0]}-by-{param}" for param in reversed(params)]
    names += [f"{names[0]}-{number}" for number in range(2, 50)]
    return next(name for name in names if name not in taken)


def api_name(stem: str) -> str:
    if stem in NAMES:
        return NAMES[stem]
    name: str = re.sub(r"^c_", "", stem)
    name = re.sub(r"[-_]?(open[-_])?api$", "", name, flags=re.IGNORECASE)
    name = re.sub(r"([a-z])([A-Z])", r"\1-\2", name)
    return re.sub(r"[^a-z0-9]+", "-", name.lower()).strip("-")


def clean_url(url: str) -> str:
    url = re.sub(r"^https?://[^/]+", "", plain(url).split("?")[0])
    url = re.sub(r"[{\[]api_version[}\]]/", "", url.replace("<", "{").replace(">", "}"))
    url = re.sub(r"\{\s*([^}/]+?)\s*\}", lambda m: "{" + m.group(1).replace(" ", "_") + "}", url)
    # Typing mistakes in the reference: stray spaces, open braces, one name.
    url = re.sub(r"\{(?=[^}]*(/|$))", "", url.replace(" ", ""))
    url = url.replace("CoostCenters", "CostCenters")
    url = "/" + url.strip("/")
    return url if url.startswith("/api/") else "/api" + url


def read_page(path: Path) -> tuple[Api | None, int]:
    text: str = path.read_text(encoding="utf-8")
    title_match = re.search(r"^# (.+)$", text, re.MULTILINE)
    title: str = plain(title_match.group(1)) if title_match else path.stem
    summary_match = re.search(r"^description: (.+)$", text, re.MULTILINE)
    name: str = api_name(path.stem)
    operations: list[Operation] = []
    taken: set[str] = set()
    sections: list[str] = re.split(r"^## ", text, flags=re.MULTILINE)[1:]
    count: int = sum(len(re.findall(r"^### URL format", s, re.MULTILINE)) for s in sections)
    gets: set[str] = set()
    for section in sections:
        if re.match(r"[^\n]*\bGET\b", section):
            for found in re.findall(r"Default URL:?\s*`([^`]+)`", section):
                gets.add(clean_url(found))
    for section in sections:
        if not re.search(r"^### URL format", section, re.MULTILINE):
            continue
        heading: str = plain(section.split("\n", 1)[0])
        method_match = re.search(r"\b(GET|POST|PUT|PATCH|DELETE|CREATE)\b", heading)
        url_area: str = section.split("### URL format", 1)[1]
        url_area = url_area.split("\n### ", 1)[0]
        urls: list[str] = [u for u in re.findall(r"`([^`]+)`", url_area) if "/" in u]
        default = re.search(r"Default URL:?\s*`([^`]+)`", url_area)
        url: str | None = default.group(1) if default else (urls[0] if urls else None)
        if url is None:
            bare = re.search(r"(?:URL:?|^)\s*(/?api/\S+)", url_area, re.MULTILINE)
            url = plain(bare.group(1)) if bare else None
        if method_match is None and url is not None:
            shown = re.search(r"-X (GET|POST|PUT|PATCH|DELETE)|--request (GET|POST|PUT|PATCH|DELETE)", section)
            method: str | None = next((g for g in shown.groups() if g), None) if shown else None
        else:
            method = method_match.group(1) if method_match else None
        if url is None or method is None:
            sys.exit(f"{path.name}: cannot read the operation '{heading}' (method {method}, url {url})")
        method = "POST" if method == "CREATE" else method
        override: str | None = next((p for (s, h), p in PATHS.items() if s == path.stem and heading.endswith(h)), None)
        url = override or clean_url(url)
        request_area: str = section.split("### Supported request parameters", 1)[-1]
        request_area = re.split(r"\n### (?:Headers|Status codes|Response body)", request_area)[0]
        cells: Cells = Cells()
        cells.feed(pipes_to_html(request_area) if "### Supported request parameters" in section else "")
        params: list[Param] = []
        body: list[Param] = []
        order: list[Place] = [Place.PATH, Place.QUERY, Place.BODY]
        for index, (css, rows) in enumerate(cells.tables):
            if "path" in css:
                place = Place.PATH
            elif "query" in css:
                place = Place.QUERY
            elif "body" in css:
                place = Place.BODY
            elif "header" in css:
                continue
            else:
                place = order[min(index, 2)]
            for row in rows:
                param = read_param(row, place)
                if param is None or param["name"] == "api_version":
                    continue
                target: list[Param] = body if place is Place.BODY else params
                # Some pages list a parameter twice.
                if all((p["place"], p["name"]) != (place, param["name"]) for p in target):
                    target.append(param)
        in_path: set[str] = set(re.findall(r"\{([^}]+)\}", url))
        params = [p for p in params if p["place"] is not Place.PATH or p["name"] in in_path]
        have: set[str] = {p["name"] for p in params if p["place"] is Place.PATH}
        params = [
            {"name": n, "place": Place.PATH, "required": True} for n in re.findall(r"\{([^}]+)\}", url) if n not in have
        ] + params
        for param in params:
            if param["place"] is Place.PATH:
                param["required"] = True
        prose: str = section.split("\n", 1)[1].split("### URL format", 1)[0].strip()
        op: Operation = {
            "id": operation_id(method, url, taken, name, gets),
            "method": method,
            "path": url,
            "summary": shorten(plain(prose.split("\n\n", 1)[0]), 200) or heading,
            "params": params + body_tree(body),
        }
        content = re.search(
            r"Content-Type.{0,400}?(application/[a-z0-9.+-]+[a-z0-9]|multipart/form-data|text/[a-z]+|\*/\*)",
            section,
            re.DOTALL,
        )
        if content and content.group(1) not in ("application/json", "application/xml"):
            op["content"] = content.group(1)
        if path.stem in IDS:
            op["id"] = IDS[path.stem][len(operations)]
        if method == "GET" and CHANGES.match(op["summary"]):
            op["writes"] = True
        if path.stem not in IDS and any((o["method"], o["path"]) == (method, url) for o in operations):
            sys.exit(f"{path.name}: '{heading}' repeats {method} {url}; check the page and add the right path to PATHS")
        taken.add(op["id"])
        operations.append(op)
    if not operations:
        return None, count
    doc: str = f"https://www.servicenow.com/docs/r/api-reference/rest-apis/{path.stem}.html"
    summary: str = shorten(plain(summary_match.group(1)) if summary_match else "", 200)
    return {"name": name, "title": title, "text": summary, "doc": doc, "operations": operations}, count


def main() -> None:
    if len(sys.argv) != 2 or not Path(sys.argv[1]).is_dir():
        sys.exit("pass the rest-apis directory of a ServiceNowDocs checkout: uv run scripts/build_catalog.py DIR")
    apis: list[Api] = []
    endpoints: int = 0
    for path in sorted(Path(sys.argv[1]).glob("*.md")):
        api, count = read_page(path)
        endpoints += count
        if api is not None:
            apis.append(api)
        elif path.stem != "api-rest":
            print(f"no endpoints: {path.name}", file=sys.stderr)
    names: list[str] = [a["name"] for a in apis]
    doubles: set[str] = {n for n in names if names.count(n) > 1}
    if doubles:
        sys.exit(f"two pages share the names {sorted(doubles)}; add them to NAMES")
    total: int = sum(len(a["operations"]) for a in apis)
    if total != endpoints:
        sys.exit(f"read {total} operations but the pages have {endpoints} URL sections")
    OUTPUT.write_text(json.dumps(apis, ensure_ascii=False, separators=(",", ":")) + "\n", encoding="utf-8")
    print(f"{len(apis)} APIs, {total} operations, {OUTPUT.stat().st_size // 1024} KiB")


if __name__ == "__main__":
    main()
