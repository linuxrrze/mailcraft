// SPDX-FileCopyrightText: MailCraft contributors
// SPDX-License-Identifier: MIT OR Apache-2.0
//! What the directory operations read: the shapes of `FindItem`, `FindFolder` and the three kinds
//! of item, held to what the model keeps.

use mailcraft_model::remote::{EventFields, RemoteEventKind};

use super::*;
use crate::reply::Reply;
use crate::soap::{MESSAGES, SOAP, TYPES};

fn answer(operation: &str, message: &str) -> Reply {
    let body = format!(
        "<?xml version=\"1.0\"?><s:Envelope xmlns:s=\"{SOAP}\" xmlns:m=\"{MESSAGES}\" xmlns:t=\"{TYPES}\"><s:Body><m:{operation}Response>\
         <m:ResponseMessages><m:{operation}ResponseMessage ResponseClass=\"Success\"><m:ResponseCode>NoError</m:ResponseCode>{message}\
         </m:{operation}ResponseMessage></m:ResponseMessages></m:{operation}Response></s:Body></s:Envelope>"
    );
    Reply::parse(body.as_bytes()).unwrap()
}

fn find_items(items: &str) -> ItemsPage {
    let reply = answer(
        "FindItem",
        &format!("<m:RootFolder IncludesLastItemInRange=\"true\" TotalItemsInView=\"1\"><t:Items>{items}</t:Items></m:RootFolder>"),
    );
    FindItems::new(Kind::Calendar, "folder", 0, 100).answer(&reply).unwrap()
}

#[test]
fn a_calendar_item_is_read_as_an_event() {
    let page = find_items(
        "<t:CalendarItem><t:ItemId Id=\"event-1\" ChangeKey=\"key-1\"/><t:Subject>Budget review</t:Subject>\
         <t:Body BodyType=\"Text\">Numbers</t:Body><t:Start>2026-10-12T13:00:00Z</t:Start><t:End>2026-10-12T14:00:00Z</t:End>\
         <t:IsAllDayEvent>false</t:IsAllDayEvent><t:Location>Room 1</t:Location><t:LegacyFreeBusyStatus>Busy</t:LegacyFreeBusyStatus>\
         <t:IsResponseRequested>true</t:IsResponseRequested><t:AllowNewTimeProposal>true</t:AllowNewTimeProposal>\
         <t:CalendarItemType>Single</t:CalendarItemType><t:UID>uid-1</t:UID><t:Categories><t:String>Work</t:String></t:Categories>\
         <t:RequiredAttendees><t:Attendee><t:Mailbox><t:Name>Cy</t:Name><t:EmailAddress>cy@example.org</t:EmailAddress><t:RoutingType>SMTP</t:RoutingType></t:Mailbox><t:ResponseType>Accept</t:ResponseType></t:Attendee></t:RequiredAttendees>\
         </t:CalendarItem>",
    );
    assert!(page.last);
    let Item::Event(event) = &page.items[0] else { panic!("not an event") };
    assert_eq!(event.id, "event-1");
    assert_eq!(event.title, "Budget review");
    assert_eq!(event.start, mailcraft_time::iso_to_timestamp("2026-10-12T13:00:00Z", 0).unwrap());
    assert_eq!(event.end, mailcraft_time::iso_to_timestamp("2026-10-12T14:00:00Z", 0).unwrap());
    assert_eq!(event.organizer, None);
    assert_eq!(event.attendees.len(), 1);
    assert_eq!(event.attendees[0].address.email, "cy@example.org");
    assert_eq!(event.categories, ["Work"]);
    assert_eq!(event.kind, RemoteEventKind::Single);
}

