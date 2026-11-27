// SPDX-FileCopyrightText: MailCraft contributors
// SPDX-License-Identifier: MIT OR Apache-2.0
//! What a directory run makes of an upload and of a vCard.

use std::future::{Future, ready};
use std::sync::Mutex;

use mailcraft_model::remote::{ContactChangeKind, ContactOp, ContactsUpload, EventChangeKind, EventFields, EventOp, EventTime};
use mailcraft_model::remote_tasks::{TaskOp, TaskOpKind};
use mailcraft_model::{Importance, TaskFields, TaskPatch};

use super::*;

/// An exchange that records the one request and answers a `DeleteItem` with success.
struct Recording {
    body: Mutex<String>,
}

impl crate::http::HttpExchange for Recording {
    fn exchange(&self, request: &crate::http::HttpRequest) -> impl Future<Output = Result<crate::http::HttpResponse, crate::http::TransportError>> {
        *self.body.lock().unwrap_or_else(|poisoned| poisoned.into_inner()) = String::from_utf8_lossy(request.body()).into_owned();
        let body = crate::standin::response(
            "DeleteItem",
            "<m:DeleteItemResponseMessage ResponseClass=\"Success\"><m:ResponseCode>NoError</m:ResponseCode></m:DeleteItemResponseMessage>",
        );
        ready(Ok(crate::http::HttpResponse::new(200, body.into_bytes())))
    }
}

#[test]
fn a_contact_deletion_goes_as_a_delete_item_and_needs_no_card() {
    let recording = Recording { body: Mutex::new(String::new()) };
    let client = crate::Client::new("http://127.0.0.1/EWS/Exchange.asmx", crate::Secret::new("test")).unwrap();
    let request = mailcraft_model::sync::EwsDirectoryRequest {
        account: 1,
        email: "ada@example.org".into(),
        url: "http://127.0.0.1/EWS/Exchange.asmx".into(),
        contact_changes: Some(ContactsUpload {
            account: 1,
            email: "ada@example.org".into(),
            ops: vec![ContactOp { kind: ContactChangeKind::Delete, contact: 7, remote: "contact-id".into(), ..Default::default() }],
        }),
        ..Default::default()
    };
    let cancel = std::sync::atomic::AtomicBool::new(false);
    let result = crate::standin::block_on(sync_directory(&client, &recording, &request, &cancel, &mut |_| true)).unwrap();
    assert_eq!(result.contacts_uploaded.as_ref().unwrap().outcomes[0].result, mailcraft_model::remote::EventResult::Done);
    let body = recording.body.lock().unwrap_or_else(|poisoned| poisoned.into_inner()).clone();
    assert!(body.contains("<m:DeleteItem") && body.contains("contact-id"), "{body}");
}

struct RefusedUpdate;

impl crate::http::HttpExchange for RefusedUpdate {
    fn exchange(&self, request: &crate::http::HttpRequest) -> impl Future<Output = Result<crate::http::HttpResponse, crate::http::TransportError>> {
        assert!(String::from_utf8_lossy(request.body()).contains("<m:UpdateItem"));
        let body = crate::standin::response(
            "UpdateItem",
            r#"<m:UpdateItemResponseMessage ResponseClass="Error"><m:ResponseCode>ErrorAccessDenied</m:ResponseCode></m:UpdateItemResponseMessage>"#,
        );
        ready(Ok(crate::http::HttpResponse::new(200, body.into_bytes())))
    }
}

#[test]
fn an_event_update_refused_inside_http_success_is_denied() {
    let client = crate::Client::new("http://127.0.0.1/EWS/Exchange.asmx", crate::Secret::new("test")).unwrap();
    let request = mailcraft_model::sync::EwsDirectoryRequest {
        changes: Some(mailcraft_model::remote::EventsUpload {
            ops: vec![EventOp {
                kind: EventChangeKind::Update,
                remote: "event-id".into(),
                fields: EventFields { title: Some("New title".into()), ..Default::default() },
                ..Default::default()
            }],
            ..Default::default()
        }),
        ..Default::default()
    };
    let cancel = std::sync::atomic::AtomicBool::new(false);
    let result = crate::standin::block_on(sync_directory(&client, &RefusedUpdate, &request, &cancel, &mut |_| true)).unwrap();
    assert_eq!(result.uploaded.unwrap().outcomes[0].result, EventResult::Denied);
}

#[test]
fn an_event_to_make_has_the_fields_it_was_sent_with() {
    let op = EventOp {
        kind: EventChangeKind::Create,
        fields: EventFields {
            title: Some("Lunch".into()),
            time: Some(EventTime { start: 1_800, end: 3_600, all_day: false }),
            importance: Some(Importance::High),
            ..Default::default()
        },
        ..Default::default()
    };
    let event = event_from_fields(&op, "");
    assert_eq!(event.title, "Lunch");
    assert_eq!((event.start, event.end), (1_800, 3_600));
    assert_eq!(event.importance, Importance::High);
    assert!(!event.all_day);
}

#[test]
fn a_contact_from_a_vcard_keeps_its_people_and_places() {
    let card = "BEGIN:VCARD\r\nVERSION:3.0\r\nFN:Grace Hopper\r\nN:Hopper;Grace;;;\r\nEMAIL;TYPE=WORK:grace@example.org\r\nTEL;TYPE=CELL:+1 555 0100\r\nEND:VCARD\r\n";
    let contact = contact_from_card(card).unwrap();
    assert_eq!(contact.display_name, "Grace Hopper");
    assert_eq!(contact.last, "Hopper");
    assert_eq!(contact.emails[0].value, "grace@example.org");
    assert_eq!(contact.phones[0].value, "+1 555 0100");
}

#[test]
fn a_stand_in_answers_a_directory_read() {
    let standin = crate::standin::StandIn::new();
    let client = crate::Client::new("http://127.0.0.1/EWS/Exchange.asmx", crate::Secret::new(crate::standin::AUTHORIZATION)).unwrap();
    let request = mailcraft_model::sync::EwsDirectoryRequest {
        account: 1,
        email: "ada@example.org".into(),
        url: "http://127.0.0.1/EWS/Exchange.asmx".into(),
        calendars: true,
        contacts: true,
        tasks: true,
        from: 1,
        to: 2_000_000_000,
        ..Default::default()
    };
    let cancel = std::sync::atomic::AtomicBool::new(false);
    let result = crate::standin::block_on(sync_directory(&client, &standin, &request, &cancel, &mut |_| true)).unwrap();
    assert!(result.errors.is_empty(), "{}", result.errors.join("; "));
    assert!(!result.auth_refused);
}

