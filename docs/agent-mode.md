# Agent mode

Add `--agent` to any command, or set `SN_AGENT=1`. The skill [`sn`](../.agents/skills/sn/SKILL.md) teaches an agent the commands:

```sh
npx skills add racetozero/servicenow-cli       # or bunx; pick the agents
sn skill --install                     # to .claude/skills/
sn skill --install .agents/skills      # or another directory
sn skill                               # print it
```

- stdout is exactly one JSON line, on success and on failure. Streams (`watch`, `flow tail`) print one line for each event.

  ```json
  {"ok":true,"command":"table list","data":[...],"meta":{"count":20,"total":431,"next_offset":20,"next":"add --offset 20"}}
  {"ok":false,"command":"table get","error":{"code":"not_found","message":"..."}}
  ```

- Nothing asks, nothing opens a browser, and the full-screen workspace does not start. `--help` still prints text.
- stdin is read only when you pass `-`, `@-` or `--secret-stdin`.
- A missing or ambiguous value is `input_required`, with the values that fit:

  ```json
  {"code":"input_required","message":"...","input":{"arg":"profile","choices":[{"value":"prod","label":"prod  https://acme.service-now.com"}]}}
  ```

- A destructive command is `confirmation_required` until it has `--yes`.
- `--dry-run` shows the write request instead of sending it.
- `--select number,state` and `--jq '.[].number'` make answers small.
- A read-only profile (`sn profile set NAME --profile-read-only true`) or `--read-only` refuses every write.

| Exit | Error codes |
| ---: | --- |
| 0 | |
| 1 | `api`, `conflict`, `rate_limited`, `not_ordered`, `check_failed`, `script_error` |
| 2 | `auth`, `permission` |
| 3 | `invalid`, `input_required`, `confirmation_required`, `read_only` |
| 4 | `not_found` |
| 5 | `network`, `timeout`, `internal` |
| 130 | interrupted |

`sn introspect` prints the whole command tree as JSON, `sn introspect table list` one command with its arguments, examples and effect, `sn introspect --brief` the names. The effect is `read`, `write`, `confirm` (needs `--yes`), `local`, `confirm-local` or `depends`; `--help` says the same in words. `sn api search TEXT` searches the documented REST operations offline, and `sn api describe API OPERATION` shows the parameters.
