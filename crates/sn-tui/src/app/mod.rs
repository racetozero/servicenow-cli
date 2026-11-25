//! The state machine: a message changes the state and asks for effects.
//! Nothing here touches the network, the terminal or the clock.

mod act;
mod list;
mod overlay;
mod record;
pub mod state;
#[cfg(test)]
pub(crate) mod tests;
mod write;

use crate::keys::{self, Act, Place};
use crate::model::{Field, Meta};
use crate::msg::{Call, Effect, Fault, Msg, Reply, Req};
use crate::tables::{self, Me, SECTIONS, Section};
use crate::theme::{Caps, Theme};
use crate::ui::{Hit, Places};
use ratatui::crossterm::event::{
    KeyCode, KeyEvent, KeyEventKind, KeyModifiers, MouseButton, MouseEvent, MouseEventKind,
};
use ratatui::layout::Position;
use state::*;
use std::borrow::Cow;
use std::collections::HashMap;

/// From this width the list and the record stand side by side.
pub const SIDE_BY_SIDE: u16 = 110;
/// Below this width the rail is a drawer.
pub const RAIL: u16 = 90;
/// From this height a narrow terminal shows the record under the list.
pub const STACKED: u16 = 32;
/// From this width the record beside the list is on at the start.
const WIDE: u16 = 150;

const REFRESH_EVERY: u64 = 30_000;
/// Typing stops this long before the instance is asked.
const PAUSE: u64 = 250;

#[derive(Clone, Debug)]
pub struct App {
    pub theme: Theme,
    pub settings: Settings,
    pub connection: Connection,
    pub profiles: Vec<ProfileInfo>,
    pub size: (u16, u16),
    /// Seconds since 1970.
    pub now: i64,
    /// Milliseconds since the start.
    pub ms: u64,
    pub me: Load<Me>,
    pub focus: Pane,
    pub rail_cursor: usize,
    pub counts: HashMap<String, Load<u64>>,
    pub list: ListState,
    pub record: Option<RecordState>,
    /// Records under the open one: the way back.
    pub records: Vec<RecordState>,
    pub back: Vec<Snapshot>,
    pub overlays: Vec<Overlay>,
    pub toasts: Vec<Toast>,
    pub metas: HashMap<String, Load<Meta>>,
    pub auto_refresh: bool,
    refresh_at: u64,
    /// The record beside the list follows the cursor; `None` decides by the
    /// width.
    pub preview: Option<bool>,
    follow_at: Option<u64>,
    downloading: Option<Req>,
    pending: Option<Pending>,
    pub places: Places,
    next_req: Req,
    effects: Vec<Effect>,
    /// Something on screen changed since the last frame.
    pub dirty: bool,
}

impl App {
    pub fn new(
        connection: Connection,
        profiles: Vec<ProfileInfo>,
        settings: Settings,
        caps: Caps,
    ) -> Self {
        let mut app = Self {
            theme: Theme::new(caps),
            settings,
            connection,
            profiles,
            size: (0, 0),
            now: 0,
            ms: 0,
            me: Load::Idle,
            focus: Pane::List,
            rail_cursor: 0,
            counts: HashMap::new(),
            list: ListState::new("task", "", "", Vec::new()),
            record: None,
            records: Vec::new(),
            back: Vec::new(),
            overlays: Vec::new(),
            toasts: Vec::new(),
            metas: HashMap::new(),
            auto_refresh: false,
            refresh_at: 0,
            preview: None,
            follow_at: None,
            downloading: None,
            pending: None,
            places: Places::default(),
            next_req: 0,
            effects: Vec::new(),
            dirty: true,
        };
        match app.settings.start.clone() {
            Some(start) => {
                let (query, mut order) =
                    crate::query::split_order(&start.query.unwrap_or_default());
                if order.is_empty() {
                    order = vec![("sys_updated_on".into(), true)];
                }
                let title = tables::many(&start.table);
                app.list = ListState::new(&start.table, &title, &query, order);
                app.list.view = start.view;
                if app.list.view.is_some() {
                    app.list.columns.clear();
                }
            }
            None => app.list = app.section_list(SECTIONS[0].id),
        }
        app.rail_to_list();
        app
    }

