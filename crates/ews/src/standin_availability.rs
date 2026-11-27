// SPDX-FileCopyrightText: MailCraft contributors
// SPDX-License-Identifier: MIT OR Apache-2.0
//! The stand-in's free and busy times (Find a Time): `GetUserAvailability` over
//! [`State::schedules`], as [MS-OXWSGTAV] documents it. The request is held to the schema: the
//! `TimeZone`, the `MailboxDataArray` and the `FreeBusyViewOptions` in that order, each mailbox an
//! address with a known `AttendeeType`, a window of 42 days at most, a merged interval between 5
//! and 1440 minutes, and a view the schema names. Only the UTC time zone is taken (a client of
//! this stand-in asks in UTC). Each mailbox asked for has a `FreeBusyResponse` of its own, in
//! order: someone the stand-in doesn't know is `ErrorMailRecipientNotFound`, someone who won't
//! let their time be seen `ErrorNoFreeBusyAccess`; a `Detailed` view would carry what each
//! stretch is, which the stand-in never writes.

use crate::soap::{MESSAGES, TYPES};
use crate::standin::{Check, Refusal, State, envelope, invalid};
use crate::xml::{Node, escape};

/// A person whose time the stand-in can tell.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Schedule {
    pub email: String,
    /// When their time is taken: start and end as UTC wall clock (`2026-10-12T09:00:00`), and the
    /// `BusyType` (`Busy`, `Tentative`, `OOF`, `WorkingElsewhere`, `Free`, `NoData`).
    pub busy: Vec<(String, String, String)>,
    /// They don't let the caller see their time.
    pub denied: bool,
}

/// The longest window a `FreeBusy` view is answered for, as Exchange's.
const MAX_DAYS: i64 = 42;

impl State {
    pub(crate) fn get_user_availability(&mut self, operation: &Node) -> Result<(u16, String, String), Refusal> {
        Check::is(operation, MESSAGES, "GetUserAvailabilityRequest", &[])?;
        Check::no_text(operation)?;
        let expected = [(TYPES, "TimeZone"), (MESSAGES, "MailboxDataArray"), (TYPES, "FreeBusyViewOptions")];
        if operation.children.len() != expected.len() || !operation.children.iter().zip(expected).all(|(child, (space, name))| child.is(space, name))
        {
            return Err(invalid("GetUserAvailabilityRequest is a TimeZone, a MailboxDataArray and FreeBusyViewOptions, in that order"));
        }
        let (Some(zone), Some(mailboxes), Some(options)) = (operation.children.first(), operation.children.get(1), operation.children.get(2)) else {
            return Err(invalid("GetUserAvailabilityRequest is missing a part"));
        };
        utc_only(zone)?;
        let people = mailbox_data(mailboxes)?;
        let (from, to) = view_options(options)?;
        self.availability_asked = self.availability_asked.saturating_add(1);
        let answers: String = people.iter().map(|email| self.free_busy_response(email, from, to)).collect();
        let body = format!(
            "<m:GetUserAvailabilityResponse xmlns:m=\"{MESSAGES}\" xmlns:t=\"{TYPES}\"><m:FreeBusyResponseArray>{answers}</m:FreeBusyResponseArray></m:GetUserAvailabilityResponse>"
        );
        Ok((200, "NoError".into(), envelope(&body)))
    }

    /// The answer for one mailbox: its taken time overlapping the window, or why there is none.
    fn free_busy_response(&self, email: &str, from: i64, to: i64) -> String {
        let error = |code: &str| {
            format!(
                "<m:FreeBusyResponse><m:ResponseMessage ResponseClass=\"Error\"><m:MessageText>{code}</m:MessageText>\
                 <m:ResponseCode>{code}</m:ResponseCode><m:DescriptiveLinkKey>0</m:DescriptiveLinkKey></m:ResponseMessage></m:FreeBusyResponse>"
            )
        };
        let Some(schedule) = self.schedules.iter().find(|s| s.email.eq_ignore_ascii_case(email)) else {
            return error("ErrorMailRecipientNotFound");
        };
        if schedule.denied {
            return error("ErrorNoFreeBusyAccess");
        }
        let events: String = schedule
            .busy
            .iter()
            .filter(|(start, end, _)| {
                let (start, end) = (seconds(start).unwrap_or(i64::MAX), seconds(end).unwrap_or(i64::MIN));
                start < to && end > from
            })
            .map(|(start, end, kind)| {
                format!(
                    "<t:CalendarEvent><t:StartTime>{}</t:StartTime><t:EndTime>{}</t:EndTime><t:BusyType>{}</t:BusyType></t:CalendarEvent>",
                    escape(start),
                    escape(end),
                    escape(kind)
                )
            })
            .collect();
        format!(
            "<m:FreeBusyResponse><m:ResponseMessage ResponseClass=\"Success\"><m:ResponseCode>NoError</m:ResponseCode></m:ResponseMessage>\
             <m:FreeBusyView><t:FreeBusyViewType>FreeBusy</t:FreeBusyViewType><t:CalendarEventArray>{events}</t:CalendarEventArray>\
             <t:WorkingHours><t:TimeZone><t:Bias>0</t:Bias></t:TimeZone><t:WorkingPeriodArray><t:WorkingPeriod>\
             <t:DayOfWeek>Monday Tuesday Wednesday Thursday Friday</t:DayOfWeek><t:StartTimeInMinutes>480</t:StartTimeInMinutes>\
             <t:EndTimeInMinutes>1020</t:EndTimeInMinutes></t:WorkingPeriod></t:WorkingPeriodArray></t:WorkingHours></m:FreeBusyView></m:FreeBusyResponse>"
        )
    }
}

