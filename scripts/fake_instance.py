"""A small fake ServiceNow instance, to try the workspace without a real one.

    python3 scripts/fake_instance.py --port 8765 [--latency 0.3] [--forbid sys_audit]
    SN_INSTANCE=http://127.0.0.1:8765 SN_USERNAME=ada \
        SN_PASSWORD=pw sn tui

POST /fake/touch/TABLE/SYS_ID changes a record as another user would.
POST /fake/expire makes every request answer 401 until /fake/expire again.
"""

import argparse
import hashlib
import json
import random
import re
import threading
import time
from datetime import UTC, datetime, timedelta
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer
from urllib.parse import parse_qs, urlparse

NOW = datetime.now(UTC).replace(microsecond=0, tzinfo=None)
rng = random.Random(7)
LOCK = threading.Lock()
EXPIRED = False


def sid(text):
    return hashlib.md5(text.encode()).hexdigest()


def ts(delta=timedelta()):
    return (NOW - delta).strftime("%Y-%m-%d %H:%M:%S")


TABLES = {}
DISPLAY = {}  # sys_id -> label
CHOICES = {}  # (table, field) -> [(value, label)]
PARENTS = {}


def choice(table, field, pairs):
    CHOICES[(table, field)] = pairs


TASK_STATES = [("-5", "Pending"), ("1", "Open"), ("2", "Work in Progress"), ("3", "Closed Complete"), ("4", "Closed Incomplete"), ("7", "Closed Skipped")]
choice("task", "state", TASK_STATES)
choice("incident", "state", [("1", "New"), ("2", "In Progress"), ("3", "On Hold"), ("6", "Resolved"), ("7", "Closed"), ("8", "Canceled")])
choice("problem", "state", [("101", "New"), ("102", "Assess"), ("103", "Root Cause Analysis"), ("104", "Fix in Progress"), ("106", "Resolved"), ("107", "Closed")])
choice("change_request", "state", [("-5", "New"), ("-4", "Assess"), ("-3", "Authorize"), ("-2", "Scheduled"), ("-1", "Implement"), ("0", "Review"), ("3", "Closed"), ("4", "Canceled")])
choice("change_request", "type", [("normal", "Normal"), ("standard", "Standard"), ("emergency", "Emergency")])
choice("change_request", "risk", [("2", "High"), ("3", "Moderate"), ("4", "Low")])
choice("change_request", "close_code", [("successful", "Successful"), ("successful_issues", "Successful with issues"), ("unsuccessful", "Unsuccessful")])
choice("task", "priority", [("1", "1 - Critical"), ("2", "2 - High"), ("3", "3 - Moderate"), ("4", "4 - Low"), ("5", "5 - Planning")])
choice("task", "urgency", [("1", "1 - High"), ("2", "2 - Medium"), ("3", "3 - Low")])
choice("task", "impact", [("1", "1 - High"), ("2", "2 - Medium"), ("3", "3 - Low")])
choice("incident", "category", [("inquiry", "Inquiry / Help"), ("software", "Software"), ("hardware", "Hardware"), ("network", "Network"), ("database", "Database")])
choice("incident", "close_code", [("Duplicate", "Duplicate"), ("Known error", "Known error"), ("No resolution provided", "No resolution provided"), ("Resolved by caller", "Resolved by caller"), ("Resolved by change", "Resolved by change"), ("Solution provided", "Solution provided"), ("Workaround provided", "Workaround provided")])
choice("incident", "contact_type", [("email", "Email"), ("phone", "Phone"), ("self-service", "Self-service"), ("walk-in", "Walk-in")])
choice("problem", "resolution_code", [("fix_applied", "Fix Applied"), ("risk_accepted", "Risk Accepted"), ("duplicate", "Duplicate")])
choice("sysapproval_approver", "state", [("requested", "Requested"), ("approved", "Approved"), ("rejected", "Rejected"), ("not_required", "No Longer Required")])
choice("sc_request", "request_state", [("in_process", "In Process"), ("requested", "Pending Approval"), ("closed_complete", "Closed Complete")])
choice("kb_knowledge", "workflow_state", [("draft", "Draft"), ("published", "Published"), ("retired", "Retired")])
choice("cmdb_ci", "operational_status", [("1", "Operational"), ("2", "Non-Operational"), ("3", "Repair in Progress"), ("6", "Retired")])
choice("sys_update_set", "state", [("in progress", "In progress"), ("complete", "Complete"), ("ignore", "Ignore")])
choice("sys_script", "when", [("before", "before"), ("after", "after"), ("async", "async"), ("before_display", "display")])
choice("sys_hub_flow", "status", [("published", "Published"), ("draft", "Draft")])
choice("sys_hub_flow", "type", [("flow", "Flow"), ("subflow", "Subflow")])

