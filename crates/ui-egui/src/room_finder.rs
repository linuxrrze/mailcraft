// SPDX-FileCopyrightText: MailCraft contributors
// SPDX-License-Identifier: MIT OR Apache-2.0
//! Room Finder, in Find a Time: for a meeting in an Exchange server account's calendar, the
//! organisation's lists of rooms (one for each building, as a rule), the rooms of the one chosen,
//! whether each is free for the meeting's time, and a button that books one. The lists and the
//! rooms come from the account's server (`GetRoomLists`, `GetRooms`) through a host
//! ([`crate::Services::ews_rooms`]), once for each account and list while MailCraft runs; whether a
//! room is free comes with everybody else's time (the rooms of the list in view are asked about
//! with the people). Asking reads; it writes nothing and tells nobody. A room booked is invited to
//! the meeting as a room, when the meeting is saved and sent.

use std::collections::HashMap;
use std::sync::mpsc::Receiver;

use egui::{RichText, Ui};
use mailcraft_model::sync::{EwsRoomsRequest, EwsRoomsResult};
use mailcraft_model::{AccountKind, Address, Id, ShowAs};

use crate::MailApp;
use crate::scheduling::{Day, Look, Meeting, address_key};
use crate::theme::Tokens;

/// Most rooms of a list that the strip shows.
const MAX_SHOWN: usize = 60;

/// What the host is asked ([`crate::Services::ews_rooms`]): the question, and the password it
/// goes with.
pub struct EwsRoomsJob {
    pub request: EwsRoomsRequest,
    /// As for a run ([`crate::ews_mail::EwsJob::secret`]). Never logged.
    pub secret: String,
}

/// What is known of the organisation's rooms, by account: the lists, and the rooms of each list
/// asked about (or why not); the question in flight.
#[derive(Default)]
pub(crate) struct Rooms {
    lists: HashMap<Id, Result<Vec<Address>, String>>,
    rooms: HashMap<(Id, String), Result<Vec<Address>, String>>,
    /// The account and list asked about (empty: the lists), the mailbox opening it was asked
    /// under, and the answer to come.
    rx: Option<(Id, String, u64, Receiver<EwsRoomsResult>)>,
}

impl Rooms {
    /// Another mailbox took the place of the open one: what was known goes with its accounts.
    pub(crate) fn forget(&mut self) {
        *self = Rooms::default();
    }
}

/// The Exchange server account whose server keeps `calendar`, when it is one: the one rooms are
/// asked of.
pub(crate) fn account_of(app: &MailApp, calendar: Id) -> Option<Id> {
    let mailbox = &app.session.mailbox;
    let keeper = mailbox.calendar(calendar).and_then(|c| mailcraft_engine::cmd::remote::keeper(mailbox, &c.origin))?;
    (keeper.kind == AccountKind::Exchange).then_some(keeper.id)
}

impl MailApp {
    /// The lists of rooms of `account`'s organisation, asked for when nothing is known yet and no
    /// other question is out. `None` while nothing is known.
    pub(crate) fn room_lists(&mut self, account: Id) -> Option<Result<Vec<Address>, String>> {
        if let Some(known) = self.rooms.lists.get(&account) {
            return Some(known.clone());
        }
        self.ask_rooms(account, "");
        // (What there is to say instead of asking was said at once.)
        self.rooms.lists.get(&account).cloned()
    }

    /// The rooms of the list at `list`, asked for as [`room_lists`](Self::room_lists) are.
    pub(crate) fn rooms_of(&mut self, account: Id, list: &str) -> Option<Result<Vec<Address>, String>> {
        if let Some(known) = self.rooms.rooms.get(&(account, list.to_owned())) {
            return Some(known.clone());
        }
        self.ask_rooms(account, list);
        self.rooms.rooms.get(&(account, list.to_owned())).cloned()
    }

    /// Whether a question about rooms is out.
    pub(crate) fn rooms_pending(&self) -> bool {
        self.rooms.rx.is_some()
    }

