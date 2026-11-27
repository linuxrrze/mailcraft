// SPDX-FileCopyrightText: MailCraft contributors
// SPDX-License-Identifier: MIT OR Apache-2.0
//! Rooms booked for a meeting (Room Finder): `rooms` beside `attendees` and `optional`, invited as
//! rooms, and kept when only the people change (and the other way round).

use mailcraft_model::Id;
use serde_json::{Value, json};

use crate::{Session, sample};

fn session() -> Session {
    let now = 1_791_993_600;
    Session::at(sample::mailbox(now, 0), now, 0)
}

/// Who is invited to `id`: (address, optional, room).
fn invited(s: &Session, id: Id) -> Vec<(String, bool, bool)> {
    s.mailbox.event(id).unwrap().attendees.iter().map(|a| (a.address.email.clone(), a.optional, a.resource)).collect()
}

fn new_meeting(s: &mut Session) -> Id {
    let made = s
        .run(
            "calendar.new",
            &json!({"title": "Planning", "start": "2026-10-20T10:00", "attendees": "cy@example.org", "optional": "di@example.org", "rooms": "Room 1.01 <room-101@example.org>"}),
        )
        .unwrap();
    made["id"].as_u64().unwrap()
}

#[test]
fn a_room_is_booked_as_one_of_the_meetings_rooms() {
    let mut s = session();
    let id = new_meeting(&mut s);
    assert_eq!(
        invited(&s, id),
        [("cy@example.org".into(), false, false), ("di@example.org".into(), true, false), ("room-101@example.org".into(), false, true)]
    );
    let shown = s.run("calendar.get", &json!({"id": id})).unwrap();
    let rooms: Vec<&Value> = shown["attendees"].as_array().unwrap().iter().filter(|a| a["room"] == json!(true)).collect();
    assert_eq!(rooms.len(), 1);
    assert_eq!(rooms[0]["address"], "Room 1.01 <room-101@example.org>");
}

/// Changing only the people keeps the rooms booked, and changing only the rooms keeps the people.
#[test]
fn what_of_the_people_and_the_rooms_isnt_given_stays() {
    let mut s = session();
    let id = new_meeting(&mut s);
    s.run("calendar.edit", &json!({"id": id, "attendees": "cy@example.org; ed@example.org", "optional": ""})).unwrap();
    assert_eq!(
        invited(&s, id),
        [("cy@example.org".into(), false, false), ("ed@example.org".into(), false, false), ("room-101@example.org".into(), false, true)]
    );
    s.run("calendar.edit", &json!({"id": id, "rooms": "room-102@example.org"})).unwrap();
    assert_eq!(
        invited(&s, id),
        [("cy@example.org".into(), false, false), ("ed@example.org".into(), false, false), ("room-102@example.org".into(), false, true)]
    );
    s.run("calendar.edit", &json!({"id": id, "rooms": ""})).unwrap();
    assert!(invited(&s, id).iter().all(|(_, _, room)| !room));
    assert!(s.run("calendar.edit", &json!({"id": id, "rooms": "not a room"})).is_err(), "a room is named by its address");
}
