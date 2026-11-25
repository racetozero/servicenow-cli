//! What goes into the state machine and what it asks for.

use crate::model::{Baseline, Meta, Row};
use crate::tables::{Me, Related};
use ratatui::crossterm::event::{KeyEvent, MouseEvent};
use serde_json::{Map, Value};
use std::path::PathBuf;

/// Names a request, so a late answer to an old one can be ignored.
pub type Req = u64;

/// How many entries a tab asks for.
pub const TAB_LIMIT: u64 = 100;

/// A failure, as the state machine keeps it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Fault {
    /// One of `sn_core::error::CODES`.
    pub code: &'static str,
    pub message: String,
}

impl From<sn_core::Error> for Fault {
    fn from(err: sn_core::Error) -> Self {
        let mut message = err.to_string();
        if let sn_core::Error::DryRun(request) = &err {
            let part = |key: &str| request.get(key).and_then(Value::as_str).unwrap_or_default();
            message = format!("dry run: {} {} was not sent", part("method"), part("url"));
        }
        if let sn_core::Error::Api(failure) = &err
            && let Some(detail) = &failure.detail
        {
            message = format!("{message}: {detail}");
        }
        Self {
            code: err.code(),
            message,
        }
    }
}

pub type Outcome<T> = Result<T, Fault>;

/// A change to one record.
#[derive(Clone, Debug, PartialEq)]
pub struct Write {
    pub table: String,
    pub sys_id: String,
    pub name: String,
    pub body: Map<String, Value>,
    /// The version the change was made on. `None` for comments and work
    /// notes, which add and never overwrite.
    pub baseline: Option<Baseline>,
}