#[test]
fn a_contact_and_a_to_do_are_read() {
    let reply = answer(
        "FindItem",
        "<m:RootFolder><t:Items><t:Contact><t:ItemId Id=\"c-1\" ChangeKey=\"k\"/><t:DisplayName>Grace Hopper</t:DisplayName>\
         <t:GivenName>Grace</t:GivenName><t:Surname>Hopper</t:Surname><t:CompanyName>Navy</t:CompanyName>\
         <t:EmailAddresses><t:Entry Key=\"EmailAddress1\">grace@example.org</t:Entry></t:EmailAddresses>\
         <t:PhoneNumbers><t:Entry Key=\"MobilePhone\">+1 555 0100</t:Entry></t:PhoneNumbers>\
         <t:PhysicalAddresses><t:Entry Key=\"Business\"><t:Street>1 Navy Way</t:Street><t:City>Arlington</t:City></t:Entry></t:PhysicalAddresses>\
         </t:Contact></t:Items></m:RootFolder>",
    );
    let page = FindItems::new(Kind::Contacts, "folder", 0, 100).answer(&reply).unwrap();
    let Item::Contact(contact) = &page.items[0] else { panic!("not a contact") };
    assert_eq!(contact.display_name, "Grace Hopper");
    assert_eq!(contact.emails[0].value, "grace@example.org");
    assert_eq!(contact.phones[0].label, "mobile");
    assert_eq!(contact.addresses[0].city, "Arlington");

    let reply = answer(
        "FindItem",
        "<m:RootFolder><t:Items><t:Task><t:ItemId Id=\"t-1\" ChangeKey=\"k\"/><t:Subject>Send the report</t:Subject>\
         <t:StartDate>2026-10-01T00:00:00Z</t:StartDate><t:DueDate>2026-10-03T00:00:00Z</t:DueDate>\
         <t:Status>Completed</t:Status><t:CompleteDate>2026-10-02T12:00:00Z</t:CompleteDate><t:Importance>High</t:Importance>\
         </t:Task></t:Items></m:RootFolder>",
    );
    let page = FindItems::new(Kind::Tasks, "folder", 0, 100).answer(&reply).unwrap();
    let Item::Task(task) = &page.items[0] else { panic!("not a to-do") };
    assert_eq!(task.title, "Send the report");
    assert!(task.completed.is_some());
    assert_eq!(task.importance, mailcraft_model::Importance::High);
}

#[test]
fn a_folder_listing_keeps_its_class_and_rights() {
    let reply = answer(
        "FindFolder",
        "<m:RootFolder IncludesLastItemInRange=\"true\"><t:Folders><t:CalendarFolder><t:FolderId Id=\"cal-2\"/><t:DisplayName>Team</t:DisplayName>\
         <t:FolderClass>IPF.Appointment</t:FolderClass><t:EffectiveRights><t:Read>true</t:Read><t:Modify>false</t:Modify></t:EffectiveRights>\
         </t:CalendarFolder></t:Folders></m:RootFolder>",
    );
    let page = crate::folder::FindFolder::new(crate::folder::DistinguishedFolder::MessageRoot, true, 0, 100).answer(&reply).unwrap();
    assert!(page.last);
    assert_eq!(page.folders[0].display_name, "Team");
    assert_eq!(page.folders[0].class, "IPF.Appointment");
    assert_eq!(page.folders[0].rights, mailcraft_model::Rights::Read);
}

#[test]
fn an_empty_answer_has_no_items() {
    let page = find_items("");
    assert!(page.items.is_empty());
    assert!(page.last);
}

