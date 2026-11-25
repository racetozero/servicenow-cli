//! Record changes as they happen, over the message bus (`crate::amb`).
//!
//! The bus has no replay: what happens while no session is subscribed is
//! gone. The instance ends a session one or two minutes after it was opened,
//! so a new session is subscribed before the old one is left
//! (`--session-rotate`), which loses nothing. After a real connection loss
//! the stream carries a marker line that says where the hole is:
//!
//! ```text
//! {"sn_watch":"reconnected","downtime_ms":4100,"attempt":2}
//! ```
//!
//! Filters run here, because a channel has no filter besides its query.

use crate::amb::{self, Amb, Transport};
use crate::ctx::{Ctx, HandlesInterrupt};
use crate::output::Out;
use clap::{Args, ValueEnum};
use serde_json::{Value, json};
use sn_core::{Error, Result, query, refs};
use std::collections::HashSet;
use std::pin::Pin;
use std::time::Duration;
use tokio::sync::watch;
use tokio::time::Instant;

const MAX_ATTEMPTS: u32 = 10;
/// Failed rotations in a row that mean that the connection is lost, although
/// the old session has not noticed it.
const MAX_ROTATIONS: u32 = 3;
/// Seconds after which a session is replaced, before the instance ends it.
const ROTATE: u64 = 45;
/// A session that lived this long was healthy; its loss is a new problem.
const HEALTHY: Duration = Duration::from_secs(60);
/// The wait after a rotation that failed.
const ROTATE_RETRY: Duration = Duration::from_secs(10);
/// How long one look at the old session waits during a rotation, and how
/// many empty looks in a row mean that it is empty.
const DRAIN_WAIT: Duration = Duration::from_millis(100);
const DRAIN_QUIET: u32 = 3;
const DRAIN_LOOKS: u32 = 50;
/// How long after a rotation a change may arrive a second time.
const SEAM: Duration = Duration::from_secs(30);

#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
pub enum Operation {
    Insert,
    Update,
    Delete,
}

impl Operation {
    const fn as_str(self) -> &'static str {
        match self {
            Self::Insert => "insert",
            Self::Update => "update",
            Self::Delete => "delete",
        }
    }
}

/// When a stream ends by itself.
#[derive(Args, Debug, Clone, Default)]
pub struct Limits {
    /// Stop after this many events.
    #[arg(long, value_name = "N")]
    pub max_events: Option<u64>,
    /// Stop after this many seconds.
    #[arg(long, value_name = "SECONDS")]
    pub duration: Option<u64>,
    /// Stop when no event arrives for this many seconds.
    #[arg(long, value_name = "SECONDS")]
    pub idle_timeout: Option<u64>,
}

#[derive(Args, Debug)]
pub struct WatchArgs {
    /// Table, or one record: a number, table:number, table:sys_id, or a link.
    #[arg(value_name = "TABLE|RECORD")]
    pub table: String,
    /// sys_id or number when the first argument is a table.
    #[arg(value_name = "SYS_ID|NUMBER", conflicts_with = "query")]
    pub id: Option<String>,
    /// Records to watch, for example priority=1^active=true.
    #[arg(long, short = 'q', value_name = "QUERY", alias = "sysparm-query")]
    pub query: Option<String>,
    /// Only these operations (repeatable).
    #[arg(long, value_enum, value_delimiter = ',', value_name = "OPERATION")]
    pub operation: Vec<Operation>,
    /// Only changes of one of these fields.
    #[arg(
        long,
        value_delimiter = ',',
        value_name = "FIELDS",
        alias = "watch-fields"
    )]
    pub on_change: Vec<String>,
    /// Replace the session after this many seconds, before the instance ends it; 0 never does.
    #[arg(long, value_name = "SECONDS", default_value_t = ROTATE)]
    pub session_rotate: u64,
    /// Accepted for compatibility: events are never completed with more fields.
    #[arg(long, hide = true)]
    pub no_hydrate: bool,
    #[command(flatten)]
    pub limits: Limits,
}

/// What to watch, what to keep, and when to stop.
pub struct Plan {
    pub channel: String,
    pub operations: Vec<Operation>,
    pub on_change: Vec<String>,
    pub limits: Limits,
    pub rotate: Option<Duration>,
}