#[derive(Clone, Debug, PartialEq)]
pub enum Written {
    Saved {
        row: Row,
        /// Fields the instance did not take, without saying so.
        kept: Vec<String>,
    },
    /// Someone changed the record first; this is what it holds now.
    Conflict(Row),
    Failed(Fault),
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct Page {
    pub rows: Vec<Row>,
    pub total: Option<u64>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum EntryKind {
    Comment,
    WorkNote,
}

/// One entry of the conversation.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Entry {
    pub kind: EntryKind,
    pub by: String,
    pub at: String,
    /// Seconds since 1970, when the instance gave a UTC time.
    pub time: Option<i64>,
    pub text: String,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Attachment {
    pub sys_id: String,
    pub name: String,
    pub size: u64,
    pub by: String,
    pub time: Option<i64>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Sla {
    pub name: String,
    pub stage: String,
    pub breached: bool,
    /// Elapsed share of the target, in percent.
    pub percent: Option<u32>,
    pub left: String,
    pub due: String,
    pub due_time: Option<i64>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Change {
    pub field: String,
    pub old: String,
    pub new: String,
    pub by: String,
    pub time: Option<i64>,
}

/// A record that a reference field can point to.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Candidate {
    pub value: String,
    pub label: String,
    pub detail: String,
}

/// Work for the instance.
#[derive(Clone, Debug, PartialEq)]
pub enum Call {
    Me {
        req: Req,
    },
    Count {
        id: String,
        table: String,
        query: String,
    },
    Meta {
        table: String,
    },
    List {
        req: Req,
        table: String,
        query: String,
        fields: Vec<String>,
        view: Option<String>,
        offset: u64,
        limit: u64,
    },
    Record {
        req: Req,
        table: String,
        sys_id: String,
    },
    Activity {
        req: Req,
        table: String,
        sys_id: String,
    },
    /// One answer for each list.
    Related {
        req: Req,
        lists: Vec<Related>,
    },
    Attachments {
        req: Req,
        table: String,
        sys_id: String,
    },
    Slas {
        req: Req,
        sys_id: String,
    },
    History {
        req: Req,
        sys_id: String,
    },
    Write {
        req: Req,
        writes: Vec<Write>,
    },
    Create {
        req: Req,
        table: String,
        body: Map<String, Value>,
    },
    Download {
        req: Req,
        sys_id: String,
        name: String,
        dir: PathBuf,
    },
    References {
        req: Req,
        table: String,
        text: String,
    },
    /// Tables and tasks for the palette.
    Search {
        req: Req,
        text: String,
        records: bool,
    },
    /// The record a number, a link or `table:id` names.
    Locate {
        req: Req,
        text: String,
    },
}

impl Call {
    /// A newer call of the same slot makes the older one pointless. Writes
    /// have none: once sent, they are never dropped.
    pub fn slot(&self) -> Option<&'static str> {
        match self {
            Self::List { .. } => Some("list"),
            Self::Record { .. } => Some("record"),
            Self::References { .. } => Some("references"),
            Self::Search { .. } => Some("search"),
            Self::Activity { .. } => Some("activity"),
            Self::Related { .. } => Some("related"),
            Self::Attachments { .. } => Some("attachments"),
            Self::Slas { .. } => Some("slas"),
            Self::History { .. } => Some("history"),
            _ => None,
        }
    }
}

/// What the instance answered.
#[derive(Clone, Debug, PartialEq)]
pub enum Reply {
    Me(Req, Outcome<Me>),
    Count(String, Outcome<u64>),
    Meta(String, Outcome<Meta>),
    List(Req, Outcome<Page>),
    Record(Req, Outcome<Row>),
    Activity(Req, Outcome<Vec<Entry>>),
    /// The place of the list in the call.
    Related(Req, usize, Outcome<Vec<Row>>),
    Attachments(Req, Outcome<Vec<Attachment>>),
    Slas(Req, Outcome<Vec<Sla>>),
    History(Req, Outcome<Vec<Change>>),
    /// Records written so far.
    Progress(Req, usize),
    /// One result for each write, in order.
    Written(Req, Vec<Written>),
    Created(Req, Outcome<Row>),
    Downloaded(Req, Outcome<PathBuf>),
    References(Req, Outcome<Vec<Candidate>>),
    Tables(Req, Outcome<Vec<(String, String)>>),
    Records(Req, Outcome<Vec<Row>>),
    Located(Req, Outcome<(String, String)>),
}

impl Reply {
    /// The failures in the answer. A failed count only leaves a section
    /// without its number.
    pub fn faults(&self) -> impl Iterator<Item = &Fault> {
        let written = match self {
            Self::Written(_, results) => results.as_slice(),
            _ => &[],
        };
        let one = match self {
            Self::Me(_, o) => o.as_ref().err(),
            Self::Meta(_, o) => o.as_ref().err(),
            Self::List(_, o) => o.as_ref().err(),
            Self::Record(_, o) | Self::Created(_, o) => o.as_ref().err(),
            Self::Activity(_, o) => o.as_ref().err(),
            Self::Related(_, _, o) | Self::Records(_, o) => o.as_ref().err(),
            Self::Attachments(_, o) => o.as_ref().err(),
            Self::Slas(_, o) => o.as_ref().err(),
            Self::History(_, o) => o.as_ref().err(),
            Self::Downloaded(_, o) => o.as_ref().err(),
            Self::References(_, o) => o.as_ref().err(),
            Self::Tables(_, o) => o.as_ref().err(),
            Self::Located(_, o) => o.as_ref().err(),
            Self::Count(..) | Self::Progress(..) | Self::Written(..) => None,
        };
        written
            .iter()
            .filter_map(|w| match w {
                Written::Failed(fault) => Some(fault),
                _ => None,
            })
            .chain(one)
    }
}

/// Work for the terminal and the computer.
#[derive(Clone, Debug, PartialEq)]
pub enum Effect {
    Call(Call),
    Open(String),
    Copy(String),
    SavePins(Vec<String>),
    Quit,
    SignIn,
    SwitchProfile(String),
}

#[derive(Clone, Debug)]
pub enum Msg {
    Key(KeyEvent),
    Paste(String),
    Mouse(MouseEvent),
    Resize(u16, u16),
    /// `now` is seconds since 1970; `ms` only ever grows.
    Tick {
        now: i64,
        ms: u64,
    },
    Reply(Reply),
    /// What drawing found out: where things are.
    Drawn(crate::ui::Places),
    /// A browser took the link, or not.
    Opened(String, bool),
}