/// The stand-in, with every request it was asked written down.
struct Logged {
    standin: crate::standin::StandIn,
    bodies: Mutex<Vec<String>>,
}

impl crate::http::HttpExchange for Logged {
    fn exchange(&self, request: &crate::http::HttpRequest) -> impl Future<Output = Result<crate::http::HttpResponse, crate::http::TransportError>> {
        self.bodies.lock().unwrap_or_else(|poisoned| poisoned.into_inner()).push(String::from_utf8_lossy(request.body()).into_owned());
        self.standin.exchange(request)
    }
}

/// A read of the calendar alone (#1274) asks for the calendars and nothing else: no address book,
/// no to-do list.
#[test]
fn a_calendar_read_asks_for_the_calendars_alone() {
    let logged = Logged { standin: crate::standin::StandIn::new(), bodies: Mutex::new(Vec::new()) };
    let client = crate::Client::new("http://127.0.0.1/EWS/Exchange.asmx", crate::Secret::new(crate::standin::AUTHORIZATION)).unwrap();
    let request = mailcraft_model::sync::EwsDirectoryRequest {
        account: 1,
        email: "ada@example.org".into(),
        url: "http://127.0.0.1/EWS/Exchange.asmx".into(),
        calendars: true,
        from: 1_791_000_000,
        to: 1_793_000_000,
        background: true,
        ..Default::default()
    };
    let cancel = std::sync::atomic::AtomicBool::new(false);
    let result = crate::standin::block_on(sync_directory(&client, &logged, &request, &cancel, &mut |_| true)).unwrap();
    assert!(result.errors.is_empty(), "{}", result.errors.join("; "));
    assert!(result.calendars.is_some() && result.contacts.is_none() && result.tasks.is_none());
    assert!(result.events.iter().all(|download| (download.from, download.to) == (request.from, request.to)));
    let bodies = logged.bodies.lock().unwrap().clone();
    assert!(bodies.iter().any(|body| body.contains("\"calendar\"")), "{bodies:?}");
    assert!(bodies.iter().all(|body| !body.contains("\"contacts\"") && !body.contains("\"tasks\"")), "{bodies:?}");
    assert!(bodies.iter().all(|body| !body.contains("CreateItem") && !body.contains("UpdateItem") && !body.contains("DeleteItem")));
}

#[test]
fn a_task_patch_becomes_a_to_do() {
    let op = TaskOp {
        kind: TaskOpKind::Create,
        patch: TaskPatch { title: Some("Send it".into()), importance: Some(Importance::Low), ..Default::default() },
        ..Default::default()
    };
    let task = task_from_patch(&op);
    assert_eq!(task.title, "Send it");
    assert_eq!(task.importance, Importance::Low);
    let _ = TaskFields::default();
}

struct TruncatedCalendar(crate::standin::StandIn);

