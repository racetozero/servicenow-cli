//! The full-screen workspace of the sn CLI.
//!
//! `app` is a state machine: a message changes the state and asks for
//! effects. `data` runs the effects that need an instance, behind a trait.
//! `ui` draws the state. This file owns the terminal and connects the three.
#![forbid(unsafe_code)]

mod app;
mod data;
mod input;
mod keys;
mod model;
mod msg;
mod query;
mod tables;
mod text;
mod theme;
mod ui;

use app::App;
use app::state::{Connection, ProfileInfo, Settings, Start};
use base64::Engine;
use data::{Live, Runner};
use msg::{Effect, Msg};
use ratatui::Terminal;
use ratatui::backend::CrosstermBackend;
use ratatui::crossterm::event::{
    self, DisableBracketedPaste, DisableMouseCapture, EnableBracketedPaste, EnableMouseCapture,
    Event,
};
use ratatui::crossterm::execute;
use ratatui::crossterm::terminal::{
    EnterAlternateScreen, LeaveAlternateScreen, disable_raw_mode, enable_raw_mode,
};
use sn_core::{Client, Error, Result};
use std::io::{IsTerminal, Stdout, Write};
use std::path::PathBuf;
use std::process::{Command, Stdio};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};
use tokio::sync::mpsc::{UnboundedReceiver, UnboundedSender, unbounded_channel};

const FRAME: Duration = Duration::from_millis(100);

/// A saved profile, for the profile switcher.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Profile {
    pub name: String,
    /// The instance's base URL.
    pub instance: String,
    pub read_only: bool,
}

/// Who is connected to what.
pub struct Session {
    profile: String,
    instance: String,
    client: std::result::Result<Arc<Client>, String>,
    profiles: Vec<Profile>,
    notice: Option<String>,
}

impl Session {
    pub fn connected(client: Client, profiles: Vec<Profile>) -> Self {
        Self {
            profile: client.profile_name().to_owned(),
            instance: client.base().to_owned(),
            client: Ok(Arc::new(client)),
            profiles,
            notice: None,
        }
    }

    /// No usable credential: the workspace offers to sign in. `instance` is
    /// empty before the first sign-in.
    pub fn signed_out(
        profile: impl Into<String>,
        instance: impl Into<String>,
        reason: impl Into<String>,
        profiles: Vec<Profile>,
    ) -> Self {
        Self {
            profile: profile.into(),
            instance: instance.into(),
            client: Err(reason.into()),
            profiles,
            notice: None,
        }
    }

    /// Something to tell the person at the start.
    pub fn with_notice(mut self, text: impl Into<String>) -> Self {
        self.notice = Some(text.into());
        self
    }

    fn connection(&self) -> Connection {
        let host = |url: &str| {
            url.split("://")
                .nth(1)
                .unwrap_or(url)
                .trim_end_matches('/')
                .to_owned()
        };
        Connection {
            profile: self.profile.clone(),
            base: self.instance.clone(),
            host: host(&self.instance),
            read_only: self.client.as_ref().is_ok_and(|c| c.is_read_only()),
            dry_run: self.client.as_ref().is_ok_and(|c| c.is_dry_run()),
            signed_out: self.client.as_ref().err().cloned(),
        }
    }

    fn profiles(&self) -> Vec<ProfileInfo> {
        self.profiles
            .iter()
            .map(|p| ProfileInfo {
                name: p.name.clone(),
                host: p
                    .instance
                    .split("://")
                    .nth(1)
                    .unwrap_or(&p.instance)
                    .to_owned(),
                read_only: p.read_only,
            })
            .collect()
    }
}

/// How the workspace starts.
#[derive(Debug)]
pub struct Options {
    /// Start on this table instead of the person's work.
    pub table: Option<String>,
    /// Encoded query for the table.
    pub query: Option<String>,
    /// A view of the table: it decides the columns.
    pub view: Option<String>,
    pub page_size: u64,
    pub no_color: bool,
    /// Where downloads go.
    pub download_dir: PathBuf,
    /// Tables kept in the sections.
    pub pins: Vec<String>,
    /// What an earlier [`Exit`] gave back.
    pub resume: Option<Resume>,
}

impl Default for Options {
    fn default() -> Self {
        Self {
            table: None,
            query: None,
            view: None,
            page_size: 50,
            no_color: false,
            download_dir: PathBuf::from("."),
            pins: Vec::new(),
            resume: None,
        }
    }
}