/// A `SerializableTimeZone` that is UTC: a bias of nought, and standard and daylight time each a
/// `Bias`, `Time`, `DayOrder`, `Month` and `DayOfWeek` with no bias either.
fn utc_only(zone: &Node) -> Result<(), Refusal> {
    let parts = Check::children(zone, TYPES, &["Bias", "StandardTime", "DaylightTime"])?;
    let [Some(bias), Some(standard), Some(daylight)] = parts.as_slice() else {
        return Err(invalid("TimeZone needs a Bias, a StandardTime and a DaylightTime"));
    };
    if bias.text.trim() != "0" {
        return Err(invalid("the stand-in answers in UTC only"));
    }
    for change in [standard, daylight] {
        let parts = Check::children(change, TYPES, &["Bias", "Time", "DayOrder", "Month", "DayOfWeek"])?;
        if parts.iter().any(Option::is_none) {
            return Err(invalid(format!("{} needs a Bias, a Time, a DayOrder, a Month and a DayOfWeek", change.name)));
        }
        if parts.first().copied().flatten().is_some_and(|bias| bias.text.trim() != "0") {
            return Err(invalid("the stand-in answers in UTC only"));
        }
    }
    Ok(())
}

/// The addresses of a `MailboxDataArray`, in order: each `MailboxData` an `Email` with its
/// `Address`, an `AttendeeType` the schema names, and maybe `ExcludeConflicts`.
fn mailbox_data(list: &Node) -> Result<Vec<String>, Refusal> {
    Check::no_text(list)?;
    if list.children.is_empty() || list.children.len() > 100 {
        return Err(invalid("MailboxDataArray holds 1 to 100 mailboxes"));
    }
    let mut people = Vec::new();
    for one in &list.children {
        Check::is(one, TYPES, "MailboxData", &[])?;
        let parts = Check::children(one, TYPES, &["Email", "AttendeeType", "ExcludeConflicts"])?;
        let email = Check::required(one, parts.first().copied().flatten(), "Email")?;
        let address = Check::children(email, TYPES, &["Name", "Address", "RoutingType"])?;
        let address = Check::required(email, address.get(1).copied().flatten(), "Address")?;
        let kind = Check::required(one, parts.get(1).copied().flatten(), "AttendeeType")?;
        if !matches!(kind.text.trim(), "Organizer" | "Required" | "Optional" | "Room" | "Resource") {
            return Err(invalid("AttendeeType is not one of the schema's"));
        }
        if let Some(exclude) = parts.get(2).copied().flatten()
            && !matches!(exclude.text.trim(), "true" | "false")
        {
            return Err(invalid("ExcludeConflicts is a boolean"));
        }
        people.push(address.text.trim().to_owned());
    }
    Ok(people)
}

/// The window of `FreeBusyViewOptions` (UTC seconds), held to the schema and to 42 days.
fn view_options(options: &Node) -> Result<(i64, i64), Refusal> {
    let parts = Check::children(options, TYPES, &["TimeWindow", "MergedFreeBusyIntervalInMinutes", "RequestedView"])?;
    let window = Check::required(options, parts.first().copied().flatten(), "TimeWindow")?;
    let times = Check::children(window, TYPES, &["StartTime", "EndTime"])?;
    let start = Check::required(window, times.first().copied().flatten(), "StartTime")?;
    let end = Check::required(window, times.get(1).copied().flatten(), "EndTime")?;
    let (Some(from), Some(to)) = (seconds(&start.text), seconds(&end.text)) else {
        return Err(invalid("the TimeWindow's times are xs:dateTime"));
    };
    if to <= from {
        return Err(invalid("the TimeWindow ends after it starts"));
    }
    if to.saturating_sub(from) > MAX_DAYS * 86_400 {
        return Err(Refusal { code: "ErrorTimeIntervalTooBig", why: "a FreeBusy view is answered for 42 days at most".into() });
    }
    if let Some(interval) = parts.get(1).copied().flatten() {
        let minutes: u32 = interval.text.trim().parse().map_err(|_| invalid("MergedFreeBusyIntervalInMinutes is a number"))?;
        if !(5..=1440).contains(&minutes) {
            return Err(invalid("MergedFreeBusyIntervalInMinutes is 5 to 1440"));
        }
    }
    if let Some(view) = parts.get(2).copied().flatten()
        && !matches!(view.text.trim(), "None" | "MergedOnly" | "FreeBusy" | "FreeBusyMerged" | "Detailed" | "DetailedMerged")
    {
        return Err(invalid("RequestedView is not one of the schema's"));
    }
    Ok((from, to))
}

/// A wall-clock time as the stand-in reads it: UTC, whatever offset it names.
fn seconds(text: &str) -> Option<i64> {
    mailcraft_time::iso_to_timestamp(text.trim(), 0)
}