impl Plan {
    pub fn new(table: &str, filter: &str, limits: Limits) -> Self {
        Self {
            channel: amb::record_channel(table, filter),
            operations: Vec::new(),
            on_change: Vec::new(),
            limits,
            rotate: Some(Duration::from_secs(ROTATE)),
        }
    }

    fn accepts(&self, event: &Value) -> bool {
        if !self.operations.is_empty()
            && !self
                .operations
                .iter()
                .any(|o| event["operation"] == o.as_str())
        {
            return false;
        }
        // `changes` names every field of an insert and none of a delete.
        self.on_change.is_empty()
            || event["changes"]
                .as_array()
                .into_iter()
                .flatten()
                .filter_map(Value::as_str)
                .any(|changed| self.on_change.iter().any(|want| want == changed))
    }
}

pub async fn run(ctx: &Ctx, args: WatchArgs) -> Result<()> {
    let names_record = args.id.is_some()
        || args.table.contains(':')
        || (refs::prefix(&args.table).is_some() && args.query.is_none());
    let (table, filter) = if names_record {
        if args.query.is_some() {
            return Err(Error::invalid(
                "you passed a record and --query; pass a record to watch it, or a table with --query",
            ));
        }
        let client = ctx.client().await?;
        let record = refs::record(client, &args.table, args.id.as_deref()).await?;
        (record.table, format!("sys_id={}", record.sys_id))
    } else {
        query::check_name("table", &args.table)?;
        let filter = args.query.filter(|q| !q.trim().is_empty()).ok_or_else(|| {
            Error::Input {
                arg: "query".into(),
                message: format!(
                    "--query is missing; say which records of {0} to watch, for example `sn watch {0} -q active=true`, or name one record",
                    args.table
                ),
                choices: Vec::new(),
            }
        })?;
        (args.table, filter)
    };
    let mut plan = Plan::new(&table, &filter, args.limits);
    plan.operations = args.operation;
    plan.on_change = args.on_change;
    plan.rotate = (args.session_rotate > 0).then(|| Duration::from_secs(args.session_rotate));
    stream(ctx, plan).await
}

/// Streams the events of a plan to stdout until a limit or Ctrl-C.
pub async fn stream(ctx: &Ctx, plan: Plan) -> Result<()> {
    let client = ctx.client().await?;
    let timeout = Duration::from_secs(ctx.globals.timeout.unwrap_or(30));
    let transport = Transport::new(client, timeout)?;
    // A clean end also at Ctrl-C: leave the bus and exit with 0.
    let _handles = HandlesInterrupt::start();
    let (interrupt, stop) = watch::channel(false);
    tokio::spawn(async move {
        if tokio::signal::ctrl_c().await.is_ok() {
            let _ = interrupt.send(true);
        }
        // Keeps the channel open when there is no signal handler.
        std::future::pending::<()>().await;
    });
    let deadline = plan
        .limits
        .duration
        .map(|s| Instant::now() + Duration::from_secs(s));
    let connect = async || {
        // A new session each time: an expired token is renewed on the way.
        let cookies = amb::session_cookies(client).await?;
        transport.connect(&cookies, &plan.channel).await
    };
    let mut watcher = Watcher {
        out: &ctx.out,
        plan: &plan,
        deadline,
        stop,
        feed: Feed::new(|event| ctx.out.event(event), plan.limits.max_events),
    };
    watcher.supervise(&connect).await
}

/// A subscribed connection. Only tests use another one than [`Amb`].
trait Link: Sized {
    async fn poll(&mut self) -> Result<Vec<Value>>;
    async fn disconnect(self);
}

impl Link for Amb {
    async fn poll(&mut self) -> Result<Vec<Value>> {
        Self::poll(self).await
    }

    async fn disconnect(self) {
        Self::disconnect(self).await;
    }
}

/// The output side: the event count, the idle clock, and the changes that
/// may arrive twice during a rotation. A marker is not an event.
struct Feed<S> {
    /// Writes a line; false when nobody reads any more.
    sink: S,
    max_events: Option<u64>,
    emitted: u64,
    /// The last event, moved forward by the time without a subscription.
    last_event: Instant,
    seam: Option<(Instant, HashSet<String>)>,
}

