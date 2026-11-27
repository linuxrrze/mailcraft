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
use mailcraft_time::{Date, DateTime, Timestamp, Weekday};

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

/// The working hours the owner has set in the mailbox, from an answer about the owner alone
/// (`FreeBusyView/WorkingHours` of its first response): the days and the hours of its first
/// working period, and the offset its zone has at `at`. `None` when the answer has none, or
/// what it has isn't working hours (no day, an end before the start, hours past the day).
pub fn working_hours(reply: &Reply, at: Timestamp) -> Option<mailcraft_model::sync::EwsWorkingHours> {
    let responses = reply.availability().ok()?;
    let (message, response) = responses.first()?;
    if message.class() == ResponseClass::Error {
        return None;
    }
    let hours = response.child("FreeBusyView")?.child_in(TYPES, "WorkingHours")?;
    let offset = zone_offset(hours.child_in(TYPES, "TimeZone")?, at)?;
    let period = hours.child_in(TYPES, "WorkingPeriodArray")?.children.iter().find(|period| period.is(TYPES, "WorkingPeriod"))?;
    let minutes = |name: &str| period.child_in(TYPES, name)?.text.trim().parse::<u16>().ok();
    let (start, end) = (minutes("StartTimeInMinutes")?, minutes("EndTimeInMinutes")?);
    let days = week_days(&period.child_in(TYPES, "DayOfWeek")?.text);
    (!days.is_empty() && start < end && end <= 24 * 60).then_some(mailcraft_model::sync::EwsWorkingHours { days, start, end, offset })
}

/// The days a `DayOfWeek` list names (`Monday Tuesday`), with the schema's `Day`, `Weekday` and
/// `WeekendDay` as the days they stand for; each once, in the week's order.
fn week_days(text: &str) -> Vec<Weekday> {
    use Weekday::{Fri, Mon, Sat, Sun, Thu, Tue, Wed};
    const WEEK: [Weekday; 7] = [Mon, Tue, Wed, Thu, Fri, Sat, Sun];
    let mut named = [false; 7];
    for word in text.split([' ', ',', ';']).take(32) {
        let days: &[usize] = match word.trim() {
            "Monday" => &[0],
            "Tuesday" => &[1],
            "Wednesday" => &[2],
            "Thursday" => &[3],
            "Friday" => &[4],
            "Saturday" => &[5],
            "Sunday" => &[6],
            "Weekday" => &[0, 1, 2, 3, 4],
            "WeekendDay" => &[5, 6],
            "Day" => &[0, 1, 2, 3, 4, 5, 6],
            _ => &[],
        };
        for at in days {
            if let Some(slot) = named.get_mut(*at) {
                *slot = true;
            }
        }
    }
    WEEK.iter().zip(named).filter(|(_, on)| *on).map(|(day, _)| *day).collect()
}

/// The offset from UTC (minutes east) that a `SerializableTimeZone` has at `at`: its `Bias` (UTC
/// is the wall clock plus the bias), and the standard or daylight time's own bias on top, as the
/// changes between them fall in that year (the `DayOrder`th `DayOfWeek` of the `Month`, 5 being
/// the last, at `Time` of the clock that is ending). A zone without changes (no month for either)
/// has its bias alone. `None` for what doesn't read, or an offset of more than a day.
fn zone_offset(zone: &Node, at: Timestamp) -> Option<i32> {
    let number = |node: &Node, name: &str| -> Option<i32> { node.child_in(TYPES, name)?.text.trim().parse::<i32>().ok() };
    let bias = number(zone, "Bias")?;
    let change = |name: &str| zone.child_in(TYPES, name).map(|node| (node, number(node, "Bias").unwrap_or(0)));
    let (standard, daylight) = (change("StandardTime"), change("DaylightTime"));
    let offset = |extra: i32| bias.checked_add(extra).and_then(i32::checked_neg).filter(|offset| offset.abs() <= 24 * 60);
    let (Some((standard, standard_bias)), Some((daylight, daylight_bias))) = (standard, daylight) else { return offset(0) };
    let standard_offset = offset(standard_bias)?;
    let daylight_offset = offset(daylight_bias)?;
    let year = DateTime::from_timestamp(at, standard_offset).date.year;
    // Each change, at the clock that is ending: daylight time begins by standard time's clock.
    let (Some(to_daylight), Some(to_standard)) = (change_at(daylight, year, standard_offset), change_at(standard, year, daylight_offset)) else {
        return Some(standard_offset);
    };
    let daylight_now = if to_daylight < to_standard {
        at >= to_daylight && at < to_standard
    } else {
        // (South of the equator: daylight time runs over the new year.)
        !(at >= to_standard && at < to_daylight)
    };
    Some(if daylight_now { daylight_offset } else { standard_offset })
}

/// When the change `rule` falls in `year`, by a clock `offset` minutes east of UTC. `None` for a
/// rule without a month (no change) or one that doesn't read.
fn change_at(rule: &Node, year: i32, offset: i32) -> Option<Timestamp> {
    let text = |name: &str| rule.child_in(TYPES, name).map_or("", |node| node.text.trim());
    let month: u8 = text("Month").parse().ok().filter(|month| (1..=12).contains(month))?;
    let order: u8 = text("DayOrder").parse().ok().filter(|order| (1..=5).contains(order))?;
    let weekday = week_days(text("DayOfWeek")).into_iter().next()?;
    let mut clock = text("Time").split(':').map(|part| part.parse::<u8>().ok());
    let (hour, minute, second) = (clock.next().flatten()?, clock.next().flatten().unwrap_or(0), clock.next().flatten().unwrap_or(0));
    let first = Date::new(year, month, 1)?;
    let ahead = i64::from(weekday.index()).saturating_sub(i64::from(first.weekday().index())).rem_euclid(7);
    let mut day = first.add_days(ahead.saturating_add(i64::from(order.saturating_sub(1)).saturating_mul(7)));
    // The fifth is the last: a month with four of that day has it on the fourth.
    while day.month != month {
        day = day.add_days(-7);
    }
    Some(DateTime::new(day, hour, minute, second)?.to_timestamp(offset))
}
