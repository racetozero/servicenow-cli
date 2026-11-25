# Sign in

```sh
sn auth login --instance acme
```

On a terminal this asks how you sign in. The first choice, the browser, is the right one for most people. A profile keeps the instance and the sign-in; use `--profile NAME` (`-p`) for more than one.

## Browser sign-in (SSO)

`sn` opens your **default browser**, in a normal window with your own profile. Company sign-in pages, device SSO and browser extensions (for example Windows SSO for Chrome and Edge) work as usual. It never starts a private window, a separate browser profile, or a browser that it controls.

The link is always printed too:

| Where you are | What happens |
| --- | --- |
| Windows | The default browser opens. |
| macOS | The default browser opens. |
| WSL | The default browser of Windows opens. If Windows interop is off, use the printed link. |
| Linux with a desktop | The default browser opens through `xdg-open`. |
| SSH, containers, no desktop | Nothing opens. Open the printed link in a browser on any computer. |

`--no-browser` prints the link only. `SN_BROWSER` (or `BROWSER`) chooses a program, and `none` turns the browser off.

This is OAuth 2.0 authorization code with PKCE. ServiceNow only sends a browser back to addresses that its OAuth client lists, so there are two ways to finish:

**With an OAuth client** (recommended for a company). An administrator creates it once:

```sh
sn auth register          # role oauth_admin or admin; --print shows the steps instead
```

If the command does not work on your release, `--print` shows the form to fill in by hand. Everyone then signs in without copying anything:

```sh
sn auth login --instance acme --client-id CLIENT_ID
```

The browser returns to `http://localhost:8400/callback`, and the command continues by itself. Use `--port N` for another port that the client lists, or `--redirect-uri URL` for a different address. ServiceNow compares the address exactly, so a random port only works with clients that allow it (`--port 0`).

If the browser runs on another computer, the last page cannot load. Copy its address from the address bar and paste it into the terminal.

**Without a client**, the client that instances ship for the ServiceNow SDK is used. It needs no setup. At the end the instance shows a code; paste it into the terminal.

### Agents and scripts

Without a terminal the command does not wait. It prints the link and stops, and a second command finishes:

```sh
sn auth login --instance acme --agent
sn auth login --code 'CODE or the address of the last page' --agent
```

With an OAuth client, `--wait` waits for the browser instead.

## Other methods

| Method | Command |
| --- | --- |
| User name and password | `--method basic --username ada --secret-stdin < password.txt` |
| User name, password and MFA code | `--method session --username ada --mfa-code 123456 --secret-stdin` |
| OAuth client credentials | `--method client-credentials --client-id ID --secret-stdin` |
| OAuth JWT bearer | `--method jwt --client-id ID --jwt-key-file key.pem --jwt-subject USER [--jwt-kid KID]` |
| OAuth password grant | `--method password --client-id ID --client-secret-file FILE --username ada --secret-stdin` |
| API key | `--method api-key --secret-stdin` |
| Bearer token | `--method token --secret-stdin`, or `--token-command 'az account get-access-token -o json'` |
| Copied browser session | `--method cookie --user-token G_CK --secret-stdin < cookie.txt` |
| TLS client certificate | `--method certificate --client-cert cert-and-key.pem` (the flag also combines with the other methods) |

`--secret-stdin` reads the secret from stdin, so it is never on a command line. On a terminal you are asked instead. A sign-in is checked before it is saved; `--no-verify` skips the check.

## Environment variables

For CI and containers, without a configuration file:

| Variable | Meaning |
| --- | --- |
| `SN_INSTANCE` | Instance name, host or URL |
| `SN_USERNAME`, `SN_PASSWORD` | Basic sign-in |
| `SN_TOKEN` | Bearer token |
| `SN_API_KEY` | API key |
| `SN_CLIENT_ID`, `SN_CLIENT_SECRET` | Client credentials; with user name and password, the password grant |
| `SN_COOKIE`, `SN_USER_TOKEN` | Copied browser session |
| `SN_PROFILE` | Profile to use |
| `SN_READ_ONLY` | Refuse writes |
| `SN_PROXY`, `SN_NO_PROXY`, `SN_CA_CERT`, `SN_CLIENT_CERT`, `SN_INSECURE` | Network |
| `SN_CONFIG_DIR`, `SN_CACHE_DIR` | Where files go |

A stored credential is only ever sent to its own instance.

## Certificates and proxies

Server certificates are checked with the certificate store of the operating system, so company roots that your IT department installs just work. `--ca-cert FILE` trusts more roots. `--proxy URL` (`http`, `https`, `socks5`) or the system proxy is used; `--no-proxy` ignores both.

## Where secrets are

`config.toml` has no secrets. Secrets are in `credentials.toml` next to it, a file that only you can read, or in the Windows Credential Manager on Windows. `--store keychain` uses the macOS keychain or the Linux Secret Service, `--store file` the file. `sn config path` shows the place.

```sh
sn auth status      # who am I, when does the token expire
sn auth refresh     # new access token
sn auth token --as bearer
sn auth logout      # removes the credential, revokes the token
sn profile list
sn profile set prod --profile-read-only true
```