impl<S: FnMut(Value) -> Result<bool>> Feed<S> {
    fn new(sink: S, max_events: Option<u64>) -> Self {
        Self {
            sink,
            max_events,
            emitted: 0,
            last_event: Instant::now(),
            seam: None,
        }
    }

    fn duplicate(&mut self, event: &Value) -> bool {
        match &mut self.seam {
            Some((until, seen)) if Instant::now() <= *until => {
                identity(event).is_some_and(|key| !seen.insert(key))
            }
            _ => {
                self.seam = None;
                false
            }
        }
    }

    /// Writes the events that the plan keeps; true when the stream is over.
    fn deliver(&mut self, plan: &Plan, events: Vec<Value>) -> Result<bool> {
        for event in events {
            if !plan.accepts(&event) || self.duplicate(&event) {
                continue;
            }
            let read = (self.sink)(event)?;
            self.emitted += 1;
            self.last_event = Instant::now();
            if !read || self.max_events.is_some_and(|max| self.emitted >= max) {
                return Ok(true);
            }
        }
        Ok(false)
    }

    /// Time without a subscription is not silence.
    fn forgive(&mut self, offline: Duration) {
        let now = Instant::now();
        self.last_event = (self.last_event + offline).min(now);
    }
}

/// What one write is known by: its record, operation, and modification
/// count. A table without the count cannot tell two writes apart, so its
/// events are never dropped.
fn identity(event: &Value) -> Option<String> {
    let sys_id = event["sys_id"].as_str()?;
    let operation = event["operation"].as_str().unwrap_or_default();
    if operation == "delete" {
        return Some(format!("{sys_id}|delete"));
    }
    let count = match event.pointer("/record/sys_mod_count/value")? {
        Value::String(text) => text.clone(),
        other => other.to_string(),
    };
    Some(format!("{sys_id}|{operation}|{count}"))
}

/// The connection failed: every other failure would repeat itself.
fn is_lost(err: &Error) -> bool {
    matches!(err, Error::Network(_)) || err.code() == "timeout"
}

/// The next attempt and its wait, or `None` to give up.
fn retry(
    err: &Error,
    established: bool,
    lifetime: Duration,
    attempt: u32,
) -> Option<(u32, Duration)> {
    // What never worked is a wrong setup.
    let next = if lifetime >= HEALTHY { 1 } else { attempt + 1 };
    (established && is_lost(err) && next <= MAX_ATTEMPTS).then(|| {
        (
            next,
            Duration::from_secs(2_u64.saturating_pow(next).min(60)),
        )
    })
}

async fn until(at: Option<Instant>) {
    match at {
        Some(at) => tokio::time::sleep_until(at).await,
        None => std::future::pending().await,
    }
}

/// A session that is being opened.
type Connecting<'a, L> = Pin<Box<dyn Future<Output = Result<L>> + 'a>>;

struct Watcher<'a, S> {
    out: &'a Out,
    plan: &'a Plan,
    deadline: Option<Instant>,
    stop: watch::Receiver<bool>,
    feed: Feed<S>,
}