/// Opens a link in a browser and tells whether one took it.
pub type OpenUrl = dyn Fn(&str) -> bool + Send + Sync;
/// Keeps the pinned tables.
pub type SavePins = dyn Fn(&[String]) -> Result<()> + Send + Sync;

/// What the workspace needs from the program around it.
pub struct Hooks {
    pub open_url: Arc<OpenUrl>,
    pub save_pins: Arc<SavePins>,
}

/// The state of a workspace that was left, with everything typed in it.
pub struct Resume {
    app: Box<App>,
    switch: bool,
}

impl std::fmt::Debug for Resume {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("Resume")
    }
}

/// Why the workspace ended.
#[derive(Debug)]
pub enum Exit {
    Quit,
    /// Sign in outside the full screen, then run again with the place.
    SignIn(Resume),
    /// Run again with this profile.
    SwitchProfile(String, Resume),
}

/// Runs the workspace until the person leaves it. The terminal is restored
/// however this ends.
pub async fn run(session: Session, options: Options, hooks: Hooks) -> Result<Exit> {
    let mut app = build(&session, options);
    let mut screen = Screen::enter()?;
    let exit = event_loop(&mut screen, &mut app, &session, &hooks).await;
    drop(screen);
    let exit = exit?;
    if let Ok(client) = &session.client {
        let finished = client.finish().await;
        // Going on with another session, the typed work is worth more than a
        // cookie that was not saved.
        if exit == Effect::Quit {
            finished?;
        }
    }
    Ok(match exit {
        Effect::SignIn => Exit::SignIn(Resume {
            app: Box::new(app),
            switch: false,
        }),
        Effect::SwitchProfile(name) => Exit::SwitchProfile(
            name,
            Resume {
                app: Box::new(app),
                switch: true,
            },
        ),
        _ => Exit::Quit,
    })
}

fn build(session: &Session, options: Options) -> App {
    let (mut connection, profiles) = (session.connection(), session.profiles());
    if let Some(resume) = options.resume {
        let mut app = *resume.app;
        // A profile that cannot be read does not say its instance.
        if connection.base.is_empty() && !resume.switch {
            connection.base.clone_from(&app.connection.base);
            connection.host.clone_from(&app.connection.host);
        }
        // Another profile can mean another instance, where no record of the
        // old one exists.
        if resume.switch || app.connection.base != connection.base {
            return app.switch(connection, profiles);
        }
        app.resume(connection, profiles);
        return app;
    }
    let settings = Settings {
        page_size: options.page_size,
        download_dir: options.download_dir,
        pins: options.pins,
        start: options.table.map(|table| Start {
            table,
            query: options.query,
            view: options.view,
        }),
    };
    App::new(
        connection,
        profiles,
        settings,
        theme::Caps::detect(options.no_color),
    )
}

