// SPDX-FileCopyrightText: MailCraft contributors
// SPDX-License-Identifier: MIT OR Apache-2.0
//! Other people's free and busy time (Find a Time): the operation `GetUserAvailability` of
//! [MS-OXWSGTAV], with the `FreeBusy` view, so that the server says when each person's time is
//! taken and how (busy, tentative, out of office, working elsewhere), never by what.
//!
//! What is sent is the addresses asked about and the stretch of time. The request names the UTC
//! time zone (a bias of nought, with no daylight saving), so the times of the window and of the
//! answer are UTC wall clock. What comes back is read as hostile as everything else a server
//! says: at most [`MAX_SCHEDULES`](mailcraft_model::remote::MAX_SCHEDULES) people, [`MAX_BUSY`]
//! stretches each, every stretch cut to the window asked for; the subjects and places a `Detailed`
//! view would carry are never asked for.
//! Asking reads; it writes nothing and tells nobody.

use mailcraft_model::ShowAs;
use mailcraft_model::remote::{BusyTime, FreeBusy, FreeBusyAsk, MAX_BUSY, PersonSchedule};
use mailcraft_time::{DateTime, Timestamp};

use crate::error::{Error, ResponseClass, ResponseCode};
use crate::reply::Reply;
use crate::soap::{Element, TYPES};
use crate::xml::Node;

/// The longest stretch asked for at once: Exchange answers a `FreeBusy` view for 42 days at most
/// (its `MaximumQueryIntervalDays`), and refuses a longer window outright.
pub const MAX_WINDOW: Timestamp = 42 * 86_400;
/// The granularity of the merged view the schema requires a value for (minutes); the `FreeBusy`
/// view asked for doesn't use it, but a server checks it is within 5 and 1440.
const MERGED_INTERVAL: u32 = 30;
/// The longest response code kept in a person's error.
const MAX_CODE: usize = 64;

/// The request `GetUserAvailability` for some people over a stretch of time.
#[derive(Clone, PartialEq, Eq)]
pub struct GetUserAvailability {
    people: Vec<String>,
    from: Timestamp,
    to: Timestamp,
}

impl std::fmt::Debug for GetUserAvailability {
    // (Whose time is asked about is the owner's business: a log line says how many.)
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("GetUserAvailability").field("people", &self.people.len()).field("from", &self.from).field("to", &self.to).finish()
    }
}

impl GetUserAvailability {
    /// The question for `ask`: its addresses that are addresses, each once,
    /// [`MAX_SCHEDULES`](mailcraft_model::remote::MAX_SCHEDULES) at most; its stretch cut to
    /// [`MAX_WINDOW`]. None when nobody is left to ask about, or the stretch is empty.
    pub fn new(ask: &FreeBusyAsk) -> Option<Self> {
        let mut ask = ask.clone();
        ask.tidy();
        let to = ask.to.min(ask.from.saturating_add(MAX_WINDOW));
        (!ask.people.is_empty() && to > ask.from).then_some(Self { people: ask.people, from: ask.from, to })
    }

    /// The addresses asked about, in the order they are sent (the answer keeps it).
    pub fn people(&self) -> &[String] {
        &self.people
    }

    /// The stretch asked about: from, and up to (Unix seconds).
    pub fn window(&self) -> (Timestamp, Timestamp) {
        (self.from, self.to)
    }

    /// The operation as the schema has it: the time zone, the mailboxes (each a required
    /// attendee, conflicts not left out), and the view's options.
    pub fn element(&self) -> Element {
        let mailboxes = self.people.iter().fold(Element::new("m:MailboxDataArray"), |list, email| {
            list.child(
                Element::new("t:MailboxData")
                    .child(Element::new("t:Email").child(Element::new("t:Address").text(email)))
                    .child(Element::new("t:AttendeeType").text("Required"))
                    .child(Element::new("t:ExcludeConflicts").text("false")),
            )
        });
        let options = Element::new("t:FreeBusyViewOptions")
            .child(
                Element::new("t:TimeWindow")
                    .child(Element::new("t:StartTime").text(&wall_clock(self.from)))
                    .child(Element::new("t:EndTime").text(&wall_clock(self.to))),
            )
            .child(Element::new("t:MergedFreeBusyIntervalInMinutes").text(&MERGED_INTERVAL.to_string()))
            .child(Element::new("t:RequestedView").text("FreeBusy"));
        Element::new("m:GetUserAvailabilityRequest").child(utc_zone()).child(mailboxes).child(options)
    }

