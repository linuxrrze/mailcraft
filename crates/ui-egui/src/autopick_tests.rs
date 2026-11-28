// SPDX-FileCopyrightText: MailCraft contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

use mailcraft_model::ShowAs;
use mailcraft_time::{Date, Weekday};

use super::*;

fn hours() -> Hours {
    Hours { days: vec![Weekday::Mon, Weekday::Tue, Weekday::Wed, Weekday::Thu, Weekday::Fri], start: 8 * 60, end: 17 * 60 }
}

#[test]
fn the_next_free_half_hour_after_the_meeting_is_found() {
    // Taken 9:00–10:30 and 11:00–12:00; the meeting is an hour long and starts at 9:00.
    let taken = [(9 * 60, 10 * 60 + 30), (11 * 60, 12 * 60)];
    let within = hours().of(60);
    assert_eq!(first_free(&taken, within, 60, Some(9 * 60), true), Some(12 * 60));
    // Half an hour fits between the two.
    assert_eq!(first_free(&taken, within, 30, Some(9 * 60), true), Some(10 * 60 + 30));
    // Going back from noon: 8:00 is the only hour free before it.
    assert_eq!(first_free(&taken, within, 60, Some(12 * 60), false), Some(8 * 60));
    // Nothing after 16:00 for an hour within the working day.
    assert_eq!(first_free(&taken, within, 60, Some(16 * 60), true), None);
    // The whole day: the first free start.
    assert_eq!(first_free(&taken, within, 60, None, true), Some(8 * 60));
    assert_eq!(first_free(&taken, within, 60, None, false), Some(16 * 60));
}

#[test]
fn free_time_and_odd_stretches_stand_in_nobodys_way() {
    let busy = [(600u32, 660u32, ShowAs::Free), (700, 650, ShowAs::Busy), (480, 540, ShowAs::Tentative)];
    let lanes: [&[(u32, u32, ShowAs)]; 1] = [&busy];
    let taken = taken(lanes.into_iter());
    assert_eq!(taken, vec![(480, 540)]);
    assert_eq!(first_free(&taken, hours().of(60), 60, None, true), Some(540));
}

#[test]
fn a_meeting_longer_than_the_working_day_is_put_anywhere_in_the_day() {
    let h = hours();
    assert_eq!(h.of(10 * 60), (0, 24 * 60));
    assert_eq!(h.first_place(10 * 60, true), 0);
    assert_eq!(h.first_place(10 * 60, false), 14 * 60);
    assert_eq!(h.first_place(60, true), 8 * 60);
    assert_eq!(h.first_place(60, false), 16 * 60);
    // Hours that don't start on a half hour: the first half hour after.
    let odd = Hours { start: 8 * 60 + 10, ..hours() };
    assert_eq!(odd.first_place(60, true), 8 * 60 + 30);
    // Hostile lengths and hours are kept to the day.
    assert_eq!(first_free(&[], (-500, 99_999), i64::MAX, None, true), Some(0));
    assert_eq!(first_free(&[], (0, 24 * 60), -5, None, false), Some(23 * 60 + 30));
}

#[test]
fn the_next_working_day_skips_the_weekend() {
    // Friday 2026-10-16: the next is Monday; back from Monday 2026-10-19 is Friday.
    let friday = Date::new(2026, 10, 16).unwrap();
    let monday = Date::new(2026, 10, 19).unwrap();
    assert_eq!(hours().next_day(friday, true), monday);
    assert_eq!(hours().next_day(monday, false), friday);
    // No working days at all: every day.
    let none = Hours { days: Vec::new(), ..hours() };
    assert_eq!(none.next_day(friday, true), friday.add_days(1));
}

#[test]
fn whom_it_counts() {
    assert!(Among::Everybody.counts(1) && Among::Everybody.counts(3) && Among::Everybody.counts(4));
    assert!(!Among::Everybody.counts(0), "the row for everybody together is not a person");
    assert!(Among::Required.counts(1) && Among::Required.counts(2));
    assert!(!Among::Required.counts(3) && !Among::Required.counts(4));
    for among in [Among::Everybody, Among::Required] {
        assert_eq!(Among::parse(among.code()), Some(among));
    }
    assert_eq!(Among::parse("anybody"), None);
}
