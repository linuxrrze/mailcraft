// SPDX-FileCopyrightText: MailCraft contributors
// SPDX-License-Identifier: MIT OR Apache-2.0
//! Writing to a calendar opened by address (#70 on an Exchange server): a new appointment is made
//! in its owner's calendar, named by the mailbox, with the owner as organizer; one day of a series
//! is looked for there too. A transport stands in for the server and keeps every request.

use std::future::{Future, ready};
use std::sync::Mutex;

use mailcraft_model::remote::{EventChangeKind, EventFields, EventOp, EventResult, EventTime, EventsUpload, opened_id};

use super::*;

/// Answers each operation the run sends as a server would that takes it, and keeps the requests.
#[derive(Default)]
struct Server {
    bodies: Mutex<Vec<String>>,
}

impl Server {
    fn bodies(&self) -> Vec<String> {
        self.bodies.lock().unwrap_or_else(|poisoned| poisoned.into_inner()).clone()
    }
}

impl crate::http::HttpExchange for Server {
    fn exchange(&self, request: &crate::http::HttpRequest) -> impl Future<Output = Result<crate::http::HttpResponse, crate::http::TransportError>> {
        let body = String::from_utf8_lossy(request.body()).into_owned();
        self.bodies.lock().unwrap_or_else(|poisoned| poisoned.into_inner()).push(body.clone());
        let ok = r#"ResponseClass="Success"><m:ResponseCode>NoError</m:ResponseCode>"#;
        let answer = if body.contains("<m:CreateItem") {
            crate::standin::response(
                "CreateItem",
                &format!(
                    r#"<m:CreateItemResponseMessage {ok}<m:Items><t:CalendarItem><t:ItemId Id="new-item" ChangeKey="k1"/></t:CalendarItem></m:Items></m:CreateItemResponseMessage>"#
                ),
            )
        } else if body.contains("<m:UpdateItem") {
            crate::standin::response(
                "UpdateItem",
                &format!(
                    r#"<m:UpdateItemResponseMessage {ok}<m:Items><t:CalendarItem><t:ItemId Id="new-item" ChangeKey="k2"/></t:CalendarItem></m:Items></m:UpdateItemResponseMessage>"#
                ),
            )
        } else if body.contains("<m:GetItem") {
            crate::standin::response(
                "GetItem",
                &format!(
                    r#"<m:GetItemResponseMessage {ok}<m:Items><t:CalendarItem><t:ItemId Id="series" ChangeKey="k"/><t:UID>uid-1</t:UID></t:CalendarItem></m:Items></m:GetItemResponseMessage>"#
                ),
            )
        } else if body.contains("<m:FindItem") {
            crate::standin::response(
                "FindItem",
                &format!(
                    r#"<m:FindItemResponseMessage {ok}<m:RootFolder TotalItemsInView="0" IncludesLastItemInRange="true"><t:Items/></m:RootFolder></m:FindItemResponseMessage>"#
                ),
            )
        } else {
            String::new()
        };
        ready(Ok(crate::http::HttpResponse::new(200, answer.into_bytes())))
    }
}

fn upload(op: EventOp) -> (Vec<String>, EventResult) {
    let server = Server::default();
    let client = crate::Client::new("http://127.0.0.1/EWS/Exchange.asmx", crate::Secret::new("test")).unwrap();
    let request = mailcraft_model::sync::EwsDirectoryRequest {
        account: 1,
        email: "ada@example.org".into(),
        url: "http://127.0.0.1/EWS/Exchange.asmx".into(),
        changes: Some(EventsUpload { ops: vec![op], ..Default::default() }),
        ..Default::default()
    };
    let cancel = std::sync::atomic::AtomicBool::new(false);
    let result = crate::standin::block_on(sync_directory(&client, &server, &request, &cancel, &mut |_| true)).unwrap();
    let outcome = result.uploaded.unwrap().outcomes[0].result;
    (server.bodies(), outcome)
}

#[test]
fn a_new_appointment_in_a_calendar_opened_by_address_goes_to_its_owners_calendar() {
    let op = EventOp {
        kind: EventChangeKind::Create,
        calendar: opened_id("cy@example.org"),
        fields: EventFields {
            title: Some("Review".into()),
            time: Some(EventTime { start: 1_800_000_000, end: 1_800_003_600, all_day: false }),
            ..Default::default()
        },
        ..Default::default()
    };
    let (bodies, result) = upload(op);
    assert_eq!(result, EventResult::Done);
    let create = bodies.iter().find(|b| b.contains("<m:CreateItem")).expect("a CreateItem");
    // Its owner's calendar, by the distinguished name in that mailbox; never the id made up here.
    assert!(create.contains(r#"<t:DistinguishedFolderId Id="calendar"><t:Mailbox><t:EmailAddress>cy@example.org</t:EmailAddress></t:Mailbox></t:DistinguishedFolderId>"#), "{create}");
    assert!(!create.contains("user:"), "{create}");
    // The organizer (the sender's properties) is the person whose calendar it is, not the account.
    assert!(create.matches("cy@example.org").count() > 1 && !create.contains("ada@example.org"), "{create}");
}

#[test]
fn one_day_of_a_series_in_a_calendar_opened_by_address_is_looked_for_there() {
    let op = EventOp {
        kind: EventChangeKind::Update,
        calendar: opened_id("cy@example.org"),
        series: "series".into(),
        occurrence: 1_800_000_000,
        fields: EventFields { title: Some("Moved".into()), ..Default::default() },
        ..Default::default()
    };
    let (bodies, result) = upload(op);
    let find = bodies.iter().find(|b| b.contains("<m:FindItem")).expect("the calendar is looked in");
    assert!(find.contains(r#"<t:DistinguishedFolderId Id="calendar"><t:Mailbox><t:EmailAddress>cy@example.org</t:EmailAddress>"#), "{find}");
    // (The stand-in's calendar has no such day: the change is let go of, as for the account's own.)
    assert_eq!(result, EventResult::Gone);
}

#[test]
fn the_folder_an_id_names() {
    let write = |folder: &str| crate::directory::folder_element(crate::directory::Kind::Contacts, folder).write();
    assert!(write("").contains(r#"<t:DistinguishedFolderId Id="contacts"/>"#), "{}", write(""));
    assert!(write("AAMkAD==").contains(r#"<t:FolderId Id="AAMkAD=="/>"#));
    assert!(write(&opened_id("cy@example.org")).contains("<t:Mailbox>"));
    // An id that only looks opened (no address after it) is a server's id as it stands.
    assert!(write("user:").contains(r#"<t:FolderId Id="user:"/>"#), "{}", write("user:"));
}
