// SPDX-FileCopyrightText: MailCraft contributors
// SPDX-License-Identifier: MIT OR Apache-2.0
//! Room Finder's rooms, the host played by a stand-in that answers as told: what is asked, with
//! what password, what is kept of the answers, and what is said without a password.

use std::sync::mpsc::{self, Sender};
use std::sync::{Arc, Mutex};

use mailcraft_engine::Session;
use mailcraft_model::{Address, Id, Mailbox};
use mailcraft_time::Date;
use serde_json::json;

use super::*;
use crate::Services;

const NOW: i64 = 1_791_993_600;
const URL: &str = "https://mail.example.org/EWS/Exchange.asmx";

type Asked = Arc<Mutex<Vec<(EwsRoomsJob, Sender<EwsRoomsResult>)>>>;

/// A window with an Exchange server account whose calendar a meeting is in, and a host for rooms
/// that keeps each question for the test to answer.
fn window() -> (MailApp, Id, Id, Asked) {
    let mut app = MailApp::new(Session::at(Mailbox::with_local_account("", ""), NOW, 0), Services::default());
    app.autosave = false;
    app.session.driver.set_auto_sync(false);
    let asked: Asked = Arc::default();
    let keep = asked.clone();
    app.services.ews_rooms = Some(Box::new(move |job| {
        let (tx, rx) = mpsc::channel();
        keep.lock().unwrap().push((job, tx));
        rx
    }));
    let added = app.execute("account.add", json!({"email": "ada@example.org", "kind": "exchange", "ews": {"url": URL}})).unwrap();
    let account = added["id"].as_u64().unwrap();
    let id = app.session.mailbox.calendars.iter().map(|c| c.id).max().unwrap_or(0) + 1_000;
    app.session.mailbox.calendars.push(mailcraft_model::Calendar {
        id,
        name: "Calendar".into(),
        visible: true,
        origin: mailcraft_model::remote::Origin { account, ..Default::default() },
        ..Default::default()
    });
    (app, account, id, asked)
}

fn answer(app: &mut MailApp, asked: &Asked, found: Vec<Address>) -> EwsRoomsJob {
    let (job, tx) = asked.lock().unwrap().pop().expect("asked");
    tx.send(EwsRoomsResult { account: job.request.account, list: job.request.list.clone(), found, ..Default::default() }).unwrap();
    app.poll_rooms();
    job
}

#[test]
fn the_lists_and_then_the_rooms_of_one_are_asked_once_each() {
    let (mut app, account, calendar, asked) = window();
    app.passwords.insert(account, "an invented password".into());
    assert_eq!(account_of(&app, calendar), Some(account));
    assert!(app.room_lists(account).is_none());
    assert!(app.rooms_pending());
    assert!(app.room_lists(account).is_none(), "one question at a time");
    let job = answer(&mut app, &asked, vec![Address::new("Building 1", "building-1@example.org")]);
    assert_eq!((job.request.list.as_str(), job.request.url.as_str(), job.secret.as_str()), ("", URL, "an invented password"));
    assert_eq!(app.room_lists(account), Some(Ok(vec![Address::new("Building 1", "building-1@example.org")])));
    assert!(asked.lock().unwrap().is_empty(), "known: not asked again");

    assert!(app.rooms_of(account, "building-1@example.org").is_none());
    let rooms = vec![Address::new("Room 1.01", "room-101@example.org"), Address::new("Room 1.02", "room-102@example.org")];
    let job = answer(&mut app, &asked, rooms.clone());
    assert_eq!(job.request.list, "building-1@example.org");
    assert_eq!(app.rooms_of(account, "building-1@example.org"), Some(Ok(rooms)));

    // Their time is asked about with the people's, as far as one question takes them.
    let meeting = crate::scheduling::Meeting {
        calendar,
        event: None,
        required: Vec::new(),
        optional: Vec::new(),
        rooms: Vec::new(),
        day: Date::new(2026, 10, 14).unwrap(),
        span: None,
        all_day: false,
    };
    assert_eq!(candidates(&mut app, &meeting, "building-1@example.org", 20), ["room-101@example.org", "room-102@example.org"]);
    assert_eq!(candidates(&mut app, &meeting, "building-1@example.org", 1), ["room-101@example.org"]);
    assert!(candidates(&mut app, &meeting, "", 20).is_empty());
}

/// Nothing is asked for: without a password at hand, Room Finder says to sign in.
#[test]
fn without_a_password_nothing_is_asked_and_it_says_to_sign_in() {
    let (mut app, account, _, asked) = window();
    assert_eq!(app.room_lists(account), Some(Err("Sign in with Send/Receive to see the rooms.".to_owned())));
    assert!(asked.lock().unwrap().is_empty());
}

/// What the server said instead of the rooms is kept and shown; a password it turned down is
/// forgotten.
#[test]
fn a_refusal_is_said_and_a_password_turned_down_forgotten() {
    let (mut app, account, _, asked) = window();
    app.passwords.insert(account, "an invented password".into());
    assert!(app.room_lists(account).is_none());
    let (job, tx) = asked.lock().unwrap().pop().unwrap();
    tx.send(EwsRoomsResult {
        account: job.request.account,
        error: "Couldn't ask for the rooms: the server turned the password down".into(),
        password_refused: true,
        ..Default::default()
    })
    .unwrap();
    app.poll_rooms();
    assert_eq!(app.room_lists(account), Some(Err("Couldn't ask for the rooms: the server turned the password down".to_owned())));
    assert!(app.ews_secret(account).is_none());
}

/// A calendar on this computer, or a Microsoft account's, has no Room Finder here.
#[test]
fn only_an_exchange_server_accounts_calendar_has_room_finder() {
    let (app, _, _, _) = window();
    let local = app.session.mailbox.calendars.iter().find(|c| c.origin.account == 0).map(|c| c.id);
    if let Some(local) = local {
        assert_eq!(account_of(&app, local), None);
    }
}
