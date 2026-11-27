// SPDX-FileCopyrightText: MailCraft contributors
// SPDX-License-Identifier: MIT OR Apache-2.0
//! Other people's free and busy time (Find a Time): the request, the reader and the client
//! against the strict stand-in, which holds the request to [MS-OXWSGTAV]'s schema.

use mailcraft_model::ShowAs;
use mailcraft_model::remote::{FreeBusyAsk, MAX_SCHEDULES};

use crate::reply::Reply;
use crate::soap::{MESSAGES, SOAP, TYPES};
use crate::standin::{AUTHORIZATION, StandIn, block_on};
use crate::standin_availability::Schedule;
use crate::*;

const URL: &str = "http://127.0.0.1:1/EWS/Exchange.asmx";
/// 2026-10-12T00:00:00Z, a Monday.
const DAY: i64 = 1_791_763_200;
const HOUR: i64 = 3_600;

fn client() -> Client {
    Client::new(URL, Secret::new(AUTHORIZATION)).unwrap()
}

fn ask(people: &[&str], from: i64, to: i64) -> FreeBusyAsk {
    FreeBusyAsk { people: people.iter().map(|p| (*p).to_owned()).collect(), from, to }
}

fn busy(start: &str, end: &str, kind: &str) -> (String, String, String) {
    (start.into(), end.into(), kind.into())
}

/// The stand-in's colleagues: Ben with a morning meeting, a tentative lunch, free time and a
/// meeting that starts the evening before; Cy out of office in the afternoon; Dee, who shares
/// nothing; Eve, of whom the server knows nothing for the day.
fn colleagues() -> StandIn {
    let standin = StandIn::new();
    standin.state().schedules = vec![
        Schedule {
            email: "ben@example.org".into(),
            busy: vec![
                busy("2026-10-11T22:00:00", "2026-10-12T01:00:00", "Busy"),
                busy("2026-10-12T09:00:00", "2026-10-12T10:30:00", "Busy"),
                busy("2026-10-12T12:00:00", "2026-10-12T13:00:00", "Tentative"),
                busy("2026-10-12T15:00:00", "2026-10-12T16:00:00", "Free"),
                busy("2026-10-14T09:00:00", "2026-10-14T10:00:00", "Busy"),
            ],
            denied: false,
        },
        Schedule {
            email: "cy@example.org".into(),
            busy: vec![
                busy("2026-10-12T13:00:00", "2026-10-12T17:00:00", "OOF"),
                busy("2026-10-12T17:00:00", "2026-10-12T18:00:00", "WorkingElsewhere"),
            ],
            denied: false,
        },
        Schedule { email: "dee@example.org".into(), busy: Vec::new(), denied: true },
        Schedule { email: "eve@example.org".into(), busy: vec![busy("2026-10-12T00:00:00", "2026-10-13T00:00:00", "NoData")], denied: false },
    ];
    standin
}

#[test]
fn a_question_asks_each_address_once_and_for_six_weeks_at_most() {
    let question =
        GetUserAvailability::new(&ask(&["Ben@Example.org", "ben@example.org", "not an address", " cy@example.org "], DAY, DAY + 90 * 86_400))
            .unwrap();
    assert_eq!(question.people(), ["Ben@Example.org", "cy@example.org"]);
    assert_eq!(question.window(), (DAY, DAY + 42 * 86_400));

    let many: Vec<String> = (0..50).map(|n| format!("p{n}@example.org")).collect();
    let many = GetUserAvailability::new(&FreeBusyAsk { people: many, from: DAY, to: DAY + 86_400 }).unwrap();
    assert_eq!(many.people().len(), MAX_SCHEDULES);

    assert!(GetUserAvailability::new(&ask(&["nobody"], DAY, DAY + 86_400)).is_none(), "nobody to ask");
    assert!(GetUserAvailability::new(&ask(&["ben@example.org"], DAY, DAY)).is_none(), "no time to ask about");
}