async fn event_loop(
    screen: &mut Screen,
    app: &mut App,
    session: &Session,
    hooks: &Hooks,
) -> Result<Effect> {
    let (reply_tx, mut replies) = unbounded_channel();
    let (note_tx, mut notes) = unbounded_channel::<Msg>();
    let mut runner = session
        .client
        .as_ref()
        .ok()
        .map(|client| Runner::new(Arc::new(Live::new(client.clone())), reply_tx));
    let mut input = Input::start();
    let started = Instant::now();
    let base = app.ms;
    let tick = || Msg::Tick {
        now: sn_core::now_unix().cast_signed(),
        ms: base + u64::try_from(started.elapsed().as_millis()).unwrap_or(u64::MAX),
    };
    let mut ticker = tokio::time::interval(FRAME);
    ticker.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
    let mut stop = std::pin::pin!(terminated());
    let size = screen.terminal.size().map_err(terminal_error)?;
    let mut effects = app.update(tick());
    effects.extend(app.update(Msg::Resize(size.width, size.height)));
    effects.extend(app.start());
    if let Some(notice) = &session.notice {
        app.toast(app::state::ToastKind::Error, notice.clone());
    }
    loop {
        for effect in std::mem::take(&mut effects) {
            match effect {
                Effect::Call(call) => {
                    if let Some(runner) = &mut runner {
                        runner.start(call);
                    }
                }
                Effect::Open(url) => {
                    let (open, note) = (hooks.open_url.clone(), note_tx.clone());
                    tokio::task::spawn_blocking(move || {
                        let opened = open(&url);
                        let _ = note.send(Msg::Opened(url, opened));
                    });
                }
                Effect::Copy(text) => copy(text, &mut screen.terminal),
                Effect::SavePins(pins) => {
                    if let Err(err) = (hooks.save_pins)(&pins) {
                        app.toast(
                            app::state::ToastKind::Error,
                            format!("The pinned tables are not saved: {err}"),
                        );
                    }
                }
                last @ (Effect::Quit | Effect::SignIn | Effect::SwitchProfile(_)) => {
                    return Ok(last);
                }
            }
        }
        if app.dirty {
            app.dirty = false;
            let mut places = None;
            screen
                .terminal
                .draw(|frame| places = Some(ui::draw(frame, app)))
                .map_err(terminal_error)?;
            if let Some(places) = places {
                effects.extend(app.update(Msg::Drawn(places)));
            }
        }
        let first = tokio::select! {
            () = &mut stop => return Ok(Effect::Quit),
            event = input.events.recv() => match event {
                Some(event) => event,
                None => return Err(Error::Internal("the terminal stopped sending input".into())),
            },
            Some(reply) = replies.recv() => Msg::Reply(reply),
            Some(note) = notes.recv() => note,
            _ = ticker.tick() => tick(),
        };
        effects.extend(app.update(first));
        // What waits already is drawn in one frame: a held key or a pasted
        // text does not queue up.
        while let Ok(event) = input.events.try_recv() {
            effects.extend(app.update(event));
        }
        while let Ok(reply) = replies.try_recv() {
            effects.extend(app.update(Msg::Reply(reply)));
        }
    }
}

/// Completes when the system asks the program to end.
async fn terminated() {
    #[cfg(unix)]
    {
        use tokio::signal::unix::{SignalKind, signal};
        if let (Ok(mut term), Ok(mut hangup)) = (
            signal(SignalKind::terminate()),
            signal(SignalKind::hangup()),
        ) {
            tokio::select! {
                _ = term.recv() => {}
                _ = hangup.recv() => {}
            }
            return;
        }
    }
    std::future::pending::<()>().await;
}

fn terminal_error(err: std::io::Error) -> Error {
    Error::Internal(format!(
        "cannot draw on the terminal ({err}); try another terminal program"
    ))
}

/// The terminal in full-screen mode, put back when dropped.
struct Screen {
    terminal: Terminal<CrosstermBackend<Stdout>>,
}

impl Screen {
    fn enter() -> Result<Self> {
        // A panic prints its message on the normal screen. Only the thread
        // that draws counts: a panic in a background task leaves the
        // workspace running, so it must not touch the terminal.
        let drawing = std::thread::current().id();
        let before = std::panic::take_hook();
        std::panic::set_hook(Box::new(move |info| {
            if std::thread::current().id() == drawing {
                restore();
                before(info);
            } else if !std::io::stdout().is_terminal() || !ACTIVE.load(Ordering::SeqCst) {
                before(info);
            }
        }));
        enable_raw_mode().map_err(terminal_error)?;
        ACTIVE.store(true, Ordering::SeqCst);
        let mut out = std::io::stdout();
        let entered = execute!(
            out,
            EnterAlternateScreen,
            EnableMouseCapture,
            EnableBracketedPaste
        )
        .and_then(|()| Terminal::new(CrosstermBackend::new(out)))
        .and_then(|mut terminal| terminal.clear().map(|()| terminal));
        match entered {
            Ok(terminal) => Ok(Self { terminal }),
            Err(err) => {
                restore();
                Err(terminal_error(err))
            }
        }
    }
}

impl Drop for Screen {
    fn drop(&mut self) {
        restore();
    }
}

static ACTIVE: AtomicBool = AtomicBool::new(false);

fn restore() {
    if !ACTIVE.swap(false, Ordering::SeqCst) {
        return;
    }
    // Nothing can be done when the terminal is gone.
    let _ = disable_raw_mode();
    let _ = execute!(
        std::io::stdout(),
        DisableBracketedPaste,
        DisableMouseCapture,
        LeaveAlternateScreen,
        ratatui::crossterm::cursor::Show
    );
}

/// Reads the keyboard on its own thread, and stops before the program asks
/// something on the normal screen.
struct Input {
    events: UnboundedReceiver<Msg>,
    stop: Arc<AtomicBool>,
    thread: Option<std::thread::JoinHandle<()>>,
}

