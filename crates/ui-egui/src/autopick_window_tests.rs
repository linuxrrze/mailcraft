// SPDX-FileCopyrightText: MailCraft contributors
// SPDX-License-Identifier: MIT OR Apache-2.0
//! AutoPick in the window: a meeting in an Exchange server account's calendar, its Find a Time
//! page, and the meeting moved by AutoPick's buttons to the times when everybody counted is free,
//! on the day in view and on the working days after it and before it. The host is a stand-in that
//! answers at once; the owner's own calendar is empty.

use egui_kittest::kittest::Queryable as _;
use mailcraft_engine::Session;
use mailcraft_model::ShowAs;
use mailcraft_model::remote::{BusyTime, FreeBusy, PersonSchedule};
use serde_json::{Value, json};

use crate::scheduling::{EwsFreeBusyJob, FreeBusyResult};
use crate::{MailApp, Services};

/// Wednesday 2026-10-14 16:00 UTC.
const NOW: i64 = 1_791_993_600;
/// Midnight UTC that began Thursday 2026-10-15, and Friday 2026-10-16.
const THURSDAY: i64 = NOW + 8 * 3_600;
const FRIDAY: i64 = THURSDAY + 86_400;

type Harness = egui_kittest::Harness<'static, MailApp>;

/// The sample mailbox without its appointments, with an Exchange server account whose calendar has
/// come down, a password at hand, and a host for free and busy times: on Thursday Cy is busy from
/// 10:00 to 12:00 and Dee (optional) from 12:00 to 13:00; on Friday Cy is out of the office all
/// day; nobody has anything on other days. The calendar's id.
fn app() -> (MailApp, u64) {
    let mut app = MailApp::new(Session::at(mailcraft_engine::sample::mailbox(NOW, 0), NOW, 0), Services::default());
    app.autosave = false;
    app.session.driver.set_auto_sync(false);
    app.session.mailbox.events.clear();
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
    app.services.ews_free_busy = Some(Box::new(|job: EwsFreeBusyJob| {
        let (tx, rx) = std::sync::mpsc::channel();
        let ask = job.request.ask;
        let taken = |from: i64, to: i64, show_as: ShowAs| BusyTime { start: from, end: to, show_as };
        let people = ask
            .people
            .iter()
            .map(|email| {
                let busy = if (ask.from..ask.to).contains(&(THURSDAY + 3_600)) && email.starts_with("cy") {
                    vec![taken(THURSDAY + 10 * 3_600, THURSDAY + 12 * 3_600, ShowAs::Busy)]
                } else if (ask.from..ask.to).contains(&(THURSDAY + 3_600)) && email.starts_with("dee") {
                    vec![taken(THURSDAY + 12 * 3_600, THURSDAY + 13 * 3_600, ShowAs::Tentative)]
                } else if (ask.from..ask.to).contains(&(FRIDAY + 3_600)) && email.starts_with("cy") {
                    vec![taken(FRIDAY, FRIDAY + 86_400, ShowAs::OutOfOffice)]
                } else {
                    Vec::new()
                };
                PersonSchedule { email: email.clone(), busy, error: String::new() }
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

fn run(harness: &mut Harness, params: Value) -> Value {
    let answer = harness.state_mut().execute("ui.eventWindow", params).unwrap_or_else(|e| panic!("ui.eventWindow: {e}"));
    harness.run_steps(6);
    answer
}

/// The meeting's time on Find a Time once AutoPick is done: its start and end.
fn settled(harness: &mut Harness) -> (String, String, Value) {
    for _ in 0..40 {
        let state = run(harness, json!({}));
        if state["scheduling"]["picking"] == json!(false) {
            let at = |key: &str| state["scheduling"][key].as_str().unwrap_or_default().to_owned();
            return (at("start"), at("end"), state);
        }
    }
    panic!("AutoPick didn't settle");
}

/// A meeting from `start` to `end` with Cy required and Dee optional, on its Find a Time page.
fn meeting(start: &str, end: &str) -> Harness {
    let (app, calendar) = app();
    let mut harness = window(app);
    harness
        .state_mut()
        .execute(
            "ui.openWindow",
            json!({"new": "meeting", "title": "Planning", "calendar": calendar, "start": start, "end": end,
                   "attendees": "cy@lumen.example", "optional": "dee@lumen.example"}),
        )
        .unwrap();
    harness.run_steps(6);
    assert_eq!(run(&mut harness, json!({"page": "scheduling"}))["page"], "scheduling");
    harness.run_steps(6);
    harness
}

#[test]
fn autopick_finds_the_next_time_everybody_is_free_and_the_one_before_for_the_required() {
    let mut harness = meeting("2026-10-15T10:00", "2026-10-15T11:00");
    assert!(harness.query_all_by_label("AutoPick Next >>").next().is_some(), "the buttons are there");
    assert_eq!(settled(&mut harness).2["scheduling"]["autoPickAmong"], "everybody");

    // Everybody: Cy until noon, then Dee (tentative) until one.
    harness.get_by_label("AutoPick Next >>").click();
    harness.run_steps(6);
    let (start, end, _) = settled(&mut harness);
    assert_eq!((start.as_str(), end.as_str()), ("2026-10-15T13:00:00", "2026-10-15T14:00:00"));

    // The required alone, back from one: half past twelve, Dee no longer counted.
    run(&mut harness, json!({"autoPickAmong": "required", "autoPick": "previous"}));
    let (start, end, state) = settled(&mut harness);
    assert_eq!((start.as_str(), end.as_str()), ("2026-10-15T12:30:00", "2026-10-15T13:30:00"));
    assert_eq!(state["scheduling"]["autoPickAmong"], "required");
    assert_eq!(state["scheduling"]["pickNote"], "");
}

#[test]
fn autopick_goes_on_to_the_next_working_day_and_back_to_today_only() {
    // Nothing for an hour after four on Thursday; Cy is away on Friday: Monday at eight.
    let mut harness = meeting("2026-10-15T16:00", "2026-10-15T17:00");
    run(&mut harness, json!({"autoPick": "next"}));
    let (start, end, _) = settled(&mut harness);
    assert_eq!((start.as_str(), end.as_str()), ("2026-10-19T08:00:00", "2026-10-19T09:00:00"));

    // Back from Monday at eight: Friday is taken, Thursday's last free hour is four.
    run(&mut harness, json!({"autoPick": "previous"}));
    let (start, _, _) = settled(&mut harness);
    assert_eq!(start, "2026-10-15T16:00:00");

    // Back from Thursday at four: the half hour before.
    run(&mut harness, json!({"autoPick": "previous"}));
    assert_eq!(settled(&mut harness).0, "2026-10-15T15:30:00");

    // From Thursday at eight, back: today (Wednesday) not before now, four o'clock.
    let mut harness = meeting("2026-10-15T08:00", "2026-10-15T09:00");
    run(&mut harness, json!({"autoPick": "previous"}));
    assert_eq!(settled(&mut harness).0, "2026-10-14T16:00:00");
    // And before that there is nothing from today on: the meeting stays, and the window says why.
    run(&mut harness, json!({"autoPick": "previous"}));
    let (start, _, state) = settled(&mut harness);
    assert_eq!(start, "2026-10-14T16:00:00");
    let why = state["scheduling"]["pickNote"].as_str().unwrap_or_default().to_owned();
    assert!(why.contains("from today on"), "{why}");
    assert!(harness.query_all_by_value(&why).next().is_some() || harness.query_all_by_label(&why).next().is_some(), "said in the window");
}

#[test]
fn autopick_is_on_find_a_time_alone_and_says_so_otherwise() {
    let mut harness = meeting("2026-10-15T10:00", "2026-10-15T11:00");
    run(&mut harness, json!({"page": "appointment"}));
    let refused = harness.state_mut().execute("ui.eventWindow", json!({"autoPick": "next"}));
    assert!(refused.is_err_and(|e| e.contains("Find a Time")));
    let wrong = harness.state_mut().execute("ui.eventWindow", json!({"autoPick": "sideways"}));
    assert!(wrong.is_err());
    let wrong = harness.state_mut().execute("ui.eventWindow", json!({"autoPickAmong": "anybody"}));
    assert!(wrong.is_err());
}
