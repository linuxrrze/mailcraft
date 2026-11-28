// SPDX-FileCopyrightText: MailCraft contributors
// SPDX-License-Identifier: MIT OR Apache-2.0
//! AutoPick, in Find a Time: the meeting goes to the next (or the previous) time, as long as it
//! is, when everybody it counts is free. Whom it counts is chosen: everybody invited and the rooms
//! booked, or the people who are required alone; the organizer always. Somebody whose time isn't
//! known doesn't stand in the way, as in Outlook. The times are the half hours of the owner's
//! working hours on their working days (the whole day when the meeting is longer than those).
//!
//! The search goes one day at a time: the day in view first, after (or before) the meeting's
//! start; when that day has no such time, the meeting goes to the next working day (the one
//! before) and the search goes on there once its free and busy times are known, for
//! [`MAX_DAYS`] days at most and never back past today. What is searched is what the grid has
//! drawn: nothing is asked that the grid doesn't ask.

use mailcraft_model::ShowAs;
use mailcraft_time::{Date, Weekday};

/// The most days AutoPick moves through before it gives up.
pub(crate) const MAX_DAYS: u8 = 30;
/// How long AutoPick waits for a day's free and busy times, in milliseconds, before it gives up.
pub(crate) const WAIT_MS: f64 = 30_000.0;
/// The most stretches of taken time looked at on a day.
const MAX_TAKEN: usize = 20_000;
/// The times a meeting is put at: the half hours.
const STEP: i64 = 30;
const DAY: i64 = 24 * 60;

/// Whose time AutoPick counts.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) enum Among {
    /// Everybody invited (required and optional) and the rooms booked.
    #[default]
    Everybody,
    /// The people who are required.
    Required,
}

impl Among {
    /// Whether a row of Find a Time's grid of `kind` (1 the organizer, 2 required, 3 optional,
    /// 4 a room booked) counts.
    pub(crate) fn counts(self, kind: u8) -> bool {
        match self {
            Among::Everybody => (1..=4).contains(&kind),
            Among::Required => kind == 1 || kind == 2,
        }
    }

    /// As `ui.eventWindow` names it.
    pub(crate) fn code(self) -> &'static str {
        match self {
            Among::Everybody => "everybody",
            Among::Required => "required",
        }
    }

    pub(crate) fn parse(text: &str) -> Option<Among> {
        match text {
            "everybody" => Some(Among::Everybody),
            "required" => Some(Among::Required),
            _ => None,
        }
    }

    /// What the choice says.
    pub(crate) fn label(self) -> String {
        match self {
            Among::Everybody => tl!("All people and rooms").to_string(),
            Among::Required => tl!("Required people").to_string(),
        }
    }
}

/// A search under way: which way, the day it is to look at (the one the meeting is on), the
/// minute of that day it looks after (or before; `None`: the whole day), how many more days it
/// may move, and since when it waits for the day's times (milliseconds).
#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) struct Pick {
    pub forward: bool,
    pub day: Date,
    pub after: Option<i64>,
    pub left: u8,
    pub since: f64,
}

/// The working hours: the days, and the start and end in minutes of the day.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Hours {
    pub days: Vec<Weekday>,
    pub start: i64,
    pub end: i64,
}

impl Hours {
    /// The stretch of `day` a meeting `length` minutes long is put in: the working hours, or the
    /// whole day when they are too short for it (or there are none).
    pub(crate) fn of(&self, length: i64) -> (i64, i64) {
        let (start, end) = (self.start.clamp(0, DAY), self.end.clamp(0, DAY));
        if end.saturating_sub(start) >= length { (start, end) } else { (0, DAY) }
    }

    /// The working day after `day` (before it, `forward` false): the next day when no day of the
    /// week is a working day.
    pub(crate) fn next_day(&self, day: Date, forward: bool) -> Date {
        let step = if forward { 1 } else { -1 };
        let mut next = day.add_days(step);
        for _ in 0..7 {
            if self.days.is_empty() || self.days.contains(&next.weekday()) {
                break;
            }
            next = next.add_days(step);
        }
        next
    }

    /// Where the meeting is put on a day it moves to, before that day is searched: the first half
    /// hour it fits in going forward, the last going back.
    pub(crate) fn first_place(&self, length: i64, forward: bool) -> i64 {
        let length = length.clamp(1, DAY);
        let (from, to) = self.of(length);
        let starts = starts(from, to, length);
        if forward { starts.first().copied().unwrap_or(from) } else { starts.last().copied().unwrap_or(from) }
    }
}

/// The half hours a meeting `length` minutes long may start at between `from` and `to`.
fn starts(from: i64, to: i64, length: i64) -> Vec<i64> {
    let first = from.saturating_add(STEP - 1).div_euclid(STEP).saturating_mul(STEP);
    (0..=DAY / STEP).map(|n| first.saturating_add(n * STEP)).take_while(|start| start.saturating_add(length) <= to).collect()
}

/// The time taken, in minutes of the day, of the days counted: what isn't free.
pub(crate) fn taken<'a>(days: impl Iterator<Item = &'a [(u32, u32, ShowAs)]>) -> Vec<(i64, i64)> {
    days.flat_map(|busy| busy.iter())
        .filter(|(from, to, show_as)| *show_as != ShowAs::Free && to > from)
        .map(|(from, to, _)| (i64::from(*from), i64::from(*to)))
        .take(MAX_TAKEN)
        .collect()
}

/// The first start, in minutes of the day, of a meeting `length` minutes long within `within`
/// that overlaps none of `taken`: after the minute `after` (before it, `forward` false; `None`:
/// anywhere in the day), the nearest to it.
pub(crate) fn first_free(taken: &[(i64, i64)], within: (i64, i64), length: i64, after: Option<i64>, forward: bool) -> Option<i64> {
    let length = length.clamp(1, DAY);
    let free = |start: i64| {
        let end = start.saturating_add(length);
        !taken.iter().any(|(from, to)| *from < end && *to > start)
    };
    let starts = starts(within.0.clamp(0, DAY), within.1.clamp(0, DAY), length);
    let wanted = |start: &i64| after.is_none_or(|after| if forward { *start > after } else { *start < after });
    if forward {
        starts.into_iter().filter(wanted).find(|start| free(*start))
    } else {
        starts.into_iter().rev().filter(wanted).find(|start| free(*start))
    }
}

#[cfg(test)]
#[path = "autopick_tests.rs"]
mod tests;

#[cfg(test)]
#[path = "autopick_window_tests.rs"]
mod window_tests;