/// A whole-day event at midnight in its own zone is the evening before in UTC; the instant is
/// kept as it came and the day is read in that zone, not by UTC's date.
#[test]
fn an_all_day_event_keeps_the_instant_and_the_day_of_its_zone() {
    let page = find_items(
        "<t:CalendarItem><t:ItemId Id=\"event-2\" ChangeKey=\"key-2\"/><t:Subject>Holiday</t:Subject>\
         <t:Start>2026-11-22T23:00:00Z</t:Start><t:End>2026-11-23T23:00:00Z</t:End>\
         <t:IsAllDayEvent>true</t:IsAllDayEvent><t:CalendarItemType>Single</t:CalendarItemType><t:UID>uid-2</t:UID>\
         <t:StartTimeZone Id=\"W. Europe Standard Time\" Name=\"W. Europe Standard Time\"/></t:CalendarItem>",
    );
    let Some(Item::Event(event)) = page.items.first() else { panic!("an event") };
    assert!(event.all_day);
    assert_eq!(event.start_zone, "W. Europe Standard Time");
    assert_eq!((event.start, event.end), (1_795_388_400, 1_795_474_800), "the instants the server gave");
    let day = all_day_day("2026-11-22T23:00:00Z", "W. Europe Standard Time");
    assert_eq!(day.map(|day| (day.year, day.month, day.day)), Some((2026, 11, 23)), "midnight there, not UTC's date");
}

/// Only the people a server can name go in a meeting: an address in SMTP form, or a directory
/// name as `EX`. A blank field or a bare name is not sent (a server answers that the people
/// couldn't be resolved, as Grommunio does).
#[test]
fn only_the_people_a_server_can_name_are_written() {
    let people = vec![
        Attendee { address: Address::new("Ada", "ada@example.org"), optional: false, response: Response::None },
        Attendee { address: Address::new("No Address", ""), optional: false, response: Response::None },
        Attendee {
            address: Address::new("Directory", "/o=example/ou=Exchange Administrative Group/cn=Someone"),
            optional: true,
            response: Response::None,
        },
    ];
    let event = RemoteEvent { title: "Review".into(), start: 1_795_000_000, end: 1_795_001_800, attendees: people, ..Default::default() };
    let written = calendar_item(&event, "", None).write();
    assert!(written.contains("<t:EmailAddress>ada@example.org</t:EmailAddress>"), "the address: {written}");
    assert!(written.contains("<t:RoutingType>SMTP</t:RoutingType>"), "as SMTP: {written}");
    assert!(written.contains("<t:RoutingType>EX</t:RoutingType>"), "the directory name as EX: {written}");
    assert!(!written.contains("<t:EmailAddress></t:EmailAddress>"), "no blank invitee: {written}");
    assert!(!written.contains("No Address"), "nothing of one without an address: {written}");
}

/// The same for a change: only the people the server can name are set, and a list that has none
/// of them is cleared rather than written half.
#[test]
fn a_change_writes_only_the_people_a_server_can_name() {
    let fields = EventFields {
        attendees: Some(vec![
            Attendee { address: Address::new("Ada", "ada@example.org"), optional: false, response: Response::None },
            Attendee { address: Address::new("No Address", ""), optional: false, response: Response::None },
        ]),
        ..Default::default()
    };
    let written = update_event(&fields, "event-1", "key-1", false, "").write();
    assert!(written.contains("calendar:RequiredAttendees"), "the list is set: {written}");
    assert!(written.contains("ada@example.org"), "the address: {written}");
    assert!(!written.contains("No Address"), "nothing of one without an address: {written}");
    let none = EventFields { attendees: Some(vec![people_without_an_address()]), ..Default::default() };
    let written = update_event(&none, "event-1", "key-1", false, "").write();
    assert!(written.contains("calendar:RequiredAttendees") && written.contains("DeleteItemField"), "cleared, not half written: {written}");
}

fn people_without_an_address() -> Attendee {
    Attendee { address: Address::new("No Address", ""), optional: false, response: Response::None }
}

/// An appointment made here names this account as its sender (`PR_SENT_REPRESENTING_*`), so that
/// the server keeps an organizer: Grommunio keeps none for a plain appointment made over EWS,
/// and then its window says the participants can't be resolved.
#[test]
fn an_appointment_made_here_names_this_account_as_its_sender() {
    let event = RemoteEvent { title: "Review".into(), start: 1_795_000_000, end: 1_795_001_800, ..Default::default() };
    let written = calendar_item(&event, "", Some(&Address::new("", "ada@example.org"))).write();
    assert!(written.contains("PropertyTag=\"0x0042\""), "the name: {written}");
    assert!(written.contains("PropertyTag=\"0x0065\""), "the address: {written}");
    assert!(written.contains("PropertyTag=\"0x0064\""), "how it is named: {written}");
    assert!(written.contains("<t:Value>ada@example.org</t:Value>"), "the account: {written}");
    let plain = calendar_item(&event, "", None).write();
    assert!(!plain.contains("0x0042") && !plain.contains("0x0065"), "nothing of the sender when there is none: {plain}");
}

