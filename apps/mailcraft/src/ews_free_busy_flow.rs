// SPDX-FileCopyrightText: MailCraft contributors
// SPDX-License-Identifier: MIT OR Apache-2.0
//! Find a Time with an Exchange server account the whole way: the host's own runner
//! (`ews_runner::run_free_busy`) and `mailcraft_net::https`, to a loopback server that answers with
//! the strict stand-in, which holds `GetUserAvailability` to its schema. Nothing here leaves the
//! machine; every name and address is invented.

use mailcraft_ews::standin_availability::Schedule;
use mailcraft_model::ShowAs;
use mailcraft_model::remote::FreeBusyAsk;
use mailcraft_model::sync::EwsFreeBusyRequest;

use crate::ews_mail_flow::{ME, PASSWORD, USER, serve};

/// 2026-10-12T00:00:00Z.
const DAY: i64 = 1_791_763_200;
const HOUR: i64 = 3_600;

fn request(url: &str, people: &[&str]) -> EwsFreeBusyRequest {
    EwsFreeBusyRequest {
        account: 7,
        email: ME.into(),
        url: url.into(),
        username: USER.into(),
        trusted: String::new(),
        ask: FreeBusyAsk { people: people.iter().map(|p| (*p).to_owned()).collect(), from: DAY, to: DAY + 86_400 },
    }
}

#[test]
fn other_peoples_time_comes_from_the_server_and_nothing_else_is_asked() {
    let server = serve();
    server.standin.state().schedules = vec![
        Schedule {
            email: "ben@example.org".into(),
            busy: vec![("2026-10-12T09:00:00".into(), "2026-10-12T10:00:00".into(), "Tentative".into())],
            denied: false,
        },
        Schedule { email: "dee@example.org".into(), busy: Vec::new(), denied: true },
    ];
    let result = crate::ews_runner::run_free_busy_with(&request(&server.url, &["ben@example.org", "dee@example.org"]), PASSWORD);
    assert!(result.error.is_empty(), "{}", result.error);
    assert!(!result.password_refused);
    assert_eq!(result.account, 7);
    let schedule = result.schedule.expect("an answer");
    let said: Vec<_> = schedule
        .people
        .iter()
        .map(|p| (p.email.as_str(), p.busy.iter().map(|t| (t.start - DAY, t.end - DAY, t.show_as)).collect::<Vec<_>>(), p.error.as_str()))
        .collect();
    assert_eq!(
        said,
        [
            ("ben@example.org", vec![(9 * HOUR, 10 * HOUR, ShowAs::Tentative)], ""),
            ("dee@example.org", Vec::new(), "their time couldn't be looked up (ErrorNoFreeBusyAccess)"),
        ]
    );
    // One question (after the sign-in's first refusal), and it read nothing else.
    let state = server.standin.state();
    let operations: Vec<&str> = state.requests.iter().filter(|r| r.status == 200).map(|r| r.operation.as_str()).collect();
    assert_eq!(operations, ["GetUserAvailabilityRequest"]);
    assert_eq!(state.availability_asked, 1);
}

#[test]
fn a_password_the_server_turns_down_is_said_and_marked() {
    let server = serve();
    let result = crate::ews_runner::run_free_busy_with(&request(&server.url, &["ben@example.org"]), "not the password");
    assert!(result.schedule.is_none());
    assert!(result.password_refused);
    assert_eq!(result.error, "Couldn't see when people are free: the server turned the password down");
}

/// Room Finder the whole way: the lists of rooms, then the rooms of one, from the stand-in.
#[test]
fn the_rooms_come_from_the_server_list_by_list() {
    use mailcraft_ews::standin_rooms::RoomList;
    use mailcraft_model::sync::EwsRoomsRequest;
    let server = serve();
    server.standin.state().room_lists = vec![RoomList {
        name: "Building 1".into(),
        email: "building-1@example.org".into(),
        rooms: vec![("Room 1.01".into(), "room-101@example.org".into())],
    }];
    let ask = |list: &str| EwsRoomsRequest {
        account: 7,
        email: ME.into(),
        url: server.url.clone(),
        username: USER.into(),
        trusted: String::new(),
        list: list.into(),
    };
    let lists = crate::ews_runner::run_rooms_with(&ask(""), PASSWORD);
    assert!(lists.error.is_empty(), "{}", lists.error);
    assert_eq!((lists.account, lists.found.len()), (7, 1));
    assert_eq!(lists.found[0].email, "building-1@example.org");
    let rooms = crate::ews_runner::run_rooms_with(&ask("building-1@example.org"), PASSWORD);
    assert_eq!(rooms.list, "building-1@example.org");
    let said: Vec<(&str, &str)> = rooms.found.iter().map(|a| (a.name.as_str(), a.email.as_str())).collect();
    assert_eq!(said, [("Room 1.01", "room-101@example.org")]);
    let refused = crate::ews_runner::run_rooms_with(&ask(""), "not the password");
    assert!(refused.password_refused && refused.found.is_empty());
    assert_eq!(refused.error, "Couldn't ask for the rooms: the server turned the password down");
}