impl crate::http::HttpExchange for TruncatedCalendar {
    async fn exchange(&self, request: &crate::http::HttpRequest) -> Result<crate::http::HttpResponse, crate::http::TransportError> {
        let body = String::from_utf8_lossy(request.body());
        if !body.contains("<m:CalendarView") {
            return self.0.exchange(request).await;
        }
        let first = body.contains("StartDate=\"2026-11-01T00:00:00Z\"");
        let second = body.contains("EndDate=\"2026-11-03T00:00:00Z\"");
        let last = !(first && second);
        let item = |id: &str, start: &str, end: &str| {
            format!(r#"<t:CalendarItem><t:ItemId Id="{id}" /><t:Start>{start}</t:Start><t:End>{end}</t:End></t:CalendarItem>"#)
        };
        let mut items = item("long", "2026-11-01T00:00:00Z", "2026-11-03T00:00:00Z");
        if second && last {
            items.push_str(&item("short", "2026-11-02T12:00:00Z", "2026-11-02T13:00:00Z"));
        }
        let reply = crate::standin::response(
            "FindItem",
            &format!(
                r#"<m:FindItemResponseMessage ResponseClass="Success"><m:ResponseCode>NoError</m:ResponseCode><m:RootFolder IncludesLastItemInRange="{last}"><t:Items>{items}</t:Items></m:RootFolder></m:FindItemResponseMessage>"#
            ),
        );
        Ok(crate::http::HttpResponse::new(200, reply.into_bytes()))
    }
}

#[test]
fn an_overlapping_long_event_does_not_hide_the_next_appointment() {
    let client = crate::Client::new("http://127.0.0.1/EWS/Exchange.asmx", crate::Secret::new(crate::standin::AUTHORIZATION)).unwrap();
    let server = TruncatedCalendar(crate::standin::StandIn::new());
    let request = mailcraft_model::sync::EwsDirectoryRequest {
        calendars: true,
        from: mailcraft_time::iso_to_timestamp("2026-11-01T00:00:00Z", 0).unwrap(),
        to: mailcraft_time::iso_to_timestamp("2026-11-03T00:00:00Z", 0).unwrap(),
        ..Default::default()
    };
    let cancel = std::sync::atomic::AtomicBool::new(false);
    let result = crate::standin::block_on(sync_directory(&client, &server, &request, &cancel, &mut |_| true)).unwrap();
    let main = result.events.iter().find(|d| d.calendar.is_empty()).unwrap();
    assert!(main.complete);
    assert_eq!(main.events.iter().filter(|e| e.id == "long").count(), 1);
    assert!(main.events.iter().any(|e| e.id == "short"));
}

/// Route the named mailbox to a stand-in of its own. Its single-mailbox requests
/// have the same folder IDs and shapes without the mailbox qualifier.
struct SharedDirectory(crate::standin::StandIn);

impl crate::http::HttpExchange for SharedDirectory {
    async fn exchange(&self, request: &crate::http::HttpRequest) -> Result<crate::http::HttpResponse, crate::http::TransportError> {
        let body = String::from_utf8_lossy(request.body()).replace("<t:Mailbox><t:EmailAddress>shared@example.org</t:EmailAddress></t:Mailbox>", "");
        let request = crate::http::HttpRequest::soap(request.url(), &crate::Secret::new(crate::standin::AUTHORIZATION), body).unwrap();
        self.0.exchange(&request).await
    }
}

#[test]
fn shared_folders_alone_cannot_replace_the_account_directory() {
    let client = crate::Client::new("http://127.0.0.1/EWS/Exchange.asmx", crate::Secret::new(crate::standin::AUTHORIZATION)).unwrap();
    let server = SharedDirectory(crate::standin::StandIn::new());
    let request = mailcraft_model::sync::EwsDirectoryRequest {
        shared: vec![mailcraft_model::sync::EwsSharedMailbox {
            address: "shared@example.org".into(),
            calendar: true,
            contacts: true,
            ..Default::default()
        }],
        ..Default::default()
    };
    let cancel = std::sync::atomic::AtomicBool::new(false);
    let result = crate::standin::block_on(sync_directory(&client, &server, &request, &cancel, &mut |_| true)).unwrap();
    assert!(!result.calendars.unwrap().complete);
    assert!(!result.contacts.unwrap().complete);
}

/// A server with calendar folders as told: `GetFolder` gives the default one (`main`),
/// `FindFolder` lists `listed` in that order, and `FindItem` in the folder `broken` is refused
/// (`ErrorInternalServerError`, no busy server) while any other is an empty folder.
struct Calendars {
    listed: Vec<String>,
    broken: &'static str,
}

impl crate::http::HttpExchange for Calendars {
    fn exchange(&self, request: &crate::http::HttpRequest) -> impl Future<Output = Result<crate::http::HttpResponse, crate::http::TransportError>> {
        let body = String::from_utf8_lossy(request.body()).into_owned();
        let folder = |id: &str| {
            format!(
                "<t:CalendarFolder><t:FolderId Id=\"{id}\" ChangeKey=\"k\"/><t:FolderClass>IPF.Appointment</t:FolderClass>\
                 <t:DisplayName>Calendar {id}</t:DisplayName></t:CalendarFolder>"
            )
        };
        let answer = if body.contains("<m:GetFolder") {
            crate::standin::response(
                "GetFolder",
                &format!(
                    "<m:GetFolderResponseMessage ResponseClass=\"Success\"><m:ResponseCode>NoError</m:ResponseCode><m:Folders>{}</m:Folders></m:GetFolderResponseMessage>",
                    folder("main")
                ),
            )
        } else if body.contains("<m:FindFolder") {
            let all: String = self.listed.iter().map(|id| folder(id)).collect();
            crate::standin::response(
                "FindFolder",
                &format!(
                    "<m:FindFolderResponseMessage ResponseClass=\"Success\"><m:ResponseCode>NoError</m:ResponseCode>\
                     <m:RootFolder IndexedPagingOffset=\"{n}\" TotalItemsInView=\"{n}\" IncludesLastItemInRange=\"true\"><t:Folders>{all}</t:Folders></m:RootFolder>\
                     </m:FindFolderResponseMessage>",
                    n = self.listed.len()
                ),
            )
        } else if body.contains(&format!("Id=\"{}\"", self.broken)) {
            crate::standin::response(
                "FindItem",
                "<m:FindItemResponseMessage ResponseClass=\"Error\"><m:ResponseCode>ErrorInternalServerError</m:ResponseCode></m:FindItemResponseMessage>",
            )
        } else {
            crate::standin::response(
                "FindItem",
                "<m:FindItemResponseMessage ResponseClass=\"Success\"><m:ResponseCode>NoError</m:ResponseCode>\
                 <m:RootFolder IndexedPagingOffset=\"0\" TotalItemsInView=\"0\" IncludesLastItemInRange=\"true\"><t:Items/></m:RootFolder></m:FindItemResponseMessage>",
            )
        };
        ready(Ok(crate::http::HttpResponse::new(200, answer.into_bytes())))
    }
}

fn read_calendars_of(server: &Calendars, shared: &[&str]) -> mailcraft_model::sync::EwsDirectoryResult {
    let client = crate::Client::new("http://127.0.0.1/EWS/Exchange.asmx", crate::Secret::new("test")).unwrap();
    let request = mailcraft_model::sync::EwsDirectoryRequest {
        account: 1,
        email: "ada@example.org".into(),
        url: "http://127.0.0.1/EWS/Exchange.asmx".into(),
        calendars: true,
        from: 1_791_000_000,
        to: 1_793_000_000,
        shared: shared
            .iter()
            .map(|address| mailcraft_model::sync::EwsSharedMailbox { address: (*address).into(), calendar: true, ..Default::default() })
            .collect(),
        ..Default::default()
    };
    let cancel = std::sync::atomic::AtomicBool::new(false);
    crate::standin::block_on(sync_directory(&client, server, &request, &cancel, &mut |_| true)).unwrap()
}

/// A calendar whose events can't be read is passed over (#962 review): the others are read, it
/// stays in the list (so nothing of it is taken away here), the list isn't whole, and it is said.
#[test]
fn a_calendar_that_cant_be_read_is_passed_over_and_the_others_are_read() {
    let server = Calendars { listed: vec!["main".into(), "broken".into(), "team".into()], broken: "broken" };
    let result = read_calendars_of(&server, &[]);
    let list = result.calendars.as_ref().expect("a list");
    assert_eq!(list.calendars.len(), 3, "the one that failed is still listed");
    assert!(!list.complete, "a list with a calendar not read takes nothing away");
    assert_eq!(result.events.len(), 2, "the main calendar's events and the team's");
    assert!(result.events.iter().all(|download| download.calendar != "broken"));
    assert_eq!(result.errors, ["Couldn't read one of the calendars from the server"]);
}

/// The default calendar is read first wherever the listing puts it, so the cap on how many are
/// read never cuts it (#962 review): a list without its main calendar would be refused whole.
#[test]
fn the_default_calendar_comes_first_whatever_the_listing() {
    let most = mailcraft_model::remote::MAX_REMOTE_CALENDARS;
    let mut listed: Vec<String> = (0..most + 20).map(|n| format!("c{n}")).collect();
    listed.insert(most + 10, "main".into());
    let result = read_calendars_of(&Calendars { listed, broken: "none" }, &[]);
    let list = result.calendars.as_ref().expect("a list");
    assert!(list.calendars.first().is_some_and(|c| c.main), "the main one first");
    assert_eq!(list.calendars.iter().filter(|c| c.main).count(), 1);
    assert_eq!(list.calendars.len(), most);
    assert!(!list.complete, "more than are read");
}

/// Shared calendars put with the account's own past the cap make a list that isn't whole
/// (#962 review): the calendars cut from it are not taken away here.
#[test]
fn shared_calendars_past_the_cap_make_the_list_not_whole() {
    let listed: Vec<String> = std::iter::once("main".to_owned()).chain((0..60).map(|n| format!("c{n}"))).collect();
    let result = read_calendars_of(&Calendars { listed, broken: "none" }, &["team@example.org"]);
    let list = result.calendars.as_ref().expect("a list");
    assert!(list.calendars.len() > mailcraft_model::remote::MAX_REMOTE_CALENDARS);
    assert!(!list.complete);
    assert!(result.errors.is_empty(), "{:?}", result.errors);
}

/// The stand-in, with every calendar view refused.
struct NoCalendarView(crate::standin::StandIn);

impl crate::http::HttpExchange for NoCalendarView {
    fn exchange(&self, request: &crate::http::HttpRequest) -> impl Future<Output = Result<crate::http::HttpResponse, crate::http::TransportError>> {
        let refused = String::from_utf8_lossy(request.body()).contains("<m:CalendarView");
        let answer = refused.then(|| {
            crate::standin::response(
                "FindItem",
                "<m:FindItemResponseMessage ResponseClass=\"Error\"><m:ResponseCode>ErrorInternalServerError</m:ResponseCode></m:FindItemResponseMessage>",
            )
        });
        let delegated = self.0.exchange(request);
        async move {
            match answer {
                Some(body) => Ok(crate::http::HttpResponse::new(200, body.into_bytes())),
                None => delegated.await,
            }
        }
    }
}

/// A calendar that couldn't be read leaves the mail whole (#962 review): the run's last piece may
/// still take away what the server no longer has, and the piece with the calendar says it failed,
/// so that it isn't taken for one read just now.
#[test]
fn a_calendar_that_failed_leaves_the_mail_whole_and_says_so_in_its_piece() {
    let standin = NoCalendarView(crate::standin::StandIn::new());
    let client = crate::Client::new("https://mail.example.org/EWS/Exchange.asmx", crate::Secret::new(crate::standin::AUTHORIZATION)).unwrap();
    let request = mailcraft_model::sync::EwsMailRequest {
        email: "ada@example.org".into(),
        url: "https://mail.example.org/EWS/Exchange.asmx".into(),
        list_folders: true,
        directory: Some(Box::new(mailcraft_model::sync::EwsDirectoryRequest {
            account: 1,
            email: "ada@example.org".into(),
            url: "https://mail.example.org/EWS/Exchange.asmx".into(),
            calendars: true,
            from: 1_791_000_000,
            to: 1_793_000_000,
            ..Default::default()
        })),
        ..Default::default()
    };
    let cancel = std::sync::atomic::AtomicBool::new(false);
    let mut pieces = Vec::new();
    crate::standin::block_on(crate::sync_mail(&client, &standin, &request, &cancel, &mut |_| true, &mut |piece| pieces.push(piece))).unwrap();
    let directory = pieces.iter().find_map(|piece| piece.directory.as_deref()).expect("the calendar's piece");
    assert_eq!(directory.errors, ["Couldn't read one of the calendars from the server"]);
    let last = pieces.last().expect("a last piece");
    assert!(last.whole, "the mail was read whole");
    assert!(last.errors.iter().any(|line| line.contains("calendars")), "the run says it too: {:?}", last.errors);
}

/// A server with one weekly series (`UID` `series-uid`, id `series-1`) and, in the week around
/// 2026-10-14T09:00Z, its occurrences of the 7th, the 14th (`occ-14`, moved to 11:00, so an
/// exception) and the 21st, with another series' occurrence at the very same time; every
/// `DeleteItem` and `UpdateItem` is done, and written down.
struct Series {
    has_the_day: bool,
    asked: Mutex<Vec<String>>,
}

const OCCURRENCE: i64 = 1_791_968_400; // 2026-10-14T09:00:00Z

impl crate::http::HttpExchange for Series {
    fn exchange(&self, request: &crate::http::HttpRequest) -> impl Future<Output = Result<crate::http::HttpResponse, crate::http::TransportError>> {
        let body = String::from_utf8_lossy(request.body()).into_owned();
        self.asked.lock().unwrap_or_else(|poisoned| poisoned.into_inner()).push(body.clone());
        let event = |id: &str, uid: &str, kind: &str, start: &str, original: &str| {
            format!(
                "<t:CalendarItem><t:ItemId Id=\"{id}\" ChangeKey=\"k\"/><t:Subject>Stand-up</t:Subject><t:Start>{start}</t:Start>\
                 <t:End>{start}</t:End><t:OriginalStart>{original}</t:OriginalStart><t:CalendarItemType>{kind}</t:CalendarItemType>\
                 <t:UID>{uid}</t:UID></t:CalendarItem>"
            )
        };
        let answer = if body.contains("<m:GetItem") {
            assert!(body.contains("Id=\"series-1\"") && body.contains("calendar:UID"), "{body}");
            crate::standin::response(
                "GetItem",
                "<m:GetItemResponseMessage ResponseClass=\"Success\"><m:ResponseCode>NoError</m:ResponseCode><m:Items>\
                 <t:CalendarItem><t:ItemId Id=\"series-1\" ChangeKey=\"k\"/><t:UID>series-uid</t:UID></t:CalendarItem></m:Items></m:GetItemResponseMessage>",
            )
        } else if body.contains("<m:FindItem") {
            assert!(body.contains("<m:CalendarView") && body.contains("DistinguishedFolderId Id=\"calendar\""), "{body}");
            let mut items = event("occ-7", "series-uid", "Occurrence", "2026-10-07T09:00:00Z", "2026-10-07T09:00:00Z");
            if self.has_the_day {
                items.push_str(&event("occ-14", "series-uid", "Exception", "2026-10-14T11:00:00Z", "2026-10-14T09:00:00Z"));
            }
            items.push_str(&event("other-14", "other-uid", "Occurrence", "2026-10-14T09:00:00Z", "2026-10-14T09:00:00Z"));
            items.push_str(&event("occ-21", "series-uid", "Occurrence", "2026-10-21T09:00:00Z", "2026-10-21T09:00:00Z"));
            crate::standin::response(
                "FindItem",
                &format!(
                    "<m:FindItemResponseMessage ResponseClass=\"Success\"><m:ResponseCode>NoError</m:ResponseCode>\
                     <m:RootFolder TotalItemsInView=\"4\" IncludesLastItemInRange=\"true\"><t:Items>{items}</t:Items></m:RootFolder></m:FindItemResponseMessage>"
                ),
            )
        } else if body.contains("<m:DeleteItem") {
            crate::standin::response(
                "DeleteItem",
                "<m:DeleteItemResponseMessage ResponseClass=\"Success\"><m:ResponseCode>NoError</m:ResponseCode></m:DeleteItemResponseMessage>",
            )
        } else {
            assert!(body.contains("<m:UpdateItem"), "{body}");
            crate::standin::response(
                "UpdateItem",
                "<m:UpdateItemResponseMessage ResponseClass=\"Success\"><m:ResponseCode>NoError</m:ResponseCode><m:Items>\
                 <t:CalendarItem><t:ItemId Id=\"occ-14\" ChangeKey=\"k2\"/></t:CalendarItem></m:Items></m:UpdateItemResponseMessage>",
            )
        };
        ready(Ok(crate::http::HttpResponse::new(200, answer.into_bytes())))
    }
}

fn one_day(kind: EventChangeKind, fields: EventFields) -> mailcraft_model::sync::EwsDirectoryRequest {
    mailcraft_model::sync::EwsDirectoryRequest {
        account: 1,
        email: "ada@example.org".into(),
        url: "http://127.0.0.1/EWS/Exchange.asmx".into(),
        changes: Some(mailcraft_model::remote::EventsUpload {
            ops: vec![EventOp { kind, event: 9, series: "series-1".into(), occurrence: OCCURRENCE, fields, ..Default::default() }],
            ..Default::default()
        }),
        ..Default::default()
    }
}

fn push(server: &Series, request: &mailcraft_model::sync::EwsDirectoryRequest) -> mailcraft_model::remote::EventOutcome {
    let client = crate::Client::new("http://127.0.0.1/EWS/Exchange.asmx", crate::Secret::new("test")).unwrap();
    let cancel = std::sync::atomic::AtomicBool::new(false);
    let result = crate::standin::block_on(sync_directory(&client, server, request, &cancel, &mut |_| true)).unwrap();
    result.uploaded.unwrap().outcomes.into_iter().next().unwrap()
}

/// One day of a series that has no name of its own here is found by the series' `UID` and when
/// the series has it (even moved since), and deleted there; it is known by its own id after.
#[test]
fn one_day_of_a_series_is_found_on_the_server_and_deleted() {
    let server = Series { has_the_day: true, asked: Mutex::new(Vec::new()) };
    let outcome = push(&server, &one_day(EventChangeKind::Delete, EventFields::default()));
    assert_eq!(outcome.result, mailcraft_model::remote::EventResult::Done, "{}", outcome.error);
    assert_eq!(outcome.remote, "occ-14");
    let asked = server.asked.lock().unwrap().clone();
    let deleted: Vec<&String> = asked.iter().filter(|body| body.contains("<m:DeleteItem")).collect();
    assert_eq!(deleted.len(), 1);
    assert!(deleted[0].contains("Id=\"occ-14\""), "that day alone, not the series nor the other series' day: {}", deleted[0]);
}

#[test]
fn one_day_of_a_series_is_found_on_the_server_and_changed() {
    let server = Series { has_the_day: true, asked: Mutex::new(Vec::new()) };
    let fields = EventFields { title: Some("Stand-up, short".into()), ..Default::default() };
    let outcome = push(&server, &one_day(EventChangeKind::Update, fields));
    assert_eq!(outcome.result, mailcraft_model::remote::EventResult::Done, "{}", outcome.error);
    assert_eq!(outcome.remote, "occ-14");
    let asked = server.asked.lock().unwrap().clone();
    let updated = asked.iter().find(|body| body.contains("<m:UpdateItem")).expect("changed");
    assert!(updated.contains("Id=\"occ-14\"") && updated.contains("Stand-up, short"), "{updated}");
}

/// The server's series has no such day (it was taken away there): the change is gone, and
/// nothing is deleted or changed.
#[test]
fn a_day_the_servers_series_hasnt_is_gone() {
    let server = Series { has_the_day: false, asked: Mutex::new(Vec::new()) };
    let outcome = push(&server, &one_day(EventChangeKind::Delete, EventFields::default()));
    assert_eq!(outcome.result, mailcraft_model::remote::EventResult::Gone);
    assert!(server.asked.lock().unwrap().iter().all(|body| !body.contains("<m:DeleteItem") && !body.contains("<m:UpdateItem")));
}

/// A server that makes every item it is asked to, writing each request down.
struct Making {
    asked: Mutex<Vec<String>>,
}

impl crate::http::HttpExchange for Making {
    fn exchange(&self, request: &crate::http::HttpRequest) -> impl Future<Output = Result<crate::http::HttpResponse, crate::http::TransportError>> {
        let body = String::from_utf8_lossy(request.body()).into_owned();
        let operation = if body.contains("<m:CreateItem") { "CreateItem" } else { "UpdateItem" };
        self.asked.lock().unwrap_or_else(|poisoned| poisoned.into_inner()).push(body);
        let answer = crate::standin::response(
            operation,
            &format!(
                "<m:{operation}ResponseMessage ResponseClass=\"Success\"><m:ResponseCode>NoError</m:ResponseCode><m:Items>\
                 <t:CalendarItem><t:ItemId Id=\"made-1\" ChangeKey=\"k\"/></t:CalendarItem></m:Items></m:{operation}ResponseMessage>"
            ),
        );
        ready(Ok(crate::http::HttpResponse::new(200, answer.into_bytes())))
    }
}

/// The request that makes a weekly stand-up, with the calendar's zone `zone`.
fn made_in(zone: &str) -> String {
    let server = Making { asked: Mutex::new(Vec::new()) };
    let client = crate::Client::new("http://127.0.0.1/EWS/Exchange.asmx", crate::Secret::new("test")).unwrap();
    let request = mailcraft_model::sync::EwsDirectoryRequest {
        account: 1,
        email: "ada@example.org".into(),
        url: "http://127.0.0.1/EWS/Exchange.asmx".into(),
        changes: Some(mailcraft_model::remote::EventsUpload {
            ops: vec![EventOp {
                kind: EventChangeKind::Create,
                event: 3,
                fields: EventFields {
                    title: Some("Stand-up".into()),
                    time: Some(EventTime { start: OCCURRENCE, end: OCCURRENCE + 900, all_day: false }),
                    ..Default::default()
                },
                ..Default::default()
            }],
            zone: zone.into(),
            ..Default::default()
        }),
        ..Default::default()
    };
    let cancel = std::sync::atomic::AtomicBool::new(false);
    let result = crate::standin::block_on(sync_directory(&client, &server, &request, &cancel, &mut |_| true)).unwrap();
    assert_eq!(result.uploaded.unwrap().outcomes[0].result, mailcraft_model::remote::EventResult::Done);
    server.asked.lock().unwrap().iter().find(|body| body.contains("<m:CreateItem")).cloned().expect("made")
}

/// Exchange takes a zone by the name Windows has for it, and refuses one of the IANA database:
/// a calendar in `Europe/Berlin` makes its events in `W. Europe Standard Time`, so that a series
/// made here repeats by Berlin's clock across a daylight-saving change.
#[test]
fn an_event_is_made_in_its_zone_by_the_name_windows_has_for_it() {
    let made = made_in("Europe/Berlin");
    assert!(made.contains("<t:StartTimeZone Id=\"W. Europe Standard Time\"/>"), "{made}");
    assert!(made.contains("<t:EndTimeZone Id=\"W. Europe Standard Time\"/>"), "{made}");
    assert!(!made.contains("Europe/Berlin"), "{made}");
    // Named the Windows way already: as it is.
    assert!(made_in("Eastern Standard Time").contains("<t:StartTimeZone Id=\"Eastern Standard Time\"/>"));
}

/// A zone that has no Windows name (or is no zone at all) is left out rather than sent and
/// refused: the times go as the instants they are.
#[test]
fn a_zone_windows_has_no_name_for_is_left_out() {
    let made = made_in("Nowhere/Atlantis");
    assert!(!made.contains("StartTimeZone") && !made.contains("Atlantis"), "{made}");
}

/// Somebody's calendar shared with the account for its free and busy times alone: listing it is
/// refused (`ErrorAccessDenied`, at `GetFolder` or at its calendar view, as `deny_folder` says),
/// and `GetUserAvailability` tells when they are busy, unless `nothing_at_all`.
struct FreeBusyOnly {
    deny_folder: bool,
    nothing_at_all: bool,
}

impl crate::http::HttpExchange for FreeBusyOnly {
    fn exchange(&self, request: &crate::http::HttpRequest) -> impl Future<Output = Result<crate::http::HttpResponse, crate::http::TransportError>> {
        let body = String::from_utf8_lossy(request.body()).into_owned();
        let denied = |operation: &str| {
            crate::standin::response(
                operation,
                &format!(
                    "<m:{operation}ResponseMessage ResponseClass=\"Error\"><m:ResponseCode>ErrorAccessDenied</m:ResponseCode></m:{operation}ResponseMessage>"
                ),
            )
        };
        let answer = if body.contains("<m:GetFolder") {
            if self.deny_folder {
                denied("GetFolder")
            } else {
                crate::standin::response(
                    "GetFolder",
                    "<m:GetFolderResponseMessage ResponseClass=\"Success\"><m:ResponseCode>NoError</m:ResponseCode><m:Folders>\
                     <t:CalendarFolder><t:FolderId Id=\"their-calendar\" ChangeKey=\"k\"/><t:FolderClass>IPF.Appointment</t:FolderClass>\
                     <t:DisplayName>Calendar</t:DisplayName></t:CalendarFolder></m:Folders></m:GetFolderResponseMessage>",
                )
            }
        } else if body.contains("<m:FindFolder") {
            denied("FindFolder")
        } else if body.contains("<m:FindItem") {
            denied("FindItem")
        } else {
            assert!(body.contains("<m:GetUserAvailabilityRequest") && body.contains("<t:Address>bo@example.org</t:Address>"), "{body}");
            let response = if self.nothing_at_all {
                "<m:FreeBusyResponse><m:ResponseMessage ResponseClass=\"Error\"><m:ResponseCode>ErrorNoFreeBusyAccess</m:ResponseCode>\
                 </m:ResponseMessage></m:FreeBusyResponse>"
                    .to_owned()
            } else {
                "<m:FreeBusyResponse><m:ResponseMessage ResponseClass=\"Success\"><m:ResponseCode>NoError</m:ResponseCode></m:ResponseMessage>\
                 <m:FreeBusyView><t:FreeBusyViewType>FreeBusy</t:FreeBusyViewType><t:CalendarEventArray>\
                 <t:CalendarEvent><t:StartTime>2026-10-14T09:00:00</t:StartTime><t:EndTime>2026-10-14T10:00:00</t:EndTime><t:BusyType>Busy</t:BusyType></t:CalendarEvent>\
                 <t:CalendarEvent><t:StartTime>2026-10-15T13:00:00</t:StartTime><t:EndTime>2026-10-15T17:00:00</t:EndTime><t:BusyType>OOF</t:BusyType></t:CalendarEvent>\
                 </t:CalendarEventArray></m:FreeBusyView></m:FreeBusyResponse>"
                    .to_owned()
            };
            crate::standin::envelope(&format!(
                "<m:GetUserAvailabilityResponse xmlns:m=\"{}\" xmlns:t=\"{}\"><m:FreeBusyResponseArray>{response}</m:FreeBusyResponseArray></m:GetUserAvailabilityResponse>",
                crate::soap::MESSAGES,
                crate::soap::TYPES
            ))
        };
        ready(Ok(crate::http::HttpResponse::new(200, answer.into_bytes())))
    }
}

fn opened_by_address(server: &FreeBusyOnly) -> mailcraft_model::sync::EwsDirectoryResult {
    let client = crate::Client::new("http://127.0.0.1/EWS/Exchange.asmx", crate::Secret::new("test")).unwrap();
    let request = mailcraft_model::sync::EwsDirectoryRequest {
        account: 1,
        email: "ada@example.org".into(),
        url: "http://127.0.0.1/EWS/Exchange.asmx".into(),
        from: 1_791_763_200,
        to: 1_791_763_200 + 7 * 86_400,
        shared: vec![mailcraft_model::sync::EwsSharedMailbox { address: "bo@example.org".into(), calendar: true, ..Default::default() }],
        ..Default::default()
    };
    let cancel = std::sync::atomic::AtomicBool::new(false);
    crate::standin::block_on(sync_directory(&client, server, &request, &cancel, &mut |_| true)).unwrap()
}

/// A calendar opened by address that may only be seen for its free and busy times is drawn from
/// those, as Outlook shows one: blocks of time without a title, listed as such.
#[test]
fn a_calendar_shared_for_free_and_busy_alone_is_drawn_from_those() {
    for deny_folder in [false, true] {
        let result = opened_by_address(&FreeBusyOnly { deny_folder, nothing_at_all: false });
        assert!(result.errors.is_empty(), "{:?}", result.errors);
        let list = result.calendars.as_ref().expect("listed");
        let [calendar] = list.calendars.as_slice() else { panic!("one calendar: {list:?}") };
        assert_eq!(calendar.id, "user:bo@example.org");
        assert_eq!(calendar.rights, mailcraft_model::Rights::FreeBusy);
        assert_eq!(calendar.group, "Shared Calendars");
        assert!(!list.complete);
        let [events] = result.events.as_slice() else { panic!("one download") };
        assert!(events.free_busy && events.complete && events.calendar == "user:bo@example.org");
        let blocks: Vec<_> = events.events.iter().map(|e| (e.start - 1_791_763_200, e.end - 1_791_763_200, e.show_as, e.title.is_empty())).collect();
        assert_eq!(
            blocks,
            [
                (2 * 86_400 + 9 * 3_600, 2 * 86_400 + 10 * 3_600, mailcraft_model::ShowAs::Busy, true),
                (3 * 86_400 + 13 * 3_600, 3 * 86_400 + 17 * 3_600, mailcraft_model::ShowAs::OutOfOffice, true),
            ],
            "deny_folder: {deny_folder}"
        );
    }
}

/// Somebody who won't let even their free and busy times be seen: the calendar isn't read, and
/// that is said, as before.
#[test]
fn a_calendar_that_tells_nothing_at_all_is_said_to_be_unread() {
    let result = opened_by_address(&FreeBusyOnly { deny_folder: true, nothing_at_all: true });
    assert_eq!(result.errors, ["Couldn't read a calendar that was shared with the account"]);
    assert!(result.events.is_empty());
}

/// A server that answers every `DeleteItem` with `code`.
struct Deleting(&'static str);

impl crate::http::HttpExchange for Deleting {
    fn exchange(&self, request: &crate::http::HttpRequest) -> impl Future<Output = Result<crate::http::HttpResponse, crate::http::TransportError>> {
        assert!(String::from_utf8_lossy(request.body()).contains("<m:DeleteItem"));
        let class = if self.0 == "NoError" { "Success" } else { "Error" };
        let body = crate::standin::response(
            "DeleteItem",
            &format!(
                "<m:DeleteItemResponseMessage ResponseClass=\"{class}\"><m:ResponseCode>{}</m:ResponseCode></m:DeleteItemResponseMessage>",
                self.0
            ),
        );
        ready(Ok(crate::http::HttpResponse::new(200, body.into_bytes())))
    }
}

/// A to-do's deletion done, or found done already, names the to-do (#962 review): the engine
/// takes it off the deletions waiting by that id, so it isn't sent again at every Send/Receive.
#[test]
fn a_to_do_deleted_names_itself_in_the_outcome() {
    for (code, result) in [("NoError", TaskResult::Done), ("ErrorItemNotFound", TaskResult::Gone)] {
        let client = crate::Client::new("http://127.0.0.1/EWS/Exchange.asmx", crate::Secret::new("test")).unwrap();
        let request = mailcraft_model::sync::EwsDirectoryRequest {
            account: 1,
            email: "ada@example.org".into(),
            url: "http://127.0.0.1/EWS/Exchange.asmx".into(),
            task_changes: Some(mailcraft_model::remote_tasks::TasksUpload {
                ops: vec![TaskOp { kind: TaskOpKind::Delete, task: 4, id: "todo-1".into(), ..Default::default() }],
                ..Default::default()
            }),
            ..Default::default()
        };
        let cancel = std::sync::atomic::AtomicBool::new(false);
        let done = crate::standin::block_on(sync_directory(&client, &Deleting(code), &request, &cancel, &mut |_| true)).unwrap();
        let outcome = &done.tasks_uploaded.unwrap().outcomes[0];
        assert_eq!((outcome.result, outcome.id.as_str()), (result, "todo-1"), "{code}");
    }
}

/// A Send/Receive of the account's own calendar asks for the owner's working hours (the stand-in's
/// are Monday to Friday, 08:00 to 17:00 UTC); a read of the days on screen doesn't ask.
#[test]
fn a_send_receive_brings_the_owners_working_hours_and_a_calendar_read_doesnt() {
    for background in [false, true] {
        let standin = crate::standin::StandIn::new();
        standin.state().schedules = vec![crate::standin_availability::Schedule { email: "ada@example.org".into(), ..Default::default() }];
        let client = crate::Client::new("http://127.0.0.1/EWS/Exchange.asmx", crate::Secret::new(crate::standin::AUTHORIZATION)).unwrap();
        let request = mailcraft_model::sync::EwsDirectoryRequest {
            account: 1,
            email: "ada@example.org".into(),
            url: "http://127.0.0.1/EWS/Exchange.asmx".into(),
            calendars: true,
            from: 1_791_000_000,
            to: 1_793_000_000,
            background,
            ..Default::default()
        };
        let cancel = std::sync::atomic::AtomicBool::new(false);
        let result = crate::standin::block_on(sync_directory(&client, &standin, &request, &cancel, &mut |_| true)).unwrap();
        assert!(result.errors.is_empty(), "{:?}", result.errors);
        if background {
            assert_eq!(result.working_hours, None);
            assert_eq!(standin.state().availability_asked, 0);
        } else {
            use mailcraft_time::Weekday::{Fri, Mon, Thu, Tue, Wed};
            let hours = result.working_hours.expect("the hours");
            assert_eq!((hours.days, hours.start, hours.end, hours.offset), (vec![Mon, Tue, Wed, Thu, Fri], 480, 1020, 0));
        }
    }
}

/// A server that refuses every `CreateItem` as not fitting its schema, as one older than Exchange
/// 2013 refuses a proposed time.
struct NoProposals;

impl crate::http::HttpExchange for NoProposals {
    fn exchange(&self, request: &crate::http::HttpRequest) -> impl Future<Output = Result<crate::http::HttpResponse, crate::http::TransportError>> {
        assert!(String::from_utf8_lossy(request.body()).contains("<t:ProposedStart>"));
        let body = crate::standin::response(
            "CreateItem",
            "<m:CreateItemResponseMessage ResponseClass=\"Error\"><m:ResponseCode>ErrorSchemaValidation</m:ResponseCode></m:CreateItemResponseMessage>",
        );
        ready(Ok(crate::http::HttpResponse::new(200, body.into_bytes())))
    }
}

/// An answer with a proposed time that the server can't take isn't sent without it: it is refused,
/// and the owner is told to answer again without one.
#[test]
fn an_answer_whose_proposal_the_server_cant_take_is_refused_and_said() {
    let client = crate::Client::new("http://127.0.0.1/EWS/Exchange.asmx", crate::Secret::new("test")).unwrap();
    let request = mailcraft_model::sync::EwsDirectoryRequest {
        changes: Some(mailcraft_model::remote::EventsUpload {
            ops: vec![EventOp {
                kind: EventChangeKind::Respond,
                remote: "meeting-1".into(),
                response: mailcraft_model::Response::Tentative,
                send: true,
                proposed: Some(EventTime { start: OCCURRENCE, end: OCCURRENCE + 3_600, all_day: false }),
                ..Default::default()
            }],
            ..Default::default()
        }),
        ..Default::default()
    };
    let cancel = std::sync::atomic::AtomicBool::new(false);
    let result = crate::standin::block_on(sync_directory(&client, &NoProposals, &request, &cancel, &mut |_| true)).unwrap();
    let outcome = &result.uploaded.unwrap().outcomes[0];
    assert_eq!(outcome.result, mailcraft_model::remote::EventResult::Refused);
    assert_eq!(outcome.error, "the server can't take a proposed time; answer again without proposing one");
}

/// Outlook shows neither a hidden calendar folder (`PidTagAttributeHidden`, one the mailbox keeps
/// for itself or for a program) nor a calendar in Deleted Items, at any depth; nor does the
/// account's list of calendars. Another calendar beside the default one is there.
#[test]
fn hidden_and_deleted_calendars_are_not_the_accounts() {
    let standin = crate::standin::StandIn::new();
    {
        let mut s = standin.state();
        let id_of = |n: usize| format!("AAMkAGV4YW1wbGUtb3JnL2V3cy1zdGFuZC1pbgAuAAAAAACq{n:06}+Tk/Zm9sZGVyLWlkAAA=");
        let parent_of = |s: &crate::standin::State, distinguished: &str| {
            s.folders.iter().find(|f| f.distinguished == distinguished).map(|f| f.id.clone()).unwrap_or_default()
        };
        let (top, deleted) = (parent_of(&s, "msgfolderroot"), parent_of(&s, "deleteditems"));
        let folder = |n: usize, name: &str, parent: &str, class: &str| crate::standin::Folder {
            distinguished: String::new(),
            id: id_of(n),
            name: name.into(),
            total: 0,
            unread: 0,
            parent: parent.into(),
            class: class.into(),
        };
        s.folders.push(folder(300, "Team", &top, "IPF.Appointment"));
        s.folders.push(folder(301, "Unbenannt", &top, "IPF.Appointment"));
        s.folders.push(folder(302, "Old plans", &deleted, "IPF.Appointment"));
        s.folders.push(folder(303, "Kept", &deleted, "IPF.Note"));
        s.folders.push(folder(304, "Deeper", &id_of(303), "IPF.Appointment"));
        s.hidden_folders.push(id_of(301));
    }
    let client = crate::Client::new("http://127.0.0.1/EWS/Exchange.asmx", crate::Secret::new(crate::standin::AUTHORIZATION)).unwrap();
    let request = mailcraft_model::sync::EwsDirectoryRequest {
        account: 1,
        email: "ada@example.org".into(),
        url: "http://127.0.0.1/EWS/Exchange.asmx".into(),
        calendars: true,
        from: 1,
        to: 2_000_000_000,
        ..Default::default()
    };
    let cancel = std::sync::atomic::AtomicBool::new(false);
    let result = crate::standin::block_on(sync_directory(&client, &standin, &request, &cancel, &mut |_| true)).unwrap();
    assert!(result.errors.is_empty(), "{}", result.errors.join("; "));
    let names: Vec<String> = result.calendars.unwrap().calendars.iter().map(|c| c.name.clone()).collect();
    // (The default calendar first; it goes by the account's name, not the folder's.)
    assert_eq!(names, vec![String::new(), "Team".to_string()]);
}