    /// The first effects. Call it once, before the first message.
    pub fn start(&mut self) -> Vec<Effect> {
        if self.connection.signed_out.is_none() {
            let req = self.req();
            self.me = Load::Loading(req);
            self.call(Call::Me { req });
            self.refresh_list();
            self.load_counts();
        }
        self.dirty = true;
        std::mem::take(&mut self.effects)
    }

    /// Continues with a new session in the place where the old one ended.
    /// Nothing that was typed is lost.
    pub fn resume(&mut self, connection: Connection, profiles: Vec<ProfileInfo>) {
        self.connection = connection;
        self.profiles = profiles;
        self.me = Load::Idle;
        self.counts.clear();
        self.toasts.clear();
        for meta in self.metas.values_mut() {
            meta.forget();
        }
        self.metas.retain(|_, m| m.ready().is_some());
        self.downloading = None;
        self.pending = None;
        self.list.extra = Extra::None;
        if self.list.phase != ListPhase::Ready {
            self.list.phase = ListPhase::Waiting;
        }
        if let Some(record) = &mut self.record {
            record.forget();
            if matches!(record.full, Load::Failed(_)) {
                record.full = Load::Idle;
            }
        }
        for overlay in &mut self.overlays {
            match overlay {
                Overlay::Review(review) => review.state = ReviewState::Ready,
                Overlay::Picker(p) if p.found.is_loading() => p.search_at = Some(self.ms),
                Overlay::Palette(p) if p.tables.is_loading() || p.records.is_loading() => {
                    p.search_at = Some(self.ms);
                }
                _ => {}
            }
        }
        self.load_record();
    }

    /// The same view on another profile. Records belong to one instance, so
    /// only the section or the table carries over.
    pub fn switch(&self, connection: Connection, profiles: Vec<ProfileInfo>) -> Self {
        let mut settings = self.settings.clone();
        settings.start = None;
        let mut app = Self::new(connection, profiles, settings, self.theme.caps);
        app.list = match &self.list.section {
            Some(id) => app.section_list(id),
            None => ListState::new(
                &self.list.table,
                &self.list.title,
                &self.list.query,
                self.list.order.clone(),
            ),
        };
        app.size = self.size;
        app.preview = self.preview;
        app.auto_refresh = self.auto_refresh;
        (app.now, app.ms) = (self.now, self.ms);
        app.rail_to_list();
        app
    }

    pub fn update(&mut self, msg: Msg) -> Vec<Effect> {
        match msg {
            Msg::Key(key) if key.kind != KeyEventKind::Release => {
                self.dirty = true;
                self.key(key);
            }
            Msg::Key(_) => {}
            Msg::Paste(text) => {
                self.dirty = true;
                self.paste(&text);
            }
            Msg::Mouse(mouse) => self.mouse(mouse),
            Msg::Resize(width, height) => {
                self.dirty = true;
                let first = self.size == (0, 0);
                self.size = (width, height);
                if self.focus == Pane::Record && self.record.is_none() {
                    self.focus = Pane::List;
                }
                if first {
                    self.follow_soon();
                }
            }
            Msg::Tick { now, ms } => self.tick(now, ms),
            Msg::Reply(reply) => {
                self.dirty = true;
                self.reply(reply);
            }
            Msg::Drawn(places) => self.places = places,
            Msg::Opened(url, true) => {
                self.toast(ToastKind::Good, format!("Opened in the browser: {url}"));
            }
            Msg::Opened(url, false) => {
                self.toast(
                    ToastKind::Warning,
                    format!("No browser took the link. Open it yourself: {url}"),
                );
            }
        }
        std::mem::take(&mut self.effects)
    }