/// A plain appointment says it is not a meeting (`PidLidAppointmentStateFlags` = 0): a server
/// keeps no such value for one made over EWS, and an older Grommunio reads it with
/// `getItemProp`, fails with E-3127 when it is absent, and then leaves the item out of
/// `FindItem` (the event seems to vanish). A meeting leaves the value to the server.
#[test]
fn a_plain_appointment_says_it_is_not_a_meeting() {
    let plain = RemoteEvent { title: "Lunch".into(), start: 1_795_000_000, end: 1_795_001_800, ..Default::default() };
    let written = calendar_item(&plain, "", None).write();
    assert!(written.contains("PropertyId=\"33303\""), "the appointment state flags: {written}");
    assert!(written.contains("PropertySetId=\"00062002-0000-0000-C000-000000000046\""), "in the appointment's own set: {written}");
    // Before Start, where the item's own part of the schema puts extended properties.
    let flags = written.find("33303").unwrap_or(usize::MAX);
    let start = written.find("<t:Start>").unwrap_or(0);
    assert!(flags < start, "before the calendar's own fields: {written}");

    let meeting = RemoteEvent {
        title: "Review".into(),
        start: 1_795_000_000,
        end: 1_795_001_800,
        attendees: vec![Attendee { address: Address::new("Ada", "ada@example.org"), optional: false, response: Response::None }],
        ..Default::default()
    };
    let written = calendar_item(&meeting, "", None).write();
    assert!(!written.contains("33303"), "the server sets the flags of a meeting: {written}");
}

fn recurring(pattern: &str, range: &str) -> RemoteEvent {
    let page = find_items(&format!(
        "<t:CalendarItem><t:ItemId Id=\"series-1\" ChangeKey=\"k\"/><t:Subject>Stand-up</t:Subject>\
         <t:Start>2026-10-12T08:00:00Z</t:Start><t:End>2026-10-12T08:15:00Z</t:End>\
         <t:CalendarItemType>RecurringMaster</t:CalendarItemType>\
         <t:Recurrence>{pattern}{range}</t:Recurrence></t:CalendarItem>"
    ));
    let Some(Item::Event(event)) = page.items.into_iter().next() else { panic!("not an event") };
    *event
}

/// `DaysOfWeek` may name a class of days ([MS-OXWSCDATA] `DayOfWeekType`): `Weekday` is Monday
/// to Friday, `WeekendDay` Saturday and Sunday, `Day` every day; each day is kept once.
#[test]
fn a_weekly_pattern_reads_the_schemas_classes_of_days() {
    let range = "<t:NoEndRecurrence><t:StartDate>2026-10-12</t:StartDate></t:NoEndRecurrence>";
    let weekdays = recurring("<t:WeeklyRecurrence><t:Interval>1</t:Interval><t:DaysOfWeek>Weekday</t:DaysOfWeek></t:WeeklyRecurrence>", range);
    let rule = weekdays.recurrence.unwrap();
    assert_eq!(rule.freq, Freq::Weekly);
    assert_eq!(rule.by_day, [Weekday::Mon, Weekday::Tue, Weekday::Wed, Weekday::Thu, Weekday::Fri]);

    let weekend = recurring(
        "<t:WeeklyRecurrence><t:Interval>2</t:Interval><t:DaysOfWeek>Saturday WeekendDay Sunday</t:DaysOfWeek></t:WeeklyRecurrence>",
        range,
    );
    assert_eq!(weekend.recurrence.unwrap().by_day, [Weekday::Sun, Weekday::Sat]);
}

