//! Opens a link in the person's own default browser: never a private window
//! and never a separate profile, so single sign-on sessions and browser
//! extensions work.

use std::ffi::OsString;
use std::process::{Command, Stdio};

/// One way to open a link: the program and its arguments.
#[derive(Debug, PartialEq, Eq)]
struct Opener {
    program: OsString,
    args: Vec<OsString>,
}

impl Opener {
    fn new(program: &str, args: &[&str]) -> Self {
        Self {
            program: program.into(),
            args: args.iter().map(OsString::from).collect(),
        }
    }
}

/// What decides how to open a link.
struct Environment {
    os: &'static str,
    wsl: bool,
    /// A desktop (X11 or Wayland) is present.
    display: bool,
    /// A remote shell: a browser would open on the wrong computer.
    ssh: bool,
    /// `SN_BROWSER` or `BROWSER`.
    custom: Option<String>,
}

impl Environment {
    fn detect() -> Self {
        let set = |name: &str| std::env::var_os(name).is_some_and(|v| !v.is_empty());
        let wsl = cfg!(target_os = "linux")
            && (set("WSL_DISTRO_NAME")
                || set("WSL_INTEROP")
                || std::fs::read_to_string("/proc/sys/kernel/osrelease")
                    .is_ok_and(|v| v.to_ascii_lowercase().contains("microsoft")));
        Self {
            os: std::env::consts::OS,
            wsl,
            display: set("DISPLAY") || set("WAYLAND_DISPLAY"),
            ssh: set("SSH_CONNECTION") || set("SSH_TTY"),
            custom: ["SN_BROWSER", "BROWSER"]
                .iter()
                .find_map(|n| std::env::var(n).ok())
                .filter(|v| !v.trim().is_empty()),
        }
    }
}

/// The openers to try, best first. Empty in a headless session.
fn openers(env: &Environment, url: &str) -> Vec<Opener> {
    if let Some(custom) = &env.custom {
        if custom.trim() == "none" {
            return Vec::new();
        }
        let mut words = custom.split_whitespace();
        let program = words.next().unwrap_or_default();
        let mut args: Vec<&str> = words.collect();
        args.push(url);
        return vec![Opener::new(program, &args)];
    }
    if env.ssh {
        return Vec::new();
    }
    // Asks Windows for the default browser and, unlike `cmd /c start`, takes
    // the link as it is (`&` included).
    let windows = |program: &str| Opener::new(program, &["url.dll,FileProtocolHandler", url]);
    match env.os {
        "macos" => vec![Opener::new("/usr/bin/open", &[url])],
        "windows" => vec![windows("rundll32.exe")],
        _ if env.wsl => {
            let mut list = vec![
                Opener::new("wslview", &[url]),
                windows("rundll32.exe"),
                windows("/mnt/c/Windows/System32/rundll32.exe"),
            ];
            if env.display {
                list.push(Opener::new("xdg-open", &[url]));
            }
            list
        }
        _ if env.display => vec![
            Opener::new("xdg-open", &[url]),
            Opener::new("gio", &["open", url]),
            Opener::new("sensible-browser", &[url]),
        ],
        // Without a desktop, xdg-open would start a text browser in this
        // terminal.
        _ => Vec::new(),
    }
}

/// Opens the link and tells whether a browser took it.
pub fn open(url: &str) -> bool {
    openers(&Environment::detect(), url).iter().any(|opener| {
        Command::new(&opener.program)
            .args(&opener.args)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .is_ok_and(|mut child| {
                // Openers hand over and exit; a program that fails at once
                // did not open anything. A browser itself keeps running.
                for _ in 0..20 {
                    match child.try_wait() {
                        Ok(Some(status)) => return status.success() || is_rundll(opener),
                        Ok(None) => std::thread::sleep(std::time::Duration::from_millis(50)),
                        Err(_) => return false,
                    }
                }
                true
            })
    })
}

/// rundll32 gives no meaningful exit code.
fn is_rundll(opener: &Opener) -> bool {
    opener.program.to_string_lossy().ends_with("rundll32.exe")
}

#[cfg(test)]
mod tests {
    use super::*;

    const URL: &str = "https://acme.service-now.com/oauth_auth.do?a=1&b=2";

    fn env(os: &'static str) -> Environment {
        Environment {
            os,
            wsl: false,
            display: false,
            ssh: false,
            custom: None,
        }
    }

    fn programs(env: &Environment) -> Vec<String> {
        openers(env, URL)
            .iter()
            .map(|o| o.program.to_string_lossy().into_owned())
            .collect()
    }

    #[test]
    fn every_system_uses_its_default_browser_with_no_flags() {
        assert_eq!(programs(&env("macos")), ["/usr/bin/open"]);
        assert_eq!(programs(&env("windows")), ["rundll32.exe"]);
        assert_eq!(
            programs(&Environment {
                wsl: true,
                ..env("linux")
            })[0],
            "wslview"
        );
        assert_eq!(
            programs(&Environment {
                display: true,
                ..env("linux")
            })[0],
            "xdg-open"
        );
        for environment in [
            env("macos"),
            env("windows"),
            Environment {
                wsl: true,
                ..env("linux")
            },
        ] {
            for opener in openers(&environment, URL) {
                let args: Vec<String> = opener
                    .args
                    .iter()
                    .map(|a| a.to_string_lossy().into_owned())
                    .collect();
                assert!(
                    args.iter()
                        .all(|a| !a.contains("incognito") && !a.contains("private")),
                    "{args:?}"
                );
                assert_eq!(
                    args.last().map(String::as_str),
                    Some(URL),
                    "the link is one unchanged argument"
                );
            }
        }
    }

    #[test]
    fn headless_sessions_open_nothing() {
        assert!(programs(&env("linux")).is_empty());
        assert!(
            programs(&Environment {
                ssh: true,
                display: true,
                ..env("linux")
            })
            .is_empty()
        );
        assert!(
            programs(&Environment {
                custom: Some("none".into()),
                ..env("macos")
            })
            .is_empty()
        );
    }

    #[test]
    fn a_chosen_browser_wins() {
        let e = Environment {
            custom: Some("firefox --new-tab".into()),
            ssh: true,
            ..env("linux")
        };
        assert_eq!(
            openers(&e, URL),
            [Opener::new("firefox", &["--new-tab", URL])]
        );
    }
}