impl Input {
    fn start() -> Self {
        let (tx, events) = unbounded_channel();
        let stop = Arc::new(AtomicBool::new(false));
        let flag = stop.clone();
        let thread = std::thread::spawn(move || read_input(&tx, &flag));
        Self {
            events,
            stop,
            thread: Some(thread),
        }
    }
}

fn read_input(tx: &UnboundedSender<Msg>, stop: &AtomicBool) {
    while !stop.load(Ordering::SeqCst) {
        match event::poll(Duration::from_millis(50)) {
            Ok(false) => continue,
            Ok(true) => {}
            Err(_) => return,
        }
        let msg = match event::read() {
            Ok(Event::Key(key)) => Msg::Key(key),
            Ok(Event::Paste(text)) => Msg::Paste(text),
            Ok(Event::Mouse(mouse)) => Msg::Mouse(mouse),
            Ok(Event::Resize(width, height)) => Msg::Resize(width, height),
            Ok(_) => continue,
            Err(_) => return,
        };
        if tx.send(msg).is_err() {
            return;
        }
    }
}

impl Drop for Input {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::SeqCst);
        let Some(thread) = self.thread.take() else {
            return;
        };
        // The thread looks at the flag every 50 ms. When the terminal window
        // was closed, its read never comes back, and then the program must
        // end without it.
        let patience = Instant::now() + Duration::from_millis(500);
        while !thread.is_finished() {
            if Instant::now() > patience {
                return;
            }
            std::thread::sleep(Duration::from_millis(5));
        }
        let _ = thread.join();
    }
}

/// Puts text on the clipboard: with the system's own tool, or through the
/// terminal, which also works over SSH.
fn copy(text: String, terminal: &mut Terminal<CrosstermBackend<Stdout>>) {
    let set = |name: &str| std::env::var_os(name).is_some_and(|v| !v.is_empty());
    let remote = set("SSH_CONNECTION") || set("SSH_TTY");
    let tool: Option<(&str, &[&str])> = match std::env::consts::OS {
        _ if remote => None,
        "macos" => Some(("/usr/bin/pbcopy", &[])),
        "windows" => Some(("clip.exe", &[])),
        _ if set("WAYLAND_DISPLAY") => Some(("wl-copy", &[])),
        _ if set("DISPLAY") => Some(("xclip", &["-selection", "clipboard"])),
        _ => None,
    };
    if let Some((program, args)) = tool {
        let child = Command::new(program)
            .args(args)
            .stdin(Stdio::piped())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn();
        if let Ok(mut child) = child {
            tokio::task::spawn_blocking(move || {
                if let Some(mut stdin) = child.stdin.take() {
                    let _ = stdin.write_all(text.as_bytes());
                }
                let _ = child.wait();
            });
            return;
        }
    }
    let encoded = base64::engine::general_purpose::STANDARD.encode(text);
    let out = terminal.backend_mut();
    let _ = write!(out, "\x1b]52;c;{encoded}\x07");
    let _ = Write::flush(out);
}

#[cfg(test)]
mod tests {
    use super::*;
    use app::state::{Load, Overlay};
    use app::tests::{keys, started};

    fn resumed(app: App, instance: &str, switch: bool) -> App {
        let session = Session::signed_out("dev", instance, "gone", Vec::new());
        build(
            &session,
            Options {
                resume: Some(Resume {
                    app: Box::new(app),
                    switch,
                }),
                ..Options::default()
            },
        )
    }

    fn drafting() -> App {
        let mut app = started(120, 40);
        keys(&mut app, &["Enter", "w", "half a thought"]);
        app
    }

    #[test]
    fn the_same_instance_keeps_the_draft_and_another_one_does_not() {
        for instance in ["https://acme.service-now.com", ""] {
            let app = resumed(drafting(), instance, false);
            assert!(matches!(app.overlays.last(), Some(Overlay::Form(_))));
            assert!(app.record.is_some());
            assert_eq!(app.connection.host, "acme.service-now.com");
        }
        for (instance, switch) in [
            ("https://other.service-now.com", false),
            ("https://acme.service-now.com", true),
        ] {
            let app = resumed(drafting(), instance, switch);
            assert!(app.overlays.is_empty() && app.record.is_none());
            assert_eq!(app.me, Load::Idle);
            assert_eq!(app.list.title, "Assigned to me");
        }
    }
}