for t in ["incident", "problem", "change_request", "change_task", "problem_task", "sc_request", "sc_req_item", "sc_task"]:
    PARENTS[t] = "task"
PARENTS.update({"cmdb_ci_server": "cmdb_ci", "cmdb_ci_service": "cmdb_ci", "cmdb_ci_computer": "cmdb_ci"})

CLASS_LABELS = {"incident": "Incident", "problem": "Problem", "change_request": "Change Request", "change_task": "Change Task", "problem_task": "Problem Task", "sc_request": "Request", "sc_req_item": "Requested Item", "sc_task": "Catalog Task", "cmdb_ci_server": "Server", "cmdb_ci_service": "Business Service", "cmdb_ci_computer": "Computer", "task": "Task"}


def chain(table):
    out = [table]
    while table in PARENTS:
        table = PARENTS[table]
        out.append(table)
    return out


def choices(table, field):
    for t in chain(table):
        if (t, field) in CHOICES:
            return CHOICES[(t, field)]
    return []


def add(table, label, /, **fields):
    row = {"sys_id": sid(f"{table}:{label}"), "sys_class_name": table if table in CLASS_LABELS else ""}
    row.update({"sys_updated_on": ts(), "sys_created_on": ts(), "sys_updated_by": "system", "sys_created_by": "system", "sys_mod_count": "0"})
    if not row["sys_class_name"]:
        del row["sys_class_name"]
    row.update(fields)
    TABLES.setdefault(table, []).append(row)
    DISPLAY[row["sys_id"]] = label
    return row["sys_id"]


ME = add("sys_user", "Ada Lovelace", name="Ada Lovelace", user_name="ada", email="ada@example.com", title="Service desk lead", active="true", user_password="hash")
PEOPLE = [ME]
for name, title in [("Bob Stone", "Network engineer"), ("Carla Díaz", "Finance analyst"), ("Dmitri Ivanov", "Database administrator"), ("Emi 田中", "Tokyo office manager"), ("Frank O'Neil", "Field technician"), ("Grace Hopper", "Change manager"), ("Hassan Ali", "Security analyst"), ("Ingrid Berg", "HR partner")]:
    user = name.split()[0].lower()
    PEOPLE.append(add("sys_user", name, name=name, user_name=user, email=f"{user}@example.com", title=title, active="true"))
GROUPS = [add("sys_user_group", g, name=g, description=d, manager=rng.choice(PEOPLE), active="true") for g, d in [("Service Desk", "First line"), ("Network", "LAN, WAN, VPN"), ("Database", "Oracle and PostgreSQL"), ("Hardware", "Laptops and printers"), ("CAB Approval", "Change advisory board")]]
for g in GROUPS[:2]:
    add("sys_user_grmember", f"me {g}", group=g, user=ME)
add("sys_user_grmember", "bob", group=GROUPS[1], user=PEOPLE[1])
CIS = []
for name, cls, ip in [("prod-db-01", "cmdb_ci_server", "10.2.0.11"), ("prod-db-02", "cmdb_ci_server", "10.2.0.12"), ("vpn-gw-emea", "cmdb_ci_server", "10.9.1.1"), ("mail-relay", "cmdb_ci_server", "10.1.4.20"), ("SAP Finance", "cmdb_ci_service", ""), ("Email", "cmdb_ci_service", ""), ("lap-00421", "cmdb_ci_computer", "10.40.3.7"), ("lap-00987", "cmdb_ci_computer", "10.40.3.19")]:
    CIS.append(add(cls, name, name=name, short_description={"cmdb_ci_server": "Linux server, Frankfurt", "cmdb_ci_service": "Business service", "cmdb_ci_computer": "ThinkPad X1"}[cls], operational_status=rng.choice(["1", "1", "1", "3"]), ip_address=ip, support_group=rng.choice(GROUPS), sys_updated_on=ts(timedelta(days=rng.randint(1, 300)))))
add("cmdb_rel_ci", "r1", parent=CIS[4], child=CIS[0], type="Depends on::Used by")
add("cmdb_rel_ci", "r2", parent=CIS[5], child=CIS[3], type="Depends on::Used by")