impl<S: FnMut(Value) -> Result<bool>> Watcher<'_, S> {
    /// Ctrl-C, or the end of `--duration`.
    fn over(&self) -> impl Future<Output = ()> + use<S> {
        let mut stop = self.stop.clone();
        let deadline = self.deadline;
        async move {
            tokio::select! {
                stopped = stop.wait_for(|s| *s) => if stopped.is_err() { std::future::pending().await },
                () = until(deadline) => {}
            }
        }
    }

    /// One session after the other, until the stream is over.
    async fn supervise<L: Link>(&mut self, connect: &impl AsyncFn() -> Result<L>) -> Result<()> {
        let mut attempt = 0;
        let mut established = false;
        let mut gap: Option<u32> = None;
        let mut offline_since = Instant::now();
        loop {
            let started = Instant::now();
            let result = tokio::select! {
                () = self.over() => return Ok(()),
                linked = connect() => linked,
            };
            let err = match result {
                Err(err) => err,
                Ok(mut link) => {
                    established = true;
                    self.out.note(&format!("watching {}", self.plan.channel));
                    let offline = offline_since.elapsed();
                    self.feed.forgive(offline);
                    let mut read = Ok(true);
                    if let Some(attempt) = gap.take() {
                        read = (self.feed.sink)(json!({
                            "sn_watch": "reconnected",
                            "downtime_ms": u64::try_from(offline.as_millis()).unwrap_or(u64::MAX),
                            "attempt": attempt,
                        }));
                    }
                    let end = match read {
                        Ok(true) => self.pump(&mut link, connect).await,
                        Ok(false) => Ok(()),
                        Err(err) => Err(err),
                    };
                    link.disconnect().await;
                    offline_since = Instant::now();
                    match end {
                        Ok(()) => return Ok(()),
                        Err(err) => err,
                    }
                }
            };
            let Some((next, wait)) = retry(&err, established, started.elapsed(), attempt) else {
                return Err(err);
            };
            attempt = next;
            gap = Some(attempt);
            self.out.note(&format!(
                "{err}; reconnecting in {} seconds (attempt {attempt} of {MAX_ATTEMPTS})",
                wait.as_secs()
            ));
            tokio::select! {
                () = self.over() => return Ok(()),
                () = tokio::time::sleep(wait) => {}
            }
        }
    }

    /// Delivers events, and replaces the session before the instance ends
    /// it. `Ok` means that the stream is over.
    async fn pump<L: Link>(
        &mut self,
        link: &mut L,
        connect: &impl AsyncFn() -> Result<L>,
    ) -> Result<()> {
        let idle = self.plan.limits.idle_timeout.map(Duration::from_secs);
        let mut rotate_at = self.plan.rotate.map(|d| Instant::now() + d);
        // The session being prepared, and when that started: its time runs
        // from there.
        let mut next: Option<(Instant, Connecting<'_, L>)> = None;
        let mut lost = 0;
        loop {
            let idle_at = idle.map(|d| self.feed.last_event + d);
            let prepared = async {
                match &mut next {
                    Some((_, future)) => future.await,
                    None => std::future::pending().await,
                }
            };
            tokio::select! {
                biased;
                () = self.over() => return Ok(()),
                () = until(idle_at) => return Ok(()),
                fresh = prepared => {
                    let since = next.take().map_or_else(Instant::now, |(since, _)| since);
                    match fresh {
                        Ok(fresh) => {
                            lost = 0;
                            let over = self.hand_over(link, fresh).await?;
                            rotate_at = self.plan.rotate.map(|d| since + d);
                            if over {
                                return Ok(());
                            }
                        }
                        Err(err) => {
                            lost += u32::from(is_lost(&err));
                            if lost >= MAX_ROTATIONS {
                                return Err(err);
                            }
                            self.out.note(&format!("the session could not be replaced ({err}); trying again"));
                            rotate_at = Some(Instant::now() + ROTATE_RETRY);
                        }
                    }
                }
                () = until(rotate_at) => {
                    rotate_at = None;
                    next = Some((Instant::now(), Box::pin(connect())));
                }
                events = link.poll() => if self.feed.deliver(self.plan, events?)? {
                    return Ok(());
                }
            }
        }
    }

    /// Takes the rest of the old session, then leaves it. True when the
    /// stream is over.
    async fn hand_over<L: Link>(&mut self, link: &mut L, fresh: L) -> Result<bool> {
        let old = std::mem::replace(link, fresh);
        self.feed.seam = Some((Instant::now() + SEAM, HashSet::new()));
        let mut old = old;
        let mut quiet = 0;
        let mut over = Ok(false);
        for _ in 0..DRAIN_LOOKS {
            match tokio::time::timeout(DRAIN_WAIT, old.poll()).await {
                Ok(Ok(events)) if !events.is_empty() => {
                    quiet = 0;
                    over = self.feed.deliver(self.plan, events);
                    if !matches!(over, Ok(false)) {
                        break;
                    }
                }
                // The new session has the same changes.
                Ok(Err(_)) => break,
                // An empty list can also be an answer without events.
                Ok(Ok(_)) | Err(_) => quiet += 1,
            }
            if quiet >= DRAIN_QUIET {
                break;
            }
        }
        old.disconnect().await;
        over
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::RefCell;
    use std::collections::VecDeque;
    use std::rc::Rc;

    // Payloads copied from events of a live instance.
    fn update(sys_id: &str, count: u32) -> Value {
        json!({
            "operation": "update", "action": "change", "sys_id": sys_id,
            "changes": ["state", "incident_state"],
            "record": { "sys_mod_count": { "display_value": count.to_string(), "value": count.to_string() } },
        })
    }

    fn plan(limits: Limits) -> Plan {
        Plan {
            rotate: None,
            ..Plan::new("incident", "active=true", limits)
        }
    }

    #[test]
    fn filters_combine_operations_and_fields() {
        let insert =
            json!({ "operation": "insert", "changes": ["number", "state", "short_description"] });
        let delete = json!({ "operation": "delete", "changes": [] });
        let mut plan = plan(Limits::default());
        assert!(plan.accepts(&insert) && plan.accepts(&delete));
        plan.operations = vec![Operation::Insert, Operation::Delete];
        assert!(plan.accepts(&insert) && plan.accepts(&delete) && !plan.accepts(&update("a", 1)));
        plan.operations.clear();
        plan.on_change = vec!["short_description".into()];
        assert!(plan.accepts(&insert), "an insert changes every field");
        assert!(!plan.accepts(&delete) && !plan.accepts(&update("a", 1)));
        plan.on_change = vec!["state".into()];
        plan.operations = vec![Operation::Update];
        assert!(plan.accepts(&update("a", 1)) && !plan.accepts(&insert));
        assert!(!plan.accepts(&json!({ "unrelated": 1 })));
    }

    #[test]
    fn duplicates_exist_only_around_a_rotation() {
        let mut feed = Feed::new(|_| Ok(true), None);
        assert!(!feed.duplicate(&update("a", 7)) && !feed.duplicate(&update("a", 7)));
        feed.seam = Some((Instant::now() + SEAM, HashSet::new()));
        assert!(!feed.duplicate(&update("a", 7)) && feed.duplicate(&update("a", 7)));
        assert!(!feed.duplicate(&update("a", 8)), "a second write");
        let bare = json!({ "operation": "update", "sys_id": "a", "record": {} });
        assert!(
            !feed.duplicate(&bare) && !feed.duplicate(&bare),
            "no write count"
        );
        let delete = json!({ "operation": "delete", "sys_id": "a" });
        assert!(!feed.duplicate(&delete) && feed.duplicate(&delete));
    }

    #[test]
    fn only_lost_connections_that_worked_are_retried() {
        let lost = Error::Network("lost".into());
        let short = Duration::from_secs(1);
        assert_eq!(
            retry(&lost, true, short, 0),
            Some((1, Duration::from_secs(2)))
        );
        assert_eq!(
            retry(&lost, true, short, 6),
            Some((7, Duration::from_secs(60)))
        );
        assert_eq!(
            retry(&lost, true, HEALTHY, 9),
            Some((1, Duration::from_secs(2)))
        );
        assert_eq!(retry(&lost, true, short, 10), None);
        assert_eq!(retry(&lost, false, short, 0), None);
        assert_eq!(retry(&Error::Auth("rejected".into()), true, short, 0), None);
    }

    /// Milliseconds after the connection, and what arrives then.
    type Batch = (u64, Result<Vec<Value>>);

    /// A connection that plays a script, then is silent.
    struct Played {
        since: Instant,
        batches: VecDeque<Batch>,
        left: Rc<RefCell<u32>>,
    }

    impl Link for Played {
        async fn poll(&mut self) -> Result<Vec<Value>> {
            let at = self
                .batches
                .front()
                .map(|(ms, _)| self.since + Duration::from_millis(*ms));
            until(at).await;
            self.batches.pop_front().expect("a batch").1
        }

        async fn disconnect(self) {
            *self.left.borrow_mut() += 1;
        }
    }

    /// Runs a watcher over one scripted connection per attempt.
    async fn play(plan: &Plan, sessions: Vec<Result<Vec<Batch>>>) -> (Result<()>, Vec<Value>, u32) {
        let lines = RefCell::new(Vec::new());
        let left = Rc::new(RefCell::new(0));
        let sessions = RefCell::new(VecDeque::from(sessions));
        let connect = async || {
            let next = sessions.borrow_mut().pop_front();
            let batches = next.expect("no more sessions")?;
            Ok(Played {
                since: Instant::now(),
                batches: batches.into(),
                left: Rc::clone(&left),
            })
        };
        let (_interrupt, stop) = watch::channel(false);
        let out = crate::output::Out {
            agent: true,
            format: crate::output::Format::Auto,
            select: Vec::new(),
            jq: None,
            raw: false,
            tty: false,
            no_headers: false,
            no_truncate: false,
            quiet: true,
            pager: false,
        };
        let mut watcher = Watcher {
            out: &out,
            plan,
            deadline: plan
                .limits
                .duration
                .map(|s| Instant::now() + Duration::from_secs(s)),
            stop,
            feed: Feed::new(
                |event| {
                    lines.borrow_mut().push(event);
                    Ok(true)
                },
                plan.limits.max_events,
            ),
        };
        let result = watcher.supervise(&connect).await;
        let left = *left.borrow();
        (result, lines.into_inner(), left)
    }

    #[tokio::test(start_paused = true)]
    async fn a_dead_connection_that_stays_silent_is_noticed() {
        let mut plan = plan(Limits {
            max_events: Some(1),
            ..Limits::default()
        });
        plan.rotate = Some(Duration::from_secs(45));
        let down = || Err(Error::Network("down".into()));
        let (result, lines, left) = play(
            &plan,
            vec![
                Ok(vec![]),
                down(),
                down(),
                down(),
                Ok(vec![(0, Ok(vec![update("a", 1)]))]),
            ],
        )
        .await;
        result.expect("ended by the limit");
        assert_eq!(lines[0]["sn_watch"], "reconnected", "{lines:?}");
        assert_eq!((lines.len(), left), (2, 2));
    }

    fn lost() -> Batch {
        (0, Err(Error::Network("lost".into())))
    }

    #[tokio::test(start_paused = true)]
    async fn a_lost_connection_leaves_one_marker_between_the_events() {
        let plan = plan(Limits {
            max_events: Some(2),
            ..Limits::default()
        });
        let (result, lines, left) = play(
            &plan,
            vec![
                Ok(vec![(0, Ok(vec![update("a", 1)])), lost()]),
                Err(Error::Network("still down".into())),
                Ok(vec![(0, Ok(vec![update("a", 2)]))]),
            ],
        )
        .await;
        result.expect("ended by the limit");
        assert_eq!(lines.len(), 3, "{lines:?}");
        assert_eq!(
            lines[1],
            json!({ "sn_watch": "reconnected", "downtime_ms": 6000, "attempt": 2 }),
            "2 s and 4 s of waiting, and the marker counts as no event"
        );
        assert_eq!(left, 2);
    }

    #[tokio::test(start_paused = true)]
    async fn the_first_failure_is_final() {
        let (result, lines, _) = play(
            &plan(Limits::default()),
            vec![Err(Error::Network("down".into()))],
        )
        .await;
        assert_eq!(result.expect_err("final").to_string(), "down");
        assert!(lines.is_empty());
    }

    #[tokio::test(start_paused = true)]
    async fn rotation_loses_and_repeats_nothing() {
        let mut plan = plan(Limits {
            idle_timeout: Some(60),
            ..Limits::default()
        });
        plan.rotate = Some(Duration::from_secs(45));
        let (result, lines, left) = play(
            &plan,
            vec![
                // One change arrives on both sessions during the rotation.
                Ok(vec![
                    (0, Ok(vec![update("a", 1)])),
                    (45_050, Ok(vec![update("a", 2)])),
                ]),
                Ok(vec![(60, Ok(vec![update("a", 2), update("a", 3)]))]),
                Ok(vec![]),
            ],
        )
        .await;
        result.expect("idle");
        assert_eq!(lines, [update("a", 1), update("a", 2), update("a", 3)]);
        assert_eq!(left, 3, "every session says goodbye");
    }

    #[tokio::test(start_paused = true)]
    async fn an_outage_is_not_silence() {
        let plan = plan(Limits {
            idle_timeout: Some(1),
            ..Limits::default()
        });
        let started = Instant::now();
        let (result, lines, _) = play(&plan, vec![Ok(vec![lost()]), Ok(vec![])]).await;
        result.expect("idle");
        assert_eq!(lines.len(), 1, "the marker: {lines:?}");
        assert_eq!(
            started.elapsed().as_secs(),
            3,
            "2 s of outage and 1 s of silence"
        );
    }
}