    /// What the server said of each person asked about. The answers come in the order the
    /// mailboxes were asked for; one that names no person of the question is left out. A person
    /// the server couldn't look up has no times and says why (its response code). A busy server
    /// is [`Error::Busy`] for the whole question, and an answer with no responses at all
    /// [`Error::Unexpected`].
    pub fn answer(&self, reply: &Reply) -> Result<FreeBusy, Error> {
        let responses = reply.availability()?;
        if responses.is_empty() {
            return Err(Error::Unexpected);
        }
        if let Some(busy) = responses.iter().find_map(|(message, _)| message.ok().err().filter(|error| matches!(error, Error::Busy { .. }))) {
            return Err(busy);
        }
        let mut out = FreeBusy { from: self.from, to: self.to, people: Vec::new() };
        for ((message, response), email) in responses.iter().zip(&self.people) {
            let mut person = PersonSchedule { email: email.clone(), ..Default::default() };
            if message.class() == ResponseClass::Error {
                person.error = could_not(message.code());
            } else {
                let view = response.child("FreeBusyView");
                let events = view.and_then(|view| view.child_in(TYPES, "CalendarEventArray"));
                let mut no_data = false;
                for event in events.map(|list| list.children.as_slice()).unwrap_or_default().iter().filter(|e| e.is(TYPES, "CalendarEvent")) {
                    if person.busy.len() >= MAX_BUSY {
                        break;
                    }
                    match self.busy_time(event) {
                        Some(Taken::Time(time)) => person.busy.push(time),
                        Some(Taken::NoData) => no_data = true,
                        None => {}
                    }
                }
                // The whole view, said in one word: `None` is the server's "nothing known".
                let view_type = view.and_then(|view| view.child_in(TYPES, "FreeBusyViewType")).map_or("", |kind| kind.text.trim());
                if person.busy.is_empty() && (no_data || view_type == "None") {
                    person.error = "their time couldn't be looked up (no information)".to_owned();
                }
            }
            out.people.push(person);
        }
        out.tidy();
        Ok(out)
    }

    /// One `t:CalendarEvent` of the answer: when (cut to the window asked for) and how. Free time
    /// and what doesn't read are nothing.
    fn busy_time(&self, event: &Node) -> Option<Taken> {
        let show_as = match event.child_in(TYPES, "BusyType")?.text.trim() {
            "Busy" => ShowAs::Busy,
            "Tentative" => ShowAs::Tentative,
            "OOF" => ShowAs::OutOfOffice,
            "WorkingElsewhere" => ShowAs::WorkingElsewhere,
            "NoData" => return Some(Taken::NoData),
            _ => return None,
        };
        // In the time zone of the request, UTC: a time without an offset is read as UTC.
        let at = |name: &str| mailcraft_time::iso_to_timestamp(event.child_in(TYPES, name)?.text.trim(), 0);
        let start = at("StartTime")?.max(self.from);
        let end = at("EndTime")?.min(self.to);
        (end > start).then_some(Taken::Time(BusyTime { start, end, show_as }))
    }
}

/// What one calendar event of the answer says.
enum Taken {
    Time(BusyTime),
    /// The server has nothing for that stretch.
    NoData,
}

/// Why nothing is known of a person, in the words Find a Time shows, with the server's code
/// when it is one (a short identifier: [`ResponseCode`] keeps no other).
fn could_not(code: &ResponseCode) -> String {
    let name = code.to_string();
    if !name.is_empty() && name.len() <= MAX_CODE && name.bytes().all(|b| b.is_ascii_alphanumeric()) {
        format!("their time couldn't be looked up ({name})")
    } else {
        "their time couldn't be looked up".to_owned()
    }
}

/// An instant as the request's time zone (UTC) writes it: wall clock, no offset.
fn wall_clock(at: Timestamp) -> String {
    let t = DateTime::from_timestamp(at, 0);
    format!("{}T{:02}:{:02}:{:02}", t.date.iso(), t.hour, t.minute, t.second)
}

/// The UTC time zone as [MS-OXWSGTAV] `SerializableTimeZone` says one: no bias, and standard and
/// daylight time alike (the schema requires both).
fn utc_zone() -> Element {
    let change = |name: &str| {
        Element::new(name)
            .child(Element::new("t:Bias").text("0"))
            .child(Element::new("t:Time").text("00:00:00"))
            .child(Element::new("t:DayOrder").text("1"))
            .child(Element::new("t:Month").text("1"))
            .child(Element::new("t:DayOfWeek").text("Sunday"))
    };
    Element::new("t:TimeZone").child(Element::new("t:Bias").text("0")).child(change("t:StandardTime")).child(change("t:DaylightTime"))
}
