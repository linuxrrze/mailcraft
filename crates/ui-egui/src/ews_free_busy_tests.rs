// SPDX-FileCopyrightText: MailCraft contributors
// SPDX-License-Identifier: MIT OR Apache-2.0
//! Find a Time for a meeting in an Exchange server account's calendar, the host played by a
//! stand-in that answers as told: what is asked of the server and with what password, what is
//! shown of the answer, and a password the server turns down.

use std::sync::mpsc::{self, Sender};
use std::sync::{Arc, Mutex};

use mailcraft_engine::Session;
use mailcraft_model::remote::{BusyTime, FreeBusy, PersonSchedule};
use mailcraft_model::{Id, Mailbox, ShowAs};
use mailcraft_time::Date;
use serde_json::json;

use super::*;
use crate::Services;

/// Wednesday 2026-10-14 16:00 UTC, and the midnight that day began with.
const NOW: i64 = 1_791_993_600;
const MIDNIGHT: i64 = NOW - 16 * 3600;
const URL: &str = "https://mail.example.org/EWS/Exchange.asmx";
const PASSWORD: &str = "an invented password";

type Asked = Arc<Mutex<Vec<(EwsFreeBusyJob, Sender<FreeBusyResult>)>>>;

/// A window with an Exchange server account whose calendar a meeting is in, and a host that keeps
/// each question for the test to answer. The account, the calendar and the questions.
fn window() -> (MailApp, Id, Id, Asked) {
    let mut app = MailApp::new(Session::at(Mailbox::with_local_account("", ""), NOW, 0), Services::default());
    app.autosave = false;
    app.session.driver.set_auto_sync(false);
    let asked: Asked = Arc::default();
    let keep = asked.clone();
    app.services.ews_free_busy = Some(Box::new(move |job| {
        let (tx, rx) = mpsc::channel();
        keep.lock().unwrap().push((job, tx));
        rx
    }));
    let added =
        app.execute("account.add", json!({"email": "ada@example.org", "kind": "exchange", "ews": {"url": URL, "username": "CORP\\ada"}})).unwrap();
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

fn day() -> Date {
    Date::new(2026, 10, 14).unwrap()
}

fn people() -> Vec<String> {
    vec!["maya@lumen.example".into(), "tomas@lumen.example".into()]
}

#[test]
fn the_exchange_server_is_asked_with_the_password_at_hand_and_its_answer_is_shown() {
    let (mut app, account, calendar, asked) = window();
    app.passwords.insert(account, PASSWORD.into());
    let look = app.free_busy_at(calendar, &people(), day(), 0.0);
    assert!(look.pending, "{}", look.note);
    let (job, tx) = asked.lock().unwrap().pop().expect("asked");
    assert_eq!((job.request.account, job.request.url.as_str(), job.secret.as_str()), (account, URL, PASSWORD));
    assert_eq!(job.request.ask.people, people());
    assert_eq!((job.request.ask.from, job.request.ask.to), (MIDNIGHT, MIDNIGHT + 86_400));
    // Asked once: still pending, not asked again.
    assert!(app.free_busy_at(calendar, &people(), day(), 1.0).pending);
    assert!(asked.lock().unwrap().is_empty());

    let schedule = FreeBusy {
        from: MIDNIGHT,
        to: MIDNIGHT + 86_400,
        people: vec![
            PersonSchedule {
                email: "maya@lumen.example".into(),
                busy: vec![BusyTime { start: MIDNIGHT + 9 * 3600, end: MIDNIGHT + 10 * 3600, show_as: ShowAs::Busy }],
                error: String::new(),
            },
            PersonSchedule {
                email: "tomas@lumen.example".into(),
                busy: Vec::new(),
                error: "their time couldn't be looked up (ErrorNoFreeBusyAccess)".into(),
            },
        ],
    };
    tx.send(FreeBusyResult { account, schedule: Some(schedule), ..Default::default() }).unwrap();
    app.poll_free_busy();
    let look = app.free_busy_at(calendar, &people(), day(), 2.0);
    assert!(!look.pending && look.note.is_empty(), "{}", look.note);
    let of = |email: &str| look.people.iter().find(|(who, _)| who == email).map(|(_, day)| day.clone()).unwrap();
    assert_eq!(of("maya@lumen.example").busy, [(9 * 60, 10 * 60, ShowAs::Busy)]);
    assert_eq!(of("tomas@lumen.example").unknown, "their time couldn't be looked up (ErrorNoFreeBusyAccess)");
}

/// Nothing is asked for: without a password at hand Find a Time says to sign in, and asks nobody.
#[test]
fn without_a_password_nothing_is_asked_and_the_page_says_to_sign_in() {
    let (mut app, _, calendar, asked) = window();
    let look = app.free_busy_at(calendar, &people(), day(), 0.0);
    assert!(!look.pending);
    assert_eq!(look.note, "Sign in with Send/Receive to see when other people are busy.");
    assert!(asked.lock().unwrap().is_empty());
}

/// A password the server turns down is forgotten, as a Send/Receive forgets it: the next look says
/// to sign in again.
#[test]
fn a_password_turned_down_is_forgotten() {
    let (mut app, account, calendar, asked) = window();
    app.passwords.insert(account, PASSWORD.into());
    assert!(app.free_busy_at(calendar, &people(), day(), 0.0).pending);
    let (_, tx) = asked.lock().unwrap().pop().expect("asked");
    tx.send(FreeBusyResult {
        account,
        password_refused: true,
        error: "Couldn't see when people are free: the server turned the password down".into(),
        ..Default::default()
    })
    .unwrap();
    app.poll_free_busy();
    assert!(app.ews_secret(account).is_none(), "forgotten");
    let look = app.free_busy_at(calendar, &people(), day(), 1.0);
    assert_eq!(look.note, "Couldn't see when people are free: the server turned the password down");
    let again = app.free_busy_at(calendar, &people(), day(), 1.0 + mailcraft_engine::sync_driver::free_busy::RETRY_MS);
    assert_eq!(again.note, "Sign in with Send/Receive to see when other people are busy.");
}

/// A build with no host for an Exchange server's question (the browser) shows the owner's own
/// calendar and says why.
#[test]
fn without_a_host_the_page_says_why() {
    let (mut app, account, calendar, _) = window();
    app.passwords.insert(account, PASSWORD.into());
    app.services.ews_free_busy = None;
    let look = app.free_busy_at(calendar, &people(), day(), 0.0);
    assert!(!look.pending);
    assert_eq!(look.note, "This build can't ask a server when other people are busy: only your own calendar is shown.");
}