SUMMARIES = [
    ("Email down for the whole Frankfurt office", "1", "network"),
    ("VPN disconnects every few minutes from home", "2", "network"),
    ("SAP rejects invoices over 10,000 EUR with error F5-702", "2", "software"),
    ("東京オフィスのプリンターが応答しません (printer offline)", "3", "hardware"),
    ("Cannot log in after password reset", "3", "inquiry"),
    ("Laptop battery swells, keyboard lifts up", "2", "hardware"),
    ("Shared drive H: is read-only since the migration", "3", "software"),
    ("Database prod-db-01 reports slow queries at night", "2", "database"),
    ("New starter needs access to the payroll folder", "4", "inquiry"),
    ("Teams calls have no audio on headsets", "3", "software"),
    ("Badge reader at gate 4 beeps but does not open", "4", "hardware"),
    ("Monitor flickers when docked", "5", "hardware"),
    ("Certificate of the intranet expires on Friday", "2", "network"),
    ("Outlook search finds nothing older than one year", "4", "software"),
]


def journal(task, element, text, by, delta):
    add("sys_journal_field", f"{task}{element}{delta}{text}", element_id=task, element=element, value=text, sys_created_on=ts(delta), sys_created_by=by, name="task")


def audit(table, key, field, old, new, by, delta):
    add("sys_audit", f"{key}{field}{delta}{new}", tablename=table, documentkey=key, fieldname=field, oldvalue=old, newvalue=new, user=by, sys_created_on=ts(delta))