/// An `AbsoluteYearlyRecurrence` has no interval in the schema: it is every year.
#[test]
fn an_absolute_yearly_pattern_is_read_as_every_year() {
    let event = recurring(
        "<t:AbsoluteYearlyRecurrence><t:DayOfMonth>12</t:DayOfMonth><t:Month>October</t:Month></t:AbsoluteYearlyRecurrence>",
        "<t:NumberedRecurrence><t:StartDate>2026-10-12</t:StartDate><t:NumberOfOccurrences>5</t:NumberOfOccurrences></t:NumberedRecurrence>",
    );
    let rule = event.recurrence.unwrap();
    assert_eq!(rule.freq, Freq::Yearly);
    assert_eq!(rule.interval, 1);
    assert_eq!(rule.count, Some(5));
}

fn series(rule: Recurrence) -> RemoteEvent {
    RemoteEvent {
        title: "Review".into(),
        // 2026-10-12T22:30Z: already the 13th east of UTC, so the series' own day must win.
        start: mailcraft_time::iso_to_timestamp("2026-10-12T22:30:00Z", 0).unwrap(),
        end: mailcraft_time::iso_to_timestamp("2026-10-12T23:00:00Z", 0).unwrap(),
        recurrence: Some(rule),
        series_day: Date::new(2026, 10, 13),
        ..Default::default()
    }
}

/// A range's `StartDate` and `EndDate` are `xs:date` values, and the pattern's day and month are
/// the series' first day as the calendar has it, not the UTC day of its start.
#[test]
fn a_series_is_written_with_dates_of_its_own_first_day() {
    let rule = Recurrence { freq: Freq::Monthly, interval: 1, until: Date::new(2027, 3, 13), ..Default::default() };
    let written = calendar_item(&series(rule), "W. Europe Standard Time", None).write();
    assert!(written.contains("<t:DayOfMonth>13</t:DayOfMonth>"), "the series' own day: {written}");
    assert!(written.contains("<t:StartDate>2026-10-13</t:StartDate>"), "an xs:date start: {written}");
    assert!(written.contains("<t:EndDate>2027-03-13</t:EndDate>"), "an xs:date end: {written}");
    assert!(!written.contains("Z</t:StartDate>") && !written.contains("Z</t:EndDate>"), "no time in the range: {written}");
}

/// Every N years becomes every 12·N months, since `AbsoluteYearlyRecurrence` has no interval;
/// every year stays yearly, with no interval written.
#[test]
fn a_yearly_series_every_few_years_is_written_in_months() {
    let every_two = calendar_item(&series(Recurrence { freq: Freq::Yearly, interval: 2, ..Default::default() }), "", None).write();
    assert!(every_two.contains("<t:AbsoluteMonthlyRecurrence><t:Interval>24</t:Interval><t:DayOfMonth>13</t:DayOfMonth>"), "{every_two}");

    let every_one = calendar_item(&series(Recurrence { freq: Freq::Yearly, interval: 1, ..Default::default() }), "", None).write();
    assert!(
        every_one.contains("<t:AbsoluteYearlyRecurrence><t:DayOfMonth>13</t:DayOfMonth><t:Month>October</t:Month></t:AbsoluteYearlyRecurrence>"),
        "{every_one}"
    );
}