    /// Put the question to the host, unless another is out. Nothing is asked for: without a
    /// password at hand, what there is to say is said instead.
    fn ask_rooms(&mut self, account: Id, list: &str) {
        if self.rooms.rx.is_some() {
            return;
        }
        let said = |app: &mut MailApp, why: String| {
            if list.is_empty() {
                app.rooms.lists.insert(account, Err(why));
            } else {
                app.rooms.rooms.insert((account, list.to_owned()), Err(why));
            }
        };
        let request = match mailcraft_engine::cmd::ews_mail::ews_rooms_request(&self.session, account, list) {
            Ok(request) => request,
            Err(e) => return said(self, e.to_string()),
        };
        let Some(secret) = self.ews_secret(account) else {
            return said(self, tl!("Sign in with Send/Receive to see the rooms.").to_string());
        };
        let Some(host) = &self.services.ews_rooms else {
            return said(self, tl!("This build can't ask the Exchange server for rooms.").to_string());
        };
        let mut request = request;
        // A certificate a file trusts counts once the owner has confirmed the file (AU3-02).
        if self.unconfirmed {
            request.trusted.clear();
        }
        let rx = host(EwsRoomsJob { request, secret });
        self.rooms.rx = Some((account, list.to_owned(), self.session.openings(), rx));
    }

    /// The host answered about rooms: keep what it said. Called every frame; nothing while no
    /// question is out.
    pub(crate) fn poll_rooms(&mut self) {
        let Some((account, list, opening, rx)) = &self.rooms.rx else { return };
        let (account, list) = (*account, list.clone());
        if *opening != self.session.openings() {
            self.rooms.forget();
            return;
        }
        let result = match rx.try_recv() {
            Ok(result) => result,
            Err(std::sync::mpsc::TryRecvError::Empty) => return,
            Err(std::sync::mpsc::TryRecvError::Disconnected) => EwsRoomsResult {
                account,
                list: list.clone(),
                error: tl!("Asking for the rooms stopped unexpectedly").to_string(),
                ..Default::default()
            },
        };
        self.rooms.rx = None;
        if result.password_refused {
            self.ews_password_refused(account);
        }
        let known = if result.error.is_empty() { Ok(result.found) } else { Err(result.error) };
        if list.is_empty() {
            self.rooms.lists.insert(account, known);
        } else {
            self.rooms.rooms.insert((account, list), known);
        }
    }
}

/// The addresses of the rooms of the list chosen in `chosen` for `meeting`, as far as they are
/// known, for their time to be asked about with the people's: at most `room` of them.
pub(crate) fn candidates(app: &mut MailApp, meeting: &Meeting, chosen: &str, room: usize) -> Vec<String> {
    let Some(account) = account_of(app, meeting.calendar) else { return Vec::new() };
    if chosen.is_empty() {
        return Vec::new();
    }
    match app.rooms_of(account, chosen) {
        Some(Ok(rooms)) => rooms.iter().filter_map(|room| address_key(&room.email)).take(room).collect(),
        _ => Vec::new(),
    }
}

/// Whether a room whose day is `day` is free from `start` to `end` (minutes of the day): `None`
/// when its day isn't known.
fn free(day: Option<&Day>, span: Option<(i64, i64)>) -> Option<bool> {
    let (day, (start, end)) = (day?, span?);
    if !day.unknown.is_empty() {
        return None;
    }
    Some(!day.busy.iter().any(|(from, to, show_as)| *show_as != ShowAs::Free && i64::from(*from) < end && i64::from(*to) > start))
}