    fn tick(&mut self, now: i64, ms: u64) {
        let minute = self.now / 60 != now / 60;
        (self.now, self.ms) = (now, ms);
        let before = self.toasts.len();
        self.toasts.retain(|t| t.until > ms);
        if minute || before != self.toasts.len() || self.is_busy() {
            self.dirty = true;
        }
        if self.follow_at.is_some_and(|at| at <= ms) {
            self.follow_at = None;
            self.follow();
        }
        self.run_searches();
        if self.auto_refresh && self.refresh_at <= ms && self.connection.signed_out.is_none() {
            self.refresh_at = ms + REFRESH_EVERY;
            self.refresh_list();
            self.load_counts();
            if self.record.as_ref().is_some_and(|r| !r.has_draft()) {
                self.reload_record();
            }
        }
    }

    /// True while something is on its way, for the spinner.
    pub fn is_busy(&self) -> bool {
        self.activity().is_some()
    }

    /// What the workspace waits for.
    pub fn activity(&self) -> Option<&'static str> {
        let sending = self.overlays.iter().any(
            |o| matches!(o, Overlay::Review(r) if matches!(r.state, ReviewState::Sending(..))),
        );
        if sending {
            return Some("saving");
        }
        if self.downloading.is_some() {
            return Some("downloading");
        }
        if matches!(self.pending, Some(Pending::Locate(_))) {
            return Some("finding the record");
        }
        if matches!(self.list.phase, ListPhase::Loading(_)) || self.list.extra != Extra::None {
            return Some("loading");
        }
        if self.record.as_ref().is_some_and(|r| {
            r.full.is_loading()
                || r.activity.is_loading()
                || r.related.iter().any(|(_, l)| l.is_loading())
                || r.attachments.is_loading()
                || r.slas.is_loading()
                || r.history.is_loading()
        }) {
            return Some("loading");
        }
        if self.me.is_loading() && self.list.phase == ListPhase::Waiting {
            return Some("signing in");
        }
        let searching = self.overlays.iter().any(|o| match o {
            Overlay::Palette(p) => p.tables.is_loading() || p.records.is_loading(),
            Overlay::Picker(p) => p.found.is_loading(),
            _ => false,
        });
        if searching {
            return Some("searching");
        }
        (self.list.phase == ListPhase::Waiting && self.connection.signed_out.is_none())
            .then_some("loading")
    }

    fn req(&mut self) -> Req {
        self.next_req += 1;
        self.next_req
    }

    fn call(&mut self, call: Call) {
        self.effects.push(Effect::Call(call));
    }

    pub fn toast(&mut self, kind: ToastKind, text: impl Into<String>) {
        let text = text.into();
        self.dirty = true;
        self.toasts.retain(|t| t.text != text);
        let seconds = match kind {
            ToastKind::Info | ToastKind::Good => 4,
            ToastKind::Warning => 8,
            ToastKind::Error => 10,
        };
        self.toasts.push(Toast {
            kind,
            text,
            until: self.ms + seconds * 1000,
        });
        if self.toasts.len() > 3 {
            self.toasts.remove(0);
        }
    }

    /// Notes a failure that has no place of its own on screen. The end of
    /// the session has the sign-in card.
    fn fail(&mut self, fault: &Fault) {
        if fault.code != "auth" {
            self.toast(ToastKind::Error, fault.message.clone());
        }
    }

    fn sign_out(&mut self, reason: &str) {
        // The request that met the refusal says nothing to a person.
        let reason = match reason.contains("HTTP 401") {
            true => {
                "The instance does not accept your sign-in any more. It ended, or the credential is wrong."
            }
            false => reason,
        };
        self.connection.signed_out = Some(reason.to_owned());
    }

    /// The overlay that takes keys and is drawn. While signed out, the others
    /// wait under the sign-in card.
    pub fn top(&self) -> Option<&Overlay> {
        let signed_out = self.connection.signed_out.is_some();
        self.overlays
            .last()
            .filter(|o| !signed_out || matches!(o, Overlay::Profiles(_) | Overlay::Confirm { .. }))
    }

    pub fn me(&self) -> Option<&Me> {
        self.me.ready()
    }

    pub fn meta(&self, table: &str) -> Option<&Meta> {
        self.metas.get(table).and_then(Load::ready)
    }

    /// The field with its label and editor, or the best guess.
    pub fn field(&self, table: &str, name: &str) -> Cow<'_, Field> {
        match self.meta(table).and_then(|m| m.field(name)) {
            Some(field) => Cow::Borrowed(field),
            None => Cow::Owned(Field::guess(name)),
        }
    }

    /// Asks for the fields of a table once.
    fn want_meta(&mut self, table: &str) {
        let load = self.metas.entry(table.to_owned()).or_default();
        if matches!(load, Load::Idle) {
            *load = Load::Loading(0);
            self.call(Call::Meta {
                table: table.to_owned(),
            });
        }
    }

    fn load_counts(&mut self) {
        let mut asks: Vec<(String, String, String)> = Vec::new();
        for section in SECTIONS {
            if let Some(query) = section.query(self.me()) {
                asks.push((section.id.to_owned(), section.table.to_owned(), query));
            }
        }
        for pin in &self.settings.pins {
            asks.push((format!("pin:{pin}"), pin.clone(), String::new()));
        }
        for (id, table, query) in asks {
            let load = self.counts.entry(id.clone()).or_default();
            // A known count stays on screen while the new one comes.
            if !matches!(load, Load::Ready(_)) {
                if load.is_loading() {
                    continue;
                }
                *load = Load::Loading(0);
            }
            self.call(Call::Count { id, table, query });
        }
    }

    pub fn rail(&self) -> Vec<RailItem> {
        let mut out = Vec::new();
        let mut group = "";
        for section in SECTIONS {
            if section.group != group {
                group = section.group;
                out.push(RailItem::Group(group));
            }
            out.push(RailItem::Section(
                section.id.to_owned(),
                section.label.to_owned(),
            ));
        }
        if !self.settings.pins.is_empty() {
            out.push(RailItem::Group("Pinned"));
            for pin in &self.settings.pins {
                out.push(RailItem::Section(format!("pin:{pin}"), tables::many(pin)));
            }
        }
        out
    }

    /// Puts the cursor of the rail on what the list shows.
    fn rail_to_list(&mut self) {
        let at = self.rail().iter().position(
            |i| matches!(i, RailItem::Section(id, _) if Some(id) == self.list.section.as_ref()),
        );
        self.rail_cursor = at.unwrap_or(1);
    }

    pub fn rail_shown(&self) -> bool {
        self.size.0 >= RAIL
    }

    pub fn side_by_side(&self) -> bool {
        self.size.0 >= SIDE_BY_SIDE
    }

    pub fn stacked(&self) -> bool {
        !self.side_by_side() && self.size.1 >= STACKED
    }

    /// The list and the record are both on screen.
    pub fn split(&self) -> bool {
        self.side_by_side() || self.stacked()
    }

    pub fn previewing(&self) -> bool {
        self.side_by_side() && self.preview.unwrap_or(self.size.0 >= WIDE)
    }

    /// Where a key works now.
    pub fn place(&self) -> Place {
        match self.focus {
            Pane::Rail => Place::Rail,
            Pane::List => Place::List,
            Pane::Record => Place::Record,
        }
    }

    /// Something typed or changed would be lost by leaving.
    pub fn has_unsent(&self) -> bool {
        self.overlays.iter().any(|o| match o {
            Overlay::Form(form) => form.fields.iter().any(|f| !f.editor.text.trim().is_empty()),
            Overlay::Text { .. } | Overlay::Review(_) => true,
            _ => false,
        }) || self.open_has_draft()
            || self.back.iter().any(Snapshot::has_draft)
    }

    /// The same for the open record and those under it.
    fn open_has_draft(&self) -> bool {
        self.record
            .iter()
            .chain(&self.records)
            .any(RecordState::has_draft)
    }

    fn is_sending(&self) -> bool {
        self.activity() == Some("saving")
    }

    fn key(&mut self, key: KeyEvent) {
        self.pending = None;
        let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
        if ctrl && key.code == KeyCode::Char('c') {
            self.quit();
            return;
        }
        if self.top().is_some() {
            self.overlay_key(key);
            return;
        }
        if self.connection.signed_out.is_some() {
            match key.code {
                KeyCode::Enter | KeyCode::Char('a') => self.act(Act::SignIn),
                KeyCode::Char('P') => self.act(Act::Profiles),
                KeyCode::Char('q') | KeyCode::Esc => self.quit(),
                _ => {}
            }
            return;
        }
        if self.list.searching && self.focus == Pane::List {
            self.search_key(key);
            return;
        }
        if self.record.as_ref().is_some_and(|r| r.inline.is_some()) && self.focus == Pane::Record {
            self.inline_key(key);
            return;
        }
        if let Some(act) = keys::find(key, self.place()) {
            self.act(act);
        }
    }

    fn paste(&mut self, text: &str) {
        self.pending = None;
        if self.connection.signed_out.is_some() {
            return;
        }
        if !self.overlays.is_empty() {
            self.overlay_paste(text);
        } else if self.list.searching && self.focus == Pane::List {
            self.list.search.insert(text);
            self.list.cursor = 0;
        } else if let Some(inline) = self.record.as_mut().and_then(|r| r.inline.as_mut()) {
            inline.editor.insert(text);
        } else if sn_core::refs::prefix(text.trim()).is_some() || text.trim().starts_with("http") {
            // A pasted number or link is a wish to see the record.
            self.locate(text.trim());
        }
    }

    fn quit(&mut self) {
        if self.is_sending() {
            self.toast(
                ToastKind::Warning,
                "A save is on its way. Wait until it ends, then quit.",
            );
        } else if self.has_unsent()
            && !matches!(
                self.overlays.last(),
                Some(Overlay::Confirm {
                    then: Then::Quit,
                    ..
                })
            )
        {
            self.overlays.push(Overlay::Confirm {
                text: "You have changes that are not sent. Leave and lose them?".into(),
                yes: "leave",
                then: Then::Quit,
            });
        } else {
            self.effects.push(Effect::Quit);
        }
    }

    fn mouse(&mut self, mouse: MouseEvent) {
        let at = Position::new(mouse.column, mouse.row);
        let hit = |app: &Self| {
            app.places
                .hits
                .iter()
                .rev()
                .find(|(area, _)| area.contains(at))
                .map(|(_, hit)| hit.clone())
        };
        match mouse.kind {
            MouseEventKind::Down(MouseButton::Left) => {
                if let Some(hit) = hit(self) {
                    self.dirty = true;
                    self.click(hit);
                }
            }
            MouseEventKind::ScrollUp | MouseEventKind::ScrollDown => {
                self.pending = None;
                let down = mouse.kind == MouseEventKind::ScrollDown;
                self.dirty = true;
                let key = KeyEvent::new(
                    if down { KeyCode::Down } else { KeyCode::Up },
                    KeyModifiers::NONE,
                );
                if self.top().is_some() {
                    self.overlay_key(key);
                    return;
                }
                // The pane under the pointer moves; the focus stays.
                let pane = self
                    .places
                    .hits
                    .iter()
                    .rev()
                    .find_map(|(area, hit)| match hit {
                        Hit::Pane(pane) if area.contains(at) => Some(*pane),
                        _ => None,
                    });
                let by = if down { 3 } else { -3 };
                match pane {
                    Some(Pane::Rail) => self.move_rail(if down { 1 } else { -1 }),
                    Some(Pane::List) => self.move_list(by),
                    Some(Pane::Record) => self.move_record(if down { 2 } else { -2 }),
                    None => {}
                }
            }
            _ => {}
        }
    }

    fn click(&mut self, hit: Hit) {
        self.pending = None;
        match hit {
            Hit::Outside => {
                self.overlay_key(KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE));
                return;
            }
            Hit::Overlay => return,
            Hit::Item(i) => {
                self.overlay_click(i);
                return;
            }
            Hit::Button(code) => {
                self.key(code);
                return;
            }
            _ => {}
        }
        if self.connection.signed_out.is_some() {
            return;
        }
        // Leaving a field keeps what was typed, as Enter does.
        let same_field = matches!(hit, Hit::Detail(i)
            if self.record.as_ref().is_some_and(|r| r.cursor == i));
        // A wrong value stays on screen, with its error.
        if !same_field && !self.commit_inline() && matches!(hit, Hit::Tab(_) | Hit::Detail(_)) {
            return;
        }
        match hit {
            Hit::Rail(i) => {
                self.focus = Pane::Rail;
                self.rail_cursor = i;
                self.open_rail();
            }
            Hit::Row(i) => {
                let again = self.focus == Pane::List && self.list.cursor == i;
                self.focus = Pane::List;
                self.list.searching = false;
                if again {
                    self.act(Act::Open);
                } else {
                    self.move_list_to(i);
                }
            }
            Hit::Mark(i) => {
                self.focus = Pane::List;
                self.move_list_to(i);
                self.act(Act::Select);
            }
            Hit::Column(field) => {
                self.focus = Pane::List;
                self.sort_by(&field);
            }
            Hit::Tab(tab) => {
                self.focus = Pane::Record;
                self.show_tab(tab);
            }
            Hit::Detail(i) => {
                let again = self.focus == Pane::Record
                    && self.record.as_ref().is_some_and(|r| r.cursor == i);
                self.focus = Pane::Record;
                if let Some(record) = &mut self.record {
                    record.cursor = i;
                }
                if again && self.record.as_ref().is_some_and(|r| r.inline.is_none()) {
                    self.act(Act::Open);
                }
            }
            Hit::Related(i) => {
                let again = self.focus == Pane::Record
                    && self.record.as_ref().is_some_and(|r| r.related_cursor == i);
                self.focus = Pane::Record;
                if let Some(record) = &mut self.record {
                    record.related_cursor = i;
                }
                if again {
                    self.act(Act::Open);
                }
            }
            Hit::File(i) => {
                let again = self.focus == Pane::Record
                    && self.record.as_ref().is_some_and(|r| r.file_cursor == i);
                self.focus = Pane::Record;
                if let Some(record) = &mut self.record {
                    record.file_cursor = i;
                }
                if again {
                    self.act(Act::Open);
                }
            }
            Hit::Act(act) => self.act(act),
            Hit::Pane(pane) => {
                if pane != Pane::Record || self.record.is_some() {
                    self.focus = pane;
                }
            }
            Hit::Outside | Hit::Overlay | Hit::Item(_) | Hit::Button(_) => {}
        }
    }

    fn reply(&mut self, reply: Reply) {
        if let Some(fault) = reply.faults().find(|f| f.code == "auth") {
            self.sign_out(&fault.message);
        }
        match reply {
            Reply::Me(req, outcome) => {
                if self.me.answer(req, outcome) {
                    self.me_known();
                }
            }
            Reply::Count(id, outcome) => {
                let old = self.counts.get(&id);
                // A failed refresh keeps the old number.
                if outcome.is_ok() || !matches!(old, Some(Load::Ready(_))) {
                    let load = self.counts.entry(id).or_default();
                    load.set(outcome);
                }
            }
            Reply::Meta(table, outcome) => {
                self.metas.entry(table.clone()).or_default().set(outcome);
                if self.list.table == table && self.list.phase == ListPhase::Waiting {
                    self.load_list();
                }
                if let Some(Pending::Fields(_, act)) = self
                    .pending
                    .take_if(|p| matches!(p, Pending::Fields(wanted, _) if *wanted == table))
                {
                    self.act(act);
                }
            }
            Reply::List(req, outcome) => self.list_reply(req, outcome),
            Reply::Record(req, outcome) => self.record_reply(req, outcome),
            Reply::Activity(req, outcome) => {
                if let Some(r) = &mut self.record {
                    r.activity.answer(req, outcome);
                }
            }
            Reply::Attachments(req, outcome) => {
                if let Some(r) = &mut self.record
                    && r.attachments.answer(req, outcome)
                {
                    r.file_cursor = 0;
                }
            }
            Reply::Slas(req, outcome) => {
                if let Some(r) = &mut self.record {
                    r.slas.answer(req, outcome);
                }
            }
            Reply::History(req, outcome) => {
                if let Some(r) = &mut self.record {
                    r.history.answer(req, outcome);
                }
            }
            Reply::Related(req, i, outcome) => {
                if let Some(r) = &mut self.record
                    && let Some((_, load)) = r.related.get_mut(i)
                    && load.answer(req, outcome)
                {
                    let last = r.related_rows().len().saturating_sub(1);
                    r.related_cursor = r.related_cursor.min(last);
                }
            }
            Reply::Progress(req, done) => {
                for overlay in &mut self.overlays {
                    if let Overlay::Review(Review {
                        state: ReviewState::Sending(r, n),
                        ..
                    }) = overlay
                        && *r == req
                    {
                        *n = done;
                    }
                }
            }
            Reply::Written(req, results) => self.written(req, results),
            Reply::Created(req, outcome) => self.created(req, outcome),
            Reply::Downloaded(req, outcome) => {
                if self.downloading == Some(req) {
                    self.downloading = None;
                    match outcome {
                        Ok(path) => {
                            self.toast(ToastKind::Good, format!("Downloaded to {}", path.display()))
                        }
                        Err(fault) => self.fail(&fault),
                    }
                }
            }
            Reply::References(req, outcome) => {
                for overlay in &mut self.overlays {
                    if let Overlay::Picker(picker) = overlay
                        && picker.found.waits_for(req)
                    {
                        picker.found.set(outcome);
                        picker.cursor = 0;
                        return;
                    }
                }
            }
            Reply::Tables(req, outcome) => {
                for overlay in &mut self.overlays {
                    if let Overlay::Palette(p) = overlay
                        && p.tables.waits_for(req)
                    {
                        p.tables.set(outcome);
                        return;
                    }
                }
            }
            Reply::Records(req, outcome) => {
                for overlay in &mut self.overlays {
                    if let Overlay::Palette(p) = overlay
                        && p.records.waits_for(req)
                    {
                        p.records.set(outcome);
                        return;
                    }
                }
            }
            Reply::Located(req, outcome) => {
                if self.pending == Some(Pending::Locate(req)) {
                    self.pending = None;
                    match outcome {
                        Ok((table, sys_id)) => self.open_record(&table, &sys_id),
                        Err(fault) => self.fail(&fault),
                    }
                }
            }
        }
    }

    /// The sections about the user can load now.
    fn me_known(&mut self) {
        if let Load::Failed(fault) = &self.me {
            let fault = fault.clone();
            if self.list.phase == ListPhase::Waiting && self.waits_for_me() {
                self.list.phase = ListPhase::Failed(fault);
            }
            return;
        }
        if self.list.phase == ListPhase::Waiting
            && let Some(id) = self.list.section.clone()
            && self.waits_for_me()
        {
            self.list = self.section_list(&id);
        }
        if self.list.phase == ListPhase::Waiting {
            self.load_list();
        }
        self.load_counts();
    }

    fn waits_for_me(&self) -> bool {
        self.list
            .section
            .as_deref()
            .and_then(Section::find)
            .is_some_and(|s| !matches!(s.filter, tables::Filter::Plain(_)))
    }

    fn locate(&mut self, text: &str) {
        let req = self.req();
        self.pending = Some(Pending::Locate(req));
        // Numbers are stored in capitals.
        let text = match sn_core::refs::prefix(text) {
            Some(_) => text.to_ascii_uppercase(),
            None => text.to_owned(),
        };
        self.call(Call::Locate { req, text });
    }
}
