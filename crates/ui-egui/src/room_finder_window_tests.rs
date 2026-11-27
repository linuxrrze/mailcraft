// SPDX-FileCopyrightText: MailCraft contributors
// SPDX-License-Identifier: MIT OR Apache-2.0
//! Room Finder in the window: a meeting in an Exchange server account's calendar, its Find a Time
//! page, a building chosen, its rooms with whether each is free, and one booked by its button. The
//! host is a stand-in that answers at once.

use egui_kittest::kittest::Queryable as _;
use mailcraft_engine::Session;
use mailcraft_model::remote::{BusyTime, FreeBusy, PersonSchedule};
use mailcraft_model::{Address, ShowAs};
use serde_json::{Value, json};

use crate::scheduling::{EwsFreeBusyJob, FreeBusyResult};
use crate::{MailApp, Services};

/// Wednesday 2026-10-14 16:00 UTC.
const NOW: i64 = 1_791_993_600;

type Harness = egui_kittest::Harness<'static, MailApp>;

/// The sample mailbox with an Exchange server account whose calendar has come down, a password
/// at hand, and a host for rooms and free and busy times: the North building's four rooms, Room
/// 1.02 busy from 10:00 to 11:00 UTC. The calendar's id.
fn app() -> (MailApp, u64) {
    let mut app = MailApp::new(Session::at(mailcraft_engine::sample::mailbox(NOW, 0), NOW, 0), Services::default());
    app.autosave = false;
    app.session.driver.set_auto_sync(false);
    let added = app.execute(
        "account.add",
        json!({"email": "ada@lumen.example", "kind": "exchange", "ews": {"url": "https://mail.lumen.example/EWS/Exchange.asmx"}}),
    );
    let account = added.unwrap()["id"].as_u64().unwrap();
    app.passwords.insert(account, "an invented password".into());
    let id = app.session.mailbox.calendars.iter().map(|c| c.id).max().unwrap_or(0) + 1_000;
    app.session.mailbox.calendars.push(mailcraft_model::Calendar {
        id,
        name: "Calendar".into(),
        visible: true,
        origin: mailcraft_model::remote::Origin { account, ..Default::default() },
        ..Default::default()
    });
    app.services.ews_rooms = Some(Box::new(|job: super::EwsRoomsJob| {
        let (tx, rx) = std::sync::mpsc::channel();
        let found = if job.request.list.is_empty() {
            vec![Address::new("North Building", "north-rooms@lumen.example"), Address::new("South Building", "south-rooms@lumen.example")]
        } else {
            (1..=4).map(|n| Address::new(format!("Room 1.0{n}"), format!("room-10{n}@lumen.example"))).collect()
        };
        let _ = tx.send(mailcraft_model::sync::EwsRoomsResult { account: job.request.account, list: job.request.list, found, ..Default::default() });
        rx
    }));
    app.services.ews_free_busy = Some(Box::new(|job: EwsFreeBusyJob| {
        let (tx, rx) = std::sync::mpsc::channel();
        let ask = job.request.ask;
        let people = ask
            .people
            .iter()
            .map(|email| PersonSchedule {
                email: email.clone(),
                busy: if email.starts_with("room-102") {
                    vec![BusyTime { start: ask.from + 10 * 3_600, end: ask.from + 11 * 3_600, show_as: ShowAs::Busy }]
                } else {
                    Vec::new()
                },
                error: String::new(),
            })
            .collect();
        let schedule = FreeBusy { from: ask.from, to: ask.to, people };
        let _ = tx.send(FreeBusyResult { account: job.request.account, schedule: Some(schedule), ..Default::default() });
        rx
    }));
    (app, id)
}

fn window(app: MailApp) -> Harness {
    let mut harness = egui_kittest::Harness::builder()
        .with_accessibility_check(false)
        .allow_missing_glyphs()
        .with_size(egui::vec2(1280.0, 900.0))
        .with_step_dt(1.0 / 60.0)
        .build_ui_state(
            |ui, app: &mut MailApp| {
                let ctx = ui.ctx().clone();
                app.logic(&ctx);
                app.ui(ui);
            },
            app,
        );
    harness.run_steps(3);
    harness
}