/// The request names UTC, each mailbox as a required attendee, and the `FreeBusy` view: never
/// `Detailed`, which would carry what each stretch is.
#[test]
fn the_request_asks_in_utc_for_the_free_busy_view_alone() {
    let written = GetUserAvailability::new(&ask(&["ben@example.org"], DAY + 9 * HOUR, DAY + 17 * HOUR)).unwrap().element().write();
    assert!(written.starts_with("<m:GetUserAvailabilityRequest><t:TimeZone><t:Bias>0</t:Bias><t:StandardTime>"), "{written}");
    assert!(written.contains("<t:StartTime>2026-10-12T09:00:00</t:StartTime><t:EndTime>2026-10-12T17:00:00</t:EndTime>"), "{written}");
    assert!(written.contains("<t:Address>ben@example.org</t:Address></t:Email><t:AttendeeType>Required</t:AttendeeType>"), "{written}");
    assert!(written.contains("<t:RequestedView>FreeBusy</t:RequestedView>"), "{written}");
    assert!(!written.contains("Detailed"), "{written}");
}

#[test]
fn each_persons_time_is_read_cut_to_the_window_with_how_it_shows() {
    let standin = colleagues();
    let question = GetUserAvailability::new(&ask(&["ben@example.org", "cy@example.org"], DAY, DAY + 86_400)).unwrap();
    let answer = block_on(client().availability(&standin, &question)).unwrap().unwrap();
    assert_eq!(standin.state().availability_asked, 1, "the stand-in took the request as the schema has it");
    assert_eq!((answer.from, answer.to), (DAY, DAY + 86_400));
    let [ben, cy] = answer.people.as_slice() else { panic!("two people: {answer:?}") };

    assert_eq!(ben.email, "ben@example.org");
    assert!(ben.error.is_empty());
    let ben_times: Vec<_> = ben.busy.iter().map(|t| (t.start - DAY, t.end - DAY, t.show_as)).collect();
    assert_eq!(
        ben_times,
        [
            // From the evening before: cut to the day asked about.
            (0, HOUR, ShowAs::Busy),
            (9 * HOUR, 10 * HOUR + 1_800, ShowAs::Busy),
            (12 * HOUR, 13 * HOUR, ShowAs::Tentative),
            // (Free time is no taken time; the meeting two days on is outside the window.)
        ]
    );

    let cy_times: Vec<_> = cy.busy.iter().map(|t| (t.start - DAY, t.end - DAY, t.show_as)).collect();
    assert_eq!(cy_times, [(13 * HOUR, 17 * HOUR, ShowAs::OutOfOffice), (17 * HOUR, 18 * HOUR, ShowAs::WorkingElsewhere)]);
}

/// Somebody the server can't look up says why, in order with the others, and the rest are read.
#[test]
fn someone_who_cant_be_looked_up_says_why_and_the_others_are_read() {
    let standin = colleagues();
    let question =
        GetUserAvailability::new(&ask(&["dee@example.org", "zed@example.org", "eve@example.org", "ben@example.org"], DAY, DAY + 86_400)).unwrap();
    let answer = block_on(client().availability(&standin, &question)).unwrap().unwrap();
    let said: Vec<_> = answer.people.iter().map(|p| (p.email.as_str(), p.error.as_str(), p.busy.len())).collect();
    assert_eq!(
        said,
        [
            ("dee@example.org", "their time couldn't be looked up (ErrorNoFreeBusyAccess)", 0),
            ("zed@example.org", "their time couldn't be looked up (ErrorMailRecipientNotFound)", 0),
            ("eve@example.org", "their time couldn't be looked up (no information)", 0),
            ("ben@example.org", "", 3),
        ]
    );
}

fn reply(body: &str) -> Reply {
    let envelope = format!(
        "<?xml version=\"1.0\"?><s:Envelope xmlns:s=\"{SOAP}\" xmlns:m=\"{MESSAGES}\" xmlns:t=\"{TYPES}\"><s:Body>\
         <m:GetUserAvailabilityResponse>{body}</m:GetUserAvailabilityResponse></s:Body></s:Envelope>"
    );
    Reply::parse(envelope.as_bytes()).unwrap()
}