/// A server that validates the request refuses an element out of the schema's order
/// (`ErrorSchemaValidation`): the item's own part ([MS-OXWSCORE] `ItemType`) first, then the
/// calendar's ([MS-OXWSCAL] `CalendarItemType`).
#[test]
fn an_events_elements_follow_the_schemas_order() {
    let mut event = series(Recurrence { freq: Freq::Daily, interval: 1, count: Some(3), ..Default::default() });
    event.private = true;
    event.categories = vec!["Work".into()];
    event.reminder_minutes = Some(15);
    event.location = "Room 1".into();
    event.attendees = vec![Attendee { address: Address::new("Cy", "cy@example.org"), optional: false, response: Response::None }];
    let written = calendar_item(&event, "W. Europe Standard Time", None).write();
    let order = [
        "<t:Subject>",
        "<t:Sensitivity>",
        "<t:Body ",
        "<t:Categories>",
        "<t:Importance>",
        "<t:ReminderIsSet>",
        "<t:ReminderMinutesBeforeStart>",
        "<t:Start>",
        "<t:End>",
        "<t:IsAllDayEvent>",
        "<t:LegacyFreeBusyStatus>",
        "<t:Location>",
        "<t:IsResponseRequested>",
        "<t:RequiredAttendees>",
        "<t:Recurrence>",
        "<t:StartTimeZone ",
        "<t:EndTimeZone ",
        "<t:AllowNewTimeProposal>",
    ];
    let places: Vec<usize> = order.iter().map(|tag| written.find(tag).unwrap_or_else(|| panic!("{tag} missing: {written}"))).collect();
    for (pair, tags) in places.windows(2).zip(order.windows(2)) {
        assert!(pair[0] < pair[1], "{} before {}: {written}", tags[0], tags[1]);
    }
}

/// An answer the owner chose not to send is only saved, out of the way in Deleted Items, without
/// the note; one that is sent goes with a copy kept.
#[test]
fn an_answer_not_sent_is_saved_without_its_note() {
    let quiet = meeting_answer(Answer::Accept, "meeting-1", "See you", false).write();
    assert!(quiet.contains("MessageDisposition=\"SaveOnly\""), "{quiet}");
    assert!(quiet.contains("Id=\"deleteditems\""), "{quiet}");
    assert!(!quiet.contains("See you"), "{quiet}");

    let sent = meeting_answer(Answer::Decline, "meeting-1", "See you", true).write();
    assert!(sent.contains("MessageDisposition=\"SendAndSaveCopy\""), "{sent}");
    assert!(sent.contains("<t:DeclineItem><t:Body BodyType=\"Text\">See you</t:Body>"), "{sent}");
    assert!(!sent.contains("SavedItemFolderId"), "{sent}");
}

/// An item the server lists but that can't be read still takes its place in the folder: the
/// next page starts past it, or where the server says, never back over what was listed.
#[test]
fn the_next_page_starts_past_every_item_listed() {
    let reply = answer(
        "FindItem",
        "<m:RootFolder IncludesLastItemInRange=\"false\" IndexedPagingOffset=\"3\"><t:Items>\
         <t:CalendarItem><t:Subject>No id, so not read</t:Subject></t:CalendarItem>\
         <t:CalendarItem><t:ItemId Id=\"e-2\" ChangeKey=\"k\"/><t:Subject>Read</t:Subject>\
         <t:Start>2026-10-12T13:00:00Z</t:Start><t:End>2026-10-12T14:00:00Z</t:End></t:CalendarItem>\
         <t:CalendarItem><t:Subject>Nor this</t:Subject></t:CalendarItem>\
         </t:Items></m:RootFolder>",
    );
    let page = FindItems::new(Kind::Calendar, "folder", 0, 100).answer(&reply).unwrap();
    assert!(!page.last);
    assert_eq!(page.items.len(), 1);
    assert_eq!(page.listed, 3);
    assert_eq!(page.next, Some(3));
    assert_eq!(page.next_offset(0), Some(3));

    // Without the server's word, past what was listed.
    let unsaid = ItemsPage { listed: 3, ..Default::default() };
    assert_eq!(unsaid.next_offset(10), Some(13));
    // A server offset that doesn't move forward is not followed.
    let stuck = ItemsPage { listed: 3, next: Some(10), ..Default::default() };
    assert_eq!(stuck.next_offset(10), Some(13));
    // A page that lists nothing ends the walk.
    let empty = ItemsPage { next: Some(20), ..Default::default() };
    assert_eq!(empty.next_offset(10), None);
}