/// The strip under Find a Time's grid, for a meeting whose calendar an Exchange server account
/// keeps: the list chosen in `chosen`, its rooms, whether each is free for the meeting's time
/// `span` (minutes of the day), and Book. The room booked, when one was.
pub(crate) fn strip(
    app: &mut MailApp,
    ui: &mut Ui,
    meeting: &Meeting,
    chosen: &mut String,
    look: &Look,
    span: Option<(i64, i64)>,
) -> Option<Address> {
    let account = account_of(app, meeting.calendar)?;
    let t = Tokens::get(ui.ctx());
    if app.rooms_pending() {
        // (The host wakes the window when the answer is there; this is for a host that doesn't.)
        ui.ctx().request_repaint_after(std::time::Duration::from_millis(250));
    }
    let mut booked = None;
    ui.add_space(6.0);
    ui.horizontal(|ui| {
        ui.label(RichText::new(tl!("Room Finder")).strong());
        match app.room_lists(account) {
            None => {
                ui.label(RichText::new(tl!("Asking the server for the rooms…")).size(11.5).color(t.text_dim));
            }
            Some(Err(why)) => {
                ui.label(RichText::new(why.chars().filter(|c| !c.is_control()).take(200).collect::<String>()).size(11.5).color(t.text_dim));
            }
            Some(Ok(lists)) if lists.is_empty() => {
                ui.label(RichText::new(tl!("The organisation has no lists of rooms.")).size(11.5).color(t.text_dim));
            }
            Some(Ok(lists)) => {
                let shown = lists.iter().find(|list| list.email.eq_ignore_ascii_case(chosen)).map_or_else(
                    || tl!("Choose a building…").to_string(),
                    |list| if list.name.trim().is_empty() { list.email.clone() } else { list.name.clone() },
                );
                egui::ComboBox::from_id_salt(("room_lists", crate::forms::kit::window())).selected_text(shown).width(220.0).show_ui(ui, |ui| {
                    for list in lists.iter().take(200) {
                        let name = if list.name.trim().is_empty() { list.email.clone() } else { list.name.clone() };
                        if ui.selectable_label(chosen.eq_ignore_ascii_case(&list.email), name).clicked() {
                            chosen.clone_from(&list.email);
                        }
                    }
                });
            }
        }
    });
    if chosen.is_empty() {
        return None;
    }
    let rooms = match app.rooms_of(account, chosen) {
        None => {
            ui.label(RichText::new(tl!("Asking the server for the rooms…")).size(11.5).color(t.text_dim));
            return None;
        }
        Some(Err(why)) => {
            ui.label(RichText::new(why.chars().filter(|c| !c.is_control()).take(200).collect::<String>()).size(11.5).color(t.text_dim));
            return None;
        }
        Some(Ok(rooms)) => rooms,
    };
    if rooms.is_empty() {
        ui.label(RichText::new(tl!("This list has no rooms.")).size(11.5).color(t.text_dim));
        return None;
    }
    let booked_here: Vec<String> = meeting.rooms.iter().filter_map(|room| address_key(&room.email)).collect();
    egui::ScrollArea::vertical().id_salt(("rooms", crate::forms::kit::window())).max_height(140.0).show(ui, |ui| {
        egui::Grid::new(("rooms_grid", crate::forms::kit::window())).num_columns(3).spacing([14.0, 4.0]).show(ui, |ui| {
            for room in rooms.iter().take(MAX_SHOWN) {
                let key = address_key(&room.email);
                let name = if room.name.trim().is_empty() { room.email.clone() } else { room.name.clone() };
                ui.label(name).on_hover_text(room.email.as_str());
                let day = key.as_ref().and_then(|key| look.people.iter().find(|(email, _)| email == key)).map(|(_, day)| day);
                let (state, ink) = match free(day, span) {
                    Some(true) => (tl!("Free"), t.green),
                    Some(false) => (tl!("Busy"), t.red),
                    None if look.pending => (tl!("Asking…"), t.text_dim),
                    None => (tl!("No information"), t.text_dim),
                };
                ui.label(RichText::new(state).size(11.5).color(ink));
                let already = key.as_ref().is_some_and(|key| booked_here.contains(key));
                if ui.add_enabled(!already && key.is_some(), egui::Button::new(if already { tl!("Booked") } else { tl!("Book") })).clicked() {
                    booked = Some(room.clone());
                }
                ui.end_row();
            }
        });
    });
    if rooms.len() > MAX_SHOWN {
        ui.label(
            RichText::new(tln!(rooms.len().saturating_sub(MAX_SHOWN), "{n} more room isn't shown", "{n} more rooms aren't shown"))
                .size(11.5)
                .color(t.text_dim),
        );
    }
    booked
}

#[cfg(test)]
#[path = "room_finder_tests.rs"]
mod tests;

#[cfg(test)]
#[path = "room_finder_window_tests.rs"]
mod window_tests;
