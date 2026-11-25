---
name: sn
description: Use the `sn` CLI to read and change ServiceNow data - incidents, changes, requests, catalog orders, approvals, knowledge, CMDB, any table - and to develop on an instance - scripts, flows, update sets, tests, code search, REST APIs. Use for every ServiceNow task.
---

# sn

Always add `--agent` (or set `SN_AGENT=1`). stdout is then one JSON line and nothing asks:

```json
{"ok":true,"command":"table list","data":[...],"meta":{"count":20,"total":431,"next":"add --offset 20"}}
{"ok":false,"command":"table get","error":{"code":"not_found","message":"..."}}
```

Exit codes: 0 ok, 1 API error, 2 sign-in or permission, 3 invalid input, 4 not found, 5 other.

## Errors

- `input_required`: run again with `error.input.arg` set to one of `error.input.choices[].value`.
- `confirmation_required`: the command is destructive. Add `--yes` only when the user asked for that change.
- `auth`: not signed in. Run `sn auth login --instance NAME --agent`, give the user `data.url`, ask for the code or the address of the page the browser ended on, and run `sn auth login --code 'THAT' --agent`. With `--client-id`, `--wait` waits for the browser instead.
- `permission`: signing in again does not help; the user needs a role or ACL.
- `read_only`: the profile refuses writes. Do not work around it.

## Read

```sh
sn get INC0010001 --agent                 # record, catalog variables, comments and work notes
sn get mine --agent                       # open work of the user
sn table list incident -q 'active=true^priority=1' -f number,short_description,assigned_to -n 20 --agent
sn table get incident:INC0010001 -f number,state --display-value all --agent
sn table count incident -q active=true --agent
sn aggregate incident --group-by state --agent
sn gr incident -f number,caller_id.manager.email -q active=true --agent
sn journal INC0010001 --limit 5 --agent
```

- A record is a number (`INC0010001`), `table:number`, `table:sys_id`, or a link.
- Values are labels. `--display-value false` gives raw values, which queries and writes need.
- Keep answers small: `-f` fields, `-n` limit, `--select a,b`, `--jq '.[].number'`.
- Before a query or write on a table you do not know: `sn schema columns TABLE --agent`, `sn choices TABLE FIELD --agent`. The instance silently ignores query terms with wrong field names.

## Write

```sh
sn incident create --short-description "VPN down" --impact 2 --urgency 2 --agent
sn incident note INC0010001 "Checked the gateway" --agent
sn incident assign INC0010001 --to ada@example.com --group Network --agent
sn incident resolve INC0010001 --code "Solution provided" --notes "Restarted" --agent
sn table update incident:INC0010001 -F state=2 -F work_notes="..." --agent
sn approval list --agent; sn approval approve SYS_ID --agent
sn change create --type normal -F short_description="Patch" --agent
sn catalog search laptop --agent; sn catalog show ITEM --agent
sn catalog order ITEM --var name=value --dry-run --agent    # then the same with --yes
```

- `--dry-run` shows the request instead of sending it.
- Bodies: `-F name=value` (repeatable) or `-D '{...}'`, `-D @file.json`, `-D -` for stdin.
- Never repeat a catalog order after a lost answer: check `catalog requests mine` first.

## Develop

```sh
sn script run 'gs.info(gs.getUserName())' --yes --agent     # server-side JavaScript
sn codesearch MyUtil --agent
sn get script MyUtil --source --agent
sn get updateset --agent; sn set updateset "Sprint 12" --agent
sn flow runs FLOW --errors --agent; sn flow debug CONTEXT --agent; sn flow why-not FLOW --record INC0010001 --agent
sn cicd atf run --suite-name Regression --wait --agent
sn impersonate abel.tuter --agent -- table list incident
sn doctor --need-role itil --agent
sn watch incident -q priority=1 --duration 60 --agent        # one JSON line for each event
```

## Any API

```sh
sn api search "change conflict" --agent                      # documented operations, offline
sn api describe change-management list-conflict --agent
sn api change-management list-conflict --sys-id SYS_ID --agent
sn apisearch attachment --agent                              # what this instance publishes
sn raw GET /api/now/table/incident -q sysparm_limit=1 --agent
sn batch "GET /api/now/table/incident?sysparm_limit=1" "GET /api/now/table/problem?sysparm_limit=1" --agent
```

`sn introspect --brief --agent` lists every command with its effect (read, write, confirm); `sn introspect incident resolve --agent` gives the arguments and examples of one, and `sn COMMAND --help` the same as text.
