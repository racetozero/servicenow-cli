# sn

**ServiceNow from your terminal, for you and your AI agent.**

`sn` is one fast binary for ServiceNow. Look up incidents, approve changes, order from the catalog, debug flows, run scripts, or call any REST API. It signs in through your company's SSO in your normal browser, and every command has a JSON mode that never stops to ask a question.

```sh
sn incident mine
```

## Install

macOS, Linux and WSL:

```sh
curl -LsSf https://raw.githubusercontent.com/racetozero/servicenow-cli/main/install.sh | sh
```

Windows, including ARM64:

```powershell
powershell -ExecutionPolicy ByPass -c "irm https://raw.githubusercontent.com/racetozero/servicenow-cli/main/install.ps1 | iex"
```

Homebrew:

```sh
brew install racetozero/tap/sn
```

The installers verify the SHA-256 checksum. `sn update` installs the newest release (with Homebrew: `brew upgrade racetozero/tap/sn`).

## Sign in

```sh
sn auth login --instance acme
```

Your **default browser** opens, so SSO, device sign-in and browser extensions work. There is no private window and no separate browser profile. Without a browser (SSH, containers, WSL without interop), open the printed link anywhere and paste the code, or the address of the last page.

Passwords with MFA, OAuth client credentials and JWT, API keys, bearer tokens and token commands, session cookies and client certificates work too. Certificates are checked with the certificate store of your operating system. See [Sign in](docs/authentication.md).

## Use

```sh
sn get INC0010001                              # record, variables, conversation
sn table list incident -q active=true^priority=1
sn incident resolve INC0010001 --code "Solution provided" --notes "Restarted the gateway"
sn approval approve SYS_ID --comment "Looks good"
sn catalog order "Standard Laptop" --var color=black
sn watch incident -q priority=1                # live changes
sn flow why-not "Assign VIP" --record INC0010001
sn api search attachment                       # 712 documented operations, offline
sn tui                                         # the full-screen workspace
```

| Area | Commands |
| --- | --- |
| Records | `get`, `table`, `gr`, `aggregate`, `graphql`, `journal`, `variables`, `attachment`, `watch`, `open`, `xml`, `import`, `resolve` |
| Service management | `incident`, `change`, `approval`, `catalog`, `knowledge`, `cmdb`, `identify`, `scores` |
| Development | `schema`, `choices`, `cache`, `set`, `flow`, `decision`, `playbook`, `script`, `codesearch`, `appmanager`, `cicd`, `sdk`, `impersonate` |
| Any API | `api`, `apisearch`, `raw`, `batch` |
| Setup | `auth`, `profile`, `config`, `ping`, `doctor`, `update`, `alias`, `completions`, `skill`, `introspect` |

A record can be a number, `table:number`, `table:sys_id`, or a link that you copied from the browser. People and groups can be names. Output is a table in a terminal and JSON in a pipe; `-o csv`, `-o yaml`, `--select` and `--jq` shape it.

`sn sdk ...` runs the official ServiceNow SDK with the same sign-in.

## Workspace

`sn tui` opens a full-screen workspace: your queues on the left, the list in the middle, and the record with its conversation, related records, files and SLAs. Every change is shown before it is sent. Press `?` for the keys.

## Agents

Add `--agent` to any command, or set `SN_AGENT=1`: one JSON line on stdout, errors with codes and valid choices, no prompts, and destructive commands wait for `--yes`.

```sh
npx skills add racetozero/servicenow-cli     # or: bunx skills add racetozero/servicenow-cli
sn skill --install                           # writes .claude/skills/sn/SKILL.md
```

See [Agent mode](docs/agent-mode.md).

## Shell completion

```sh
eval "$(sn completions zsh)"     # bash, zsh, fish, powershell, elvish
sn cache refresh                 # completes table and column names too
```

## License

MIT. The API catalog is derived from ServiceNow's documentation (Apache-2.0); see [NOTICE](NOTICE). This project is not affiliated with ServiceNow.