INCIDENTS = []
for i in range(64):
    summary, priority, category = SUMMARIES[i % len(SUMMARIES)]
    if i >= len(SUMMARIES):
        summary = f"{summary} (again, floor {i % 9 + 1})"
        priority = rng.choice(["2", "3", "3", "4", "4", "5"])
    number = f"INC{10001 + i:07d}"
    age = timedelta(minutes=rng.randint(2, 60 * 24 * (1 + i // 6)))
    owner = ME if i % 7 == 0 else ("" if i % 7 == 3 else rng.choice(PEOPLE[1:]))
    state = rng.choice(["1", "2", "2", "3"]) if owner else "1"
    key = add("incident", number, number=number, short_description=summary, description=f"The caller reports: {summary.lower()}.\n\nIt started this morning. A restart did not help.\nOther colleagues on the same floor see the same.\nTried: cable, other account, other network.", priority=priority, urgency="1" if priority == "1" else "2", impact="1" if priority == "1" else "2", state=state, category=category, caller_id=rng.choice(PEOPLE[2:]), opened_by=rng.choice(PEOPLE), assigned_to=owner, assignment_group=rng.choice(GROUPS[:4]) if i % 5 else GROUPS[0], cmdb_ci=rng.choice(CIS), contact_type=rng.choice(["phone", "email", "self-service"]), opened_at=ts(age + timedelta(hours=3)), due_date=ts(timedelta(hours=rng.randint(-60, 40))) if i % 4 == 0 else "", active="true", sys_updated_on=ts(age), sys_updated_by=rng.choice(["bob", "grace", "ada"]), sys_mod_count=str(rng.randint(1, 12)), close_code="", close_notes="", parent_incident="", problem_id="", rfc="", comments="", work_notes="", made_sla="true", u_api_token="abc123", on_hold_reason="")
    INCIDENTS.append(key)
    journal(key, "comments", "Hello, when will this be fixed? My whole team waits.", "carla", age + timedelta(hours=2))
    journal(key, "work_notes", "Looked at the logs of the gateway:\n\n  10:02 tunnel up\n  10:07 tunnel down (DPD timeout)\n\nIt is the **DPD timer**. Asked Network to raise it to 30 s on both sides before we try a firmware update, because an update needs a change and a maintenance window.", "bob", age + timedelta(hours=1))
    if i % 3 == 0:
        journal(key, "comments", "[code]<p>We found the cause and prepare a <b>fix</b>.</p>[/code]", "ada", age)
    audit("incident", key, "state", "1", state, "bob", age + timedelta(hours=1))
    audit("incident", key, "assigned_to", "", owner, "bob", age + timedelta(hours=1))
    audit("incident", key, "priority", "3", priority, "grace", age)
    add("task_sla", f"{number} P{priority}", task=key, sla=add("contract_sla", f"Priority {priority} resolution ({['', '4', '8', '24', '72', '120'][int(priority)]} hours)"), stage="in_progress", has_breached="true" if i % 5 == 0 else "false", business_percentage=str([112.4, 34.1, 81.0, 8.3][i % 4] if i % 5 else 141.2), percentage="50", business_time_left="3 Hours 12 Minutes" if i % 5 else "0 Seconds", time_left="", planned_end_time=ts(timedelta(hours=-3 if i % 5 else 2)))
    add("task_ci", f"{number} ci", task=key, ci_item=rng.choice(CIS))
for a, b in [(1, 0), (2, 0)]:
    TABLES["incident"][a]["parent_incident"] = INCIDENTS[b]
choice("task_sla", "stage", [("in_progress", "In progress"), ("breached", "Breached"), ("completed", "Completed")])

for i, text in enumerate(["VPN concentrator drops sessions under load", "Mail relay queue grows without limit", "SAP posting period check is wrong for EUR"]):
    number = f"PRB{40001 + i:07d}"
    key = add("problem", number, number=number, short_description=text, state=["102", "103", "104"][i], priority=["2", "1", "3"][i], assigned_to=[ME, PEOPLE[1], ""][i], assignment_group=GROUPS[1], active="true", sys_updated_on=ts(timedelta(days=i)), comments="", work_notes="", cause_notes="", fix_notes="", resolution_code="", close_notes="", opened_at=ts(timedelta(days=9)))
    TABLES["incident"][i]["problem_id"] = key
    add("problem_task", f"PTASK{i}", number=f"PTASK{10 + i:07d}", short_description="Collect traces", state="2", problem=key, parent=key, assigned_to=PEOPLE[1], active="true", comments="", work_notes="", priority="3")

CHANGES = []
for i, (text, kind, state) in enumerate([("Raise the DPD timer on vpn-gw-emea", "normal", "-4"), ("Firmware 12.4 on the core switches", "normal", "-3"), ("Renew the intranet certificate", "standard", "-2"), ("Move prod-db-02 to the new storage", "normal", "-5"), ("Emergency: restart mail-relay", "emergency", "-1"), ("Quarterly patching of Windows servers", "standard", "-2")]):
    number = f"CHG{30001 + i:07d}"
    key = add("change_request", number, number=number, short_description=text, description="See the plan.", type=kind, state=state, risk=["3", "2", "4", "2", "3", "4"][i], priority="3", assigned_to=[PEOPLE[1], PEOPLE[6], ME, PEOPLE[3], PEOPLE[1], ""][i], assignment_group=GROUPS[1], requested_by=PEOPLE[6], start_date=ts(timedelta(hours=-20 * i + 10)), end_date=ts(timedelta(hours=-20 * i + 8)), justification="Users lose their sessions.", implementation_plan="1. Announce\n2. Change the timer\n3. Watch for 30 minutes", backout_plan="Put the old value back.", test_plan="Connect from home for one hour.", active="true", sys_updated_on=ts(timedelta(hours=5 * i)), comments="", work_notes="", close_code="", close_notes="", opened_at=ts(timedelta(days=3)), cmdb_ci=CIS[2])
    CHANGES.append(key)
    for j in range(2):
        add("change_task", f"CTASK{i}{j}", number=f"CTASK{i * 10 + j:07d}", short_description=["Announce to users", "Implement and watch"][j], state="1", change_request=key, parent=key, assigned_to="", active="true", comments="", work_notes="", priority="4", assignment_group=GROUPS[1])
    for who, st in [(ME, "requested"), (PEOPLE[6], "approved" if i % 2 else "requested")]:
        if i < 4:
            add("sysapproval_approver", f"app {number} {who}", sysapproval=key, document_id=key, approver=who, state=st, source_table="change_request", comments="", sys_created_on=ts(timedelta(hours=7 * i + 1)), sys_updated_on=ts(timedelta(hours=7 * i + 1)), sys_mod_count="0")

ITEM = add("sc_cat_item", "Standard laptop", name="Standard laptop", short_description="14-inch laptop with dock", category="Hardware", price="1,240.00 EUR", active="true")
for n, d, c, p in [("Monitor 27\"", "QHD monitor, USB-C", "Hardware", "310.00 EUR"), ("VPN access", "Remote access for staff", "Access", "0.00 EUR"), ("Adobe Creative Cloud", "One seat for one year", "Software", "640.00 EUR"), ("New starter package", "Everything for day one", "Onboarding", "1,900.00 EUR")]:
    add("sc_cat_item", n, name=n, short_description=d, category=c, price=p, active="true")
REQ = add("sc_request", "REQ0020001", number="REQ0020001", short_description="Laptop for Emi", request_state="requested", requested_for=ME, price="1,240.00 EUR", active="true", opened_at=ts(timedelta(days=2)), opened_by=ME, sys_updated_on=ts(timedelta(hours=9)), comments="", work_notes="", assigned_to="", assignment_group="", state="1", priority="4")
RITM = add("sc_req_item", "RITM0030001", number="RITM0030001", short_description="Standard laptop", cat_item=ITEM, request=REQ, state="1", stage="Waiting for approval", active="true", comments="", work_notes="", assigned_to="", assignment_group=GROUPS[3], priority="4")
for i, text in enumerate(["Order the laptop", "Install the image", "Hand it over in Tokyo"]):
    add("sc_task", f"SCTASK{i}", number=f"SCTASK{50001 + i:07d}", short_description=text, state="1", request_item=RITM, parent=RITM, assigned_to=[ME, "", PEOPLE[5]][i], assignment_group=GROUPS[3] if i != 1 else GROUPS[0], priority="4", active="true", due_date=ts(timedelta(hours=[30, -20, -50][i])), sys_updated_on=ts(timedelta(hours=i)), comments="", work_notes="", close_notes="")

for i, (title, author) in enumerate([("How to connect to the VPN from home", 1), ("Reset your password yourself", 0), ("Set up the Tokyo printer (東京)", 4), ("What to do when SAP shows F5-702", 2)]):
    add("kb_knowledge", f"KB{i}", number=f"KB{10 + i:07d}", short_description=title, text=f"<h2>{title}</h2><p>Follow these steps:</p><ul><li>Open the portal</li><li>Choose <b>Connect</b></li></ul>", kb_knowledge_base="IT", workflow_state="published", author=PEOPLE[author], sys_updated_on=ts(timedelta(days=20 * i + 2)))
for i, (n, st) in enumerate([("VPN timer 2026-11", "in progress"), ("Incident form cleanup", "in progress"), ("Default", "in progress"), ("October release", "complete")]):
    key = add("sys_update_set", n, name=n, state=st, application="Global", sys_created_by="ada", sys_updated_on=ts(timedelta(days=i)), is_default="false", description="")
    for j in range(3):
        add("sys_update_xml", f"x{i}{j}", name=f"sys_script_{j}", type="Business Rule", target_name=["Set priority", "Notify owner", "Close children"][j], action="INSERT_OR_UPDATE", update_set=key, sys_created_on=ts())
for n in ["IncidentUtils", "ChangeConflict", "VPNHelper", "global.JSONParser"]:
    add("sys_script_include", n, name=n, api_name=f"global.{n}", client_callable=rng.choice(["true", "false"]), active="true", script="var IncidentUtils = Class.create();\nIncidentUtils.prototype = {\n\tinitialize: function() {\n\t\tthis.max = 5;\n\t},\n\ttype: 'IncidentUtils'\n};", sys_updated_by="ada", sys_updated_on=ts(timedelta(days=rng.randint(0, 90))))
for n, c in [("Set priority", "incident"), ("Notify owner", "incident"), ("Close children", "change_request"), ("Check dates", "change_request")]:
    add("sys_script", n, name=n, collection=c, when=rng.choice(["before", "after"]), order="100", active=rng.choice(["true", "false"]), script="(function executeRule(current, previous) {\n\tcurrent.priority = 1;\n})(current, previous);", sys_updated_by="ada", sys_updated_on=ts(timedelta(days=rng.randint(0, 90))))
for n, s in [("Laptop approval", "published"), ("Escalate breached SLAs", "published"), ("Sync users", "draft")]:
    add("sys_hub_flow", n, name=n, type="flow", status=s, sys_scope="Global", active="true", sys_updated_by="ada", sys_updated_on=ts(timedelta(days=3)))
for i in range(4):
    add("u_vendor_contract", f"c{i}", u_vendor=["Acme", "Globex", "Initech", "Umbrella"][i], u_value=str(1000 * i), state=["open", "closed", "open", "draft"][i], sys_updated_on=ts(timedelta(days=i)))
for n, label in [("incident", "Incident"), ("u_vendor_contract", "Vendor contract"), ("cmdb_ci_server", "Server"), ("sys_user", "User"), ("sys_properties", "System Property"), ("alm_asset", "Asset"), ("sys_email", "Email")]:
    add("sys_db_object", n, name=n, label=label)
ATTACHMENTS = [
    {"sys_id": sid("att1"), "file_name": "gateway.log", "size_bytes": "48213", "content_type": "text/plain", "table_name": "incident", "table_sys_id": INCIDENTS[0], "sys_created_by": "bob", "sys_created_on": ts(timedelta(hours=2))},
    {"sys_id": sid("att2"), "file_name": "写真 of the printer.png", "size_bytes": "1834211", "content_type": "image/png", "table_name": "incident", "table_sys_id": INCIDENTS[0], "sys_created_by": "emi", "sys_created_on": ts(timedelta(hours=1))},
]

TYPES = {"short_description": ("string", 160), "description": ("string", 4000), "close_notes": ("string", 4000), "cause_notes": ("string", 4000), "fix_notes": ("string", 4000), "justification": ("string", 4000), "implementation_plan": ("string", 4000), "backout_plan": ("string", 4000), "test_plan": ("string", 4000), "script": ("script", 8000), "text": ("html", 65000), "comments": ("journal_input", 4000), "work_notes": ("journal_input", 4000), "opened_at": ("glide_date_time", 40), "due_date": ("due_date", 40), "start_date": ("glide_date_time", 40), "end_date": ("glide_date_time", 40), "active": ("boolean", 40), "client_callable": ("boolean", 40), "made_sla": ("boolean", 40), "price": ("currency", 40), "order": ("integer", 40), "u_value": ("integer", 40)}
REFS = {"assigned_to": "sys_user", "caller_id": "sys_user", "opened_by": "sys_user", "requested_for": "sys_user", "requested_by": "sys_user", "approver": "sys_user", "author": "sys_user", "manager": "sys_user", "user": "sys_user", "assignment_group": "sys_user_group", "support_group": "sys_user_group", "group": "sys_user_group", "cmdb_ci": "cmdb_ci", "ci_item": "cmdb_ci", "parent": "task", "parent_incident": "incident", "problem_id": "problem", "rfc": "change_request", "sysapproval": "task", "document_id": "task", "request": "sc_request", "request_item": "sc_req_item", "cat_item": "sc_cat_item", "change_request": "change_request", "problem": "problem", "task": "task", "sla": "contract_sla", "child": "cmdb_ci", "update_set": "sys_update_set"}
LABELS = {"caller_id": "Caller", "assigned_to": "Assigned to", "assignment_group": "Assignment group", "cmdb_ci": "Configuration item", "opened_at": "Opened", "due_date": "Due date", "sys_updated_on": "Updated", "sys_created_on": "Created", "sys_id": "Sys ID", "contact_type": "Channel", "problem_id": "Problem", "rfc": "Change request", "made_sla": "Made SLA", "start_date": "Planned start date", "end_date": "Planned end date", "u_api_token": "API token", "close_code": "Resolution code", "close_notes": "Resolution notes"}


def meta(table):
    names = {}
    for t in TABLES:
        if table in chain(t):
            for row in TABLES[t][:3]:
                names.update(dict.fromkeys(row))
    columns = {}
    for n in names:
        kind, length = TYPES.get(n, ("string", 100))
        if n in REFS:
            kind, length = "reference", 32
        if n.startswith("sys_") and n.endswith("_on"):
            kind = "glide_date_time"
        columns[n] = {"name": n, "label": LABELS.get(n, n.replace("sys_", "").replace("_", " ").capitalize()), "type": kind, "max_length": length, "mandatory": n in ("caller_id", "short_description") and table == "incident", "read_only": n.startswith("sys_") or n in ("number", "opened_by"), "choices": [{"value": v, "label": l} for v, l in choices(table, n)]}
        if n in REFS:
            columns[n]["reference"] = REFS[n]
    return columns


def find(key):
    for t, rows in TABLES.items():
        for r in rows:
            if r["sys_id"] == key:
                return t, r
    return None, None


def value(row, field):
    head, _, rest = field.partition(".")
    v = row.get(head)
    if not rest:
        return v
    _, target = find(v or "")
    return value(target, rest) if target else ""


def owner_table(table, row, field):
    head, _, rest = field.partition(".")
    if rest:
        t, target = find(row.get(head) or "")
        return owner_table(t or "", target or {}, rest)
    return row.get("sys_class_name") or table, head


def shown(table, row, field):
    v = value(row, field) or ""
    t, f = owner_table(table, row, field)
    for cv, label in choices(t, f):
        if cv == v:
            return label
    if f == "sys_class_name":
        return CLASS_LABELS.get(v, v)
    if re.fullmatch(r"[0-9a-f]{32}", v) and f != "sys_id":
        return DISPLAY.get(v, v)
    if f in ("comments", "work_notes"):
        return ""
    return v


def rows_of(table):
    out = []
    for t, rows in TABLES.items():
        if table in chain(t):
            out.extend((t, r) for r in rows)
    return out


OPS = ["ISNOTEMPTY", "ISEMPTY", "STARTSWITH", "ENDSWITH", "NOT LIKE", "LIKE", "IN", "!=", ">=", "<=", "=", ">", "<"]


def term_true(table, row, term):
    if term.startswith("123TEXTQUERY321="):
        words = term.split("=", 1)[1].lower().split()
        text = " ".join(str(v) for v in row.values()).lower()
        return all(w in text for w in words)
    m = re.match(r"([a-z0-9_.]+)RELATIVEGT@dayofweek@ago@(\d+)$", term)
    if m:
        return (value(row, m[1]) or "") >= ts(timedelta(days=int(m[2])))
    m = re.match(r"([a-z0-9_.]+)(.*)$", term)
    if not m:
        return True
    field, tail = m[1], m[2]
    v = value(row, field)
    if v is None and "." not in field and not any(field in r for _, r in rows_of(table)[:400]):
        return True  # as the real one: an unknown field is dropped
    v = v or ""
    for op in OPS:
        if tail.startswith(op):
            arg = tail[len(op):].replace("^^", "^")
            lv, la = v.lower(), arg.lower()
            return {"ISNOTEMPTY": v != "", "ISEMPTY": v == "", "STARTSWITH": lv.startswith(la), "ENDSWITH": lv.endswith(la), "NOT LIKE": la not in lv, "LIKE": la in lv, "IN": v in arg.split(","), "!=": v != arg, "=": v == arg, ">=": v >= arg, "<=": v <= arg, ">": v > arg, "<": v < arg}[op]
    return True


def select(table, query):
    terms = [t for t in re.split(r"(?<!\^)\^(?!\^)", query) if t]
    order = []
    groups = [[]]
    for t in terms:
        if t.startswith("ORDERBYDESC"):
            order.append((t[11:], True))
        elif t.startswith("ORDERBY"):
            order.append((t[7:], False))
        elif t.startswith("NQ"):
            groups.append([[t[2:]]])
        elif t.startswith("OR") and groups[-1]:
            groups[-1][-1].append(t[2:])
        else:
            groups[-1].append([t])
    rows = [(t, r) for t, r in rows_of(table) if any(all(any(term_true(table, r, x) for x in alt) for alt in g) for g in groups)]
    for field, down in reversed(order):
        rows.sort(key=lambda tr: value(tr[1], field) or "", reverse=down)
    return rows


def render(table, row, fields, mode):
    out = {}
    for f in fields or list(row):
        raw = value(row, f)
        if raw is None:
            if "." in f:
                raw = ""
            else:
                continue
        text = shown(table, row, f)
        out[f] = {"display_value": text, "value": raw} if mode == "all" else (text if mode == "true" else raw)
    return out


class Handler(BaseHTTPRequestHandler):
    protocol_version = "HTTP/1.1"

    def log_message(self, *args):
        pass

    def send(self, status, body, headers=None, kind="application/json"):
        data = body if isinstance(body, bytes) else json.dumps(body).encode()
        self.send_response(status)
        self.send_header("content-type", kind)
        self.send_header("content-length", str(len(data)))
        for k, v in (headers or {}).items():
            self.send_header(k, v)
        self.end_headers()
        self.wfile.write(data)

    def fail(self, status, message, detail=""):
        self.send(status, {"error": {"message": message, "detail": detail}, "status": "failure"})

    def handle_all(self, method):
        global EXPIRED
        length = int(self.headers.get("content-length") or 0)
        body = self.rfile.read(length)
        url = urlparse(self.path)
        q = {k: v[0] for k, v in parse_qs(url.query).items()}
        parts = [p for p in url.path.split("/") if p]
        if parts[:1] == ["fake"]:
            if parts[1] == "expire":
                EXPIRED = not EXPIRED
            else:
                _, row = find(parts[3])
                row["sys_mod_count"] = str(int(row["sys_mod_count"]) + 1)
                row["sys_updated_by"], row["sys_updated_on"], row["urgency"] = "grace", datetime.now(UTC).strftime("%Y-%m-%d %H:%M:%S"), "3"
            return self.send(200, {})
        time.sleep(ARGS.latency * rng.uniform(0.5, 1.5))
        if EXPIRED or not self.headers.get("authorization"):
            return self.fail(401, "User Not Authenticated", "Required to provide Auth information")
        with LOCK:
            return self.api(method, parts, q, body)

    def api(self, method, parts, q, body):
        path = "/".join(parts)
        if path == "api/now/ui/user/current_user":
            return self.send(200, {"result": {"user_sys_id": ME, "user_name": "ada"}})
        if path.startswith("api/now/ui/meta/"):
            if parts[4] in ARGS.forbid:
                return self.fail(403, "User Not Authorized", "Failed API level ACL Validation")
            columns = meta(parts[4])
            return self.send(200, {"result": {"columns": columns}}) if columns else self.fail(404, "Invalid table")
        if path.startswith("api/now/stats/"):
            if parts[3] in ARGS.forbid:
                return self.fail(403, "User Not Authorized")
            return self.send(200, {"result": {"stats": {"count": str(len(select(parts[3], q.get("sysparm_query", ""))))}}})
        if path == "api/now/attachment":
            rows = [a for a in ATTACHMENTS if all(term_true("x", a, t) for t in q["sysparm_query"].split("^") if not t.startswith("ORDERBY"))]
            return self.send(200, {"result": rows})
        if path.startswith("api/now/attachment/"):
            return self.send(200, b"10:02 tunnel up\n10:07 tunnel down\n", kind="text/plain")
        if path.startswith("api/now/table/"):
            return self.table(method, parts[3], parts[4] if len(parts) > 4 else None, q, body)
        return self.fail(404, "Requested URI does not represent any resource")

    def table(self, method, table, key, q, body):
        if table in ARGS.forbid:
            return self.fail(403, "User Not Authorized", "Failed API level ACL Validation")
        fields = [f for f in q.get("sysparm_fields", "").split(",") if f]
        mode = q.get("sysparm_display_value", "false")
        if not rows_of(table):
            return self.fail(400, f"Invalid table {table}")
        if method == "GET" and not key:
            rows = select(table, q.get("sysparm_query", ""))
            at, limit = int(q.get("sysparm_offset", 0)), int(q.get("sysparm_limit", 10000))
            return self.send(200, {"result": [render(t, r, fields, mode) for t, r in rows[at:at + limit]]}, {"X-Total-Count": str(len(rows))})
        if method == "POST":
            data = json.loads(body)
            number = f"{ {'incident': 'INC', 'problem': 'PRB', 'change_request': 'CHG'}.get(table, 'TSK') }{rng.randint(90000, 99999):07d}"
            new = add(table, number, number=number, active="true", state=(choices(table, "state") or [("1", "")])[0][0], priority="4", **{k: v for k, v in data.items() if k not in ("state",)})
            return self.send(201, {"result": render(table, find(new)[1], fields, mode)})
        t, row = find(key or "")
        if row is None:
            return self.fail(404, "No Record found", "Record doesn't exist or ACL restricts the record retrieval")
        if method == "PATCH":
            for k, v in json.loads(body).items():
                if k in ("comments", "work_notes"):
                    journal(row["sys_id"], k, v, "ada", NOW - datetime.now(UTC).replace(tzinfo=None))
                elif k == "impact":
                    pass  # a business rule keeps it
                else:
                    audit(t, row["sys_id"], k, row.get(k, ""), v, "ada", NOW - datetime.now(UTC).replace(tzinfo=None))
                    row[k] = v
            if row.get("state") in ("6", "7") and t == "incident":
                row["active"] = "false"
            row["sys_mod_count"] = str(int(row["sys_mod_count"]) + 1)
            row["sys_updated_by"], row["sys_updated_on"] = "ada", datetime.now(UTC).strftime("%Y-%m-%d %H:%M:%S")
        return self.send(200, {"result": render(t, row, fields, mode)})

    def do_GET(self):
        self.handle_all("GET")

    def do_POST(self):
        self.handle_all("POST")

    def do_PATCH(self):
        self.handle_all("PATCH")


if __name__ == "__main__":
    parser = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    parser.add_argument("--port", type=int, default=8765)
    parser.add_argument("--latency", type=float, default=0.15, help="seconds each answer takes, about")
    parser.add_argument("--forbid", nargs="*", default=["sys_hub_flow"], help="tables that answer 403")
    ARGS = parser.parse_args()
    ThreadingHTTPServer.request_queue_size = 64
    print(f"fake instance on http://127.0.0.1:{ARGS.port}, user ada, any password", flush=True)
    ThreadingHTTPServer(("127.0.0.1", ARGS.port), Handler).serve_forever()
