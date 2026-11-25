# ServiceNow CLI - `sn`

[![CI](https://github.com/racetozero/servicenow-cli/actions/workflows/ci.yml/badge.svg)](https://github.com/racetozero/servicenow-cli/actions/workflows/ci.yml)
[![Release](https://img.shields.io/github/v/release/racetozero/servicenow-cli?sort=semver)](https://github.com/racetozero/servicenow-cli/releases)
[![Rust](https://img.shields.io/badge/Rust-stable-orange?logo=rust)](https://github.com/racetozero/servicenow-cli/blob/main/rust-toolchain.toml)
[![Downloads](https://img.shields.io/github/downloads/racetozero/servicenow-cli/total)](https://github.com/racetozero/servicenow-cli/releases)
[![Homebrew](https://img.shields.io/badge/homebrew-racetozero%2Ftap%2Fsn-fbb040?logo=homebrew)](https://github.com/racetozero/homebrew-tap)
[![License: MIT](https://img.shields.io/badge/license-MIT-blue)](LICENSE)

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

## Performance

`sn` adds little time to a request. Each time below is one `sn` process, from start to exit, against a mock instance on the same machine that answers at once, so the table shows the cost of `sn` and not of the network.

| Command | p50 | p99 | Peak memory |
| --- | ---: | ---: | ---: |
| `sn --version` | 4 ms | 6 ms | 11.9 MiB |
| `sn --help` | 6 ms | 14 ms | 13.7 MiB |
| `sn api search attachment` | 7 ms | 11 ms | 18.2 MiB |
| `sn api describe attachment create-file` | 6 ms | 8 ms | 18.1 MiB |
| `sn table get task SYS_ID` | 8 ms | 19 ms | 16.0 MiB |
| `sn table list change_request -n 300 -o table` | 9 ms | 24 ms | 16.2 MiB |
| `sn table list problem -n 1000 --agent` | 11 ms | 17 ms | 17.0 MiB |

Measured on an Apple M4 (10 cores) with macOS 27 and the `dist` release build: 300 runs of each local command and 200 of each command that calls the instance, after 10 warmups. Peak memory is the median peak resident memory.

### Profile-guided release builds

Release binaries are built with profile-guided optimization (PGO). The release workflow trains a profile on deterministic offline workloads, then measures the result on separate held-out commands (`just pgo-bench`).

| Measure | Standard build | Optimized build | Change |
| --- | ---: | ---: | ---: |
| `sn cicd updateset --help` | 6.46 ms | 6.43 ms | 0.39% faster |
| `sn api describe` | 9.32 ms | 8.77 ms | 5.88% faster |
| `sn table list problem -n 1000` | 14.10 ms | 13.83 ms | 1.88% faster |
| `sn table list change_request -o table` | 8.92 ms | 9.00 ms | 1.00% slower |
| `sn table get task` | 8.35 ms | 8.06 ms | 3.53% faster |
| Geometric mean latency | 1.000x | 0.978x | 2.17% faster |
| Executable size | 14.28 MiB | 13.39 MiB | 6.27% smaller |
| gzip size | 6.43 MiB | 6.09 MiB | 5.20% smaller |

## License

MIT