fn one_person(events: &str) -> String {
    format!(
        "<m:FreeBusyResponseArray><m:FreeBusyResponse><m:ResponseMessage ResponseClass=\"Success\"><m:ResponseCode>NoError</m:ResponseCode>\
         </m:ResponseMessage><m:FreeBusyView><t:FreeBusyViewType>FreeBusy</t:FreeBusyViewType><t:CalendarEventArray>{events}\
         </t:CalendarEventArray></m:FreeBusyView></m:FreeBusyResponse></m:FreeBusyResponseArray>"
    )
}

/// A server's answer is hostile: times that don't read, that end before they start, a busy type
/// the schema hasn't, an offset of its own, are each left out or read for what they say; answers
/// for more people than were asked about name nobody.
#[test]
fn a_hostile_answer_keeps_only_what_reads() {
    let question = GetUserAvailability::new(&ask(&["ben@example.org"], DAY, DAY + 86_400)).unwrap();
    let event = |start: &str, end: &str, kind: &str| {
        format!("<t:CalendarEvent><t:StartTime>{start}</t:StartTime><t:EndTime>{end}</t:EndTime><t:BusyType>{kind}</t:BusyType></t:CalendarEvent>")
    };
    let events = [
        event("yesterday", "2026-10-12T10:00:00", "Busy"),
        event("2026-10-12T11:00:00", "2026-10-12T10:00:00", "Busy"),
        event("2026-10-12T11:00:00", "2026-10-12T12:00:00", "Asleep"),
        // An offset of its own: 14:00 at +02:00 is 12:00 UTC.
        event("2026-10-12T14:00:00+02:00", "2026-10-12T15:00:00+02:00", "Busy"),
    ]
    .concat();
    let doubled = one_person(&events).replace("</m:FreeBusyResponseArray>", "").replace("<m:FreeBusyResponseArray>", "");
    let answer = question.answer(&reply(&format!("<m:FreeBusyResponseArray>{doubled}{doubled}</m:FreeBusyResponseArray>"))).unwrap();
    let [ben] = answer.people.as_slice() else { panic!("only the one asked about: {answer:?}") };
    let times: Vec<_> = ben.busy.iter().map(|t| (t.start - DAY, t.end - DAY, t.show_as)).collect();
    assert_eq!(times, [(12 * HOUR, 13 * HOUR, ShowAs::Busy)]);

    assert!(matches!(question.answer(&reply("")), Err(Error::Unexpected)), "no answers at all");
    assert!(matches!(question.answer(&reply("<m:FreeBusyResponseArray/>")), Err(Error::Unexpected)));
}

/// A busy server is the whole question's: it is asked again after its back-off.
#[test]
fn a_busy_server_is_the_whole_questions() {
    let question = GetUserAvailability::new(&ask(&["ben@example.org"], DAY, DAY + 86_400)).unwrap();
    let busy = reply(
        "<m:FreeBusyResponseArray><m:FreeBusyResponse><m:ResponseMessage ResponseClass=\"Error\"><m:ResponseCode>ErrorServerBusy</m:ResponseCode>\
         <m:MessageXml><t:Value Name=\"BackOffMilliseconds\">5000</t:Value></m:MessageXml></m:ResponseMessage></m:FreeBusyResponse></m:FreeBusyResponseArray>",
    );
    assert!(matches!(question.answer(&busy), Err(Error::Busy { back_off_ms: 5_000 })), "{:?}", question.answer(&busy));
}

/// Exchange refuses a `FreeBusy` window longer than 42 days outright (as the stand-in does): a
/// longer question is cut to it, and answered.
#[test]
fn a_long_question_is_cut_to_what_the_server_answers() {
    let standin = colleagues();
    let question = GetUserAvailability::new(&ask(&["ben@example.org"], DAY, DAY + 60 * 86_400)).unwrap();
    let answer = block_on(client().availability(&standin, &question)).unwrap().unwrap();
    assert_eq!(answer.to, DAY + 42 * 86_400);
    assert_eq!(answer.people.first().map(|p| p.busy.len()), Some(4), "the meeting two days on too");
}