fn run(harness: &mut Harness, command: &str, params: Value) -> Value {
    let answer = harness.state_mut().execute(command, params).unwrap_or_else(|e| panic!("{command}: {e}"));
    harness.run_steps(6);
    answer
}

/// Whether the window shows `text`: as a label, or as the text of a label (its value).
fn says(harness: &Harness, text: &str) -> bool {
    harness.query_all_by_label_contains(text).next().is_some() || harness.query_all_by_value(text).next().is_some()
}

#[test]
fn a_room_is_found_free_and_booked_from_find_a_time() {
    let (app, calendar) = app();
    let mut harness = window(app);
    run(
        &mut harness,
        "ui.openWindow",
        json!({"new": "meeting", "title": "Planning", "calendar": calendar, "start": "2026-10-15T10:00", "end": "2026-10-15T10:30", "attendees": "cy@lumen.example"}),
    );
    assert_eq!(run(&mut harness, "ui.eventWindow", json!({"page": "scheduling"}))["page"], "scheduling");
    assert!(says(&harness, "Room Finder"));
    // (A list to choose from shows what is chosen as its value.)
    assert!(harness.query_all_by_value("Choose a building…").next().is_some(), "the lists came, none chosen yet");

    run(&mut harness, "ui.eventWindow", json!({"roomList": "north-rooms@lumen.example"}));
    harness.run_steps(6);
    for n in 1..=4 {
        assert!(says(&harness, &format!("Room 1.0{n}")), "room {n} listed");
    }
    // Room 1.02 is busy at ten (and the legend says Busy too); the others are free.
    assert_eq!(harness.query_all_by_label("Busy").count(), 2);
    assert_eq!(harness.query_all_by_label("Free").count(), 3);

    // Booked by its button: a room of the meeting, with a row of its own, and the meeting's place.
    let rows_before = run(&mut harness, "ui.eventWindow", json!({}))["scheduling"]["rows"].as_u64().unwrap();
    harness.get_all_by_label("Book").next().expect("a Book button").click();
    harness.run_steps(6);
    let state = run(&mut harness, "ui.eventWindow", json!({}));
    assert_eq!(state["rooms"], json!(["room-101@lumen.example"]));
    assert_eq!(state["scheduling"]["rows"].as_u64().unwrap(), rows_before + 1);
    assert_eq!(harness.query_all_by_label("Booked").count(), 1);
    run(&mut harness, "ui.eventWindow", json!({"page": "appointment"}));
    assert!(harness.query_all_by_role(egui::accesskit::Role::TextInput).any(|n| n.value().as_deref() == Some("Room 1.01")), "its place");
    // A Rooms line on the Appointment page, with the room booked in it.
    let rooms_line = |n: &egui_kittest::Node<'_>| n.value().is_some_and(|value| value.contains("room-101@lumen.example"));
    assert!(harness.query_all_by_role(egui::accesskit::Role::TextInput).any(|n| rooms_line(&n)), "a Rooms line on the Appointment page");
}

/// A meeting in a calendar on this computer has no Room Finder.
#[test]
fn a_meeting_on_this_computer_has_no_room_finder() {
    let (app, _) = app();
    let mut harness = window(app);
    let local = harness.state().session.mailbox.calendars.iter().find(|c| c.origin.is_local()).map(|c| c.id).unwrap();
    run(
        &mut harness,
        "ui.openWindow",
        json!({"new": "meeting", "title": "Here", "calendar": local, "start": "2026-10-15T10:00", "attendees": "cy@lumen.example"}),
    );
    run(&mut harness, "ui.eventWindow", json!({"page": "scheduling"}));
    assert!(!says(&harness, "Room Finder"));
}
