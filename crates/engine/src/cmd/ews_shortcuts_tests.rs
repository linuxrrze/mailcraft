// SPDX-FileCopyrightText: MailCraft contributors
// SPDX-License-Identifier: MIT OR Apache-2.0
//! The calendars Outlook lists as shared with an Exchange server account's owner (its navigation
//! shortcuts), as the engine takes them in: added unticked under Shared Calendars, never again
//! once the owner removed one, and gone again when Outlook's list no longer has them.

use mailcraft_model::sync::{EwsDirectoryResult, EwsSharedCalendar, EwsSharedCalendars};
use serde_json::json;

use super::*;

const ME: &str = "ada@example.org";

fn session() -> (Session, Id) {
    let mut s = Session::empty();
    let added =
        s.run("account.add", &json!({"email": ME, "kind": "exchange", "ews": {"url": "https://exchange.example.org/EWS/Exchange.asmx"}})).unwrap();
    (s, added["id"].as_u64().unwrap())
}

fn found(account: Id, people: &[(&str, &str)], complete: bool) -> EwsDirectoryResult {
    EwsDirectoryResult {
        account,
        email: ME.into(),
        shared_calendars: Some(EwsSharedCalendars {
            calendars: people.iter().map(|(address, name)| EwsSharedCalendar { address: (*address).into(), name: (*name).into() }).collect(),
            complete,
        }),
        ..Default::default()
    }
}

fn opened(s: &Session, account: Id) -> Vec<(String, String, bool, bool)> {
    s.mailbox.account(account).unwrap().opened_calendars.iter().map(|o| (o.address.clone(), o.name.clone(), o.ticked, o.shortcut)).collect()
}

#[test]
fn a_calendar_outlook_lists_as_shared_is_added_unticked_and_once() {
    let (mut s, account) = session();
    let answer = apply_ews_directory(&mut s, &found(account, &[("cy@example.org", "Cy Lumen"), (ME, "Me"), ("not an address", "X")], true)).unwrap();
    assert_eq!(answer["sharedCalendarsAdded"], 1);
    assert_eq!(opened(&s, account), vec![("cy@example.org".into(), "Cy Lumen".into(), false, true)]);
    // Again: nothing more.
    let answer = apply_ews_directory(&mut s, &found(account, &[("CY@example.org", "Cy")], true)).unwrap();
    assert_eq!(answer["sharedCalendarsAdded"], 0);
    assert_eq!(opened(&s, account).len(), 1);
}

#[test]
fn one_the_owner_removed_is_not_added_again_until_they_open_it() {
    let (mut s, account) = session();
    apply_ews_directory(&mut s, &found(account, &[("cy@example.org", "Cy")], true)).unwrap();
    s.run("calendar.removeShared", &json!({"address": "cy@example.org"})).unwrap();
    apply_ews_directory(&mut s, &found(account, &[("cy@example.org", "Cy")], true)).unwrap();
    assert!(opened(&s, account).is_empty(), "removed here, it stays removed");
    // Opened by the owner: theirs, and not taken away when Outlook's list doesn't have it.
    s.run("calendar.openShared", &json!({"address": "cy@example.org", "account": account})).unwrap();
    assert!(s.mailbox.account(account).unwrap().dismissed_shortcuts.is_empty());
    apply_ews_directory(&mut s, &found(account, &[], true)).unwrap();
    assert_eq!(opened(&s, account), vec![("cy@example.org".into(), "cy@example.org".into(), true, false)]);
}

#[test]
fn one_outlook_no_longer_lists_goes_only_when_the_list_is_whole() {
    let (mut s, account) = session();
    apply_ews_directory(&mut s, &found(account, &[("cy@example.org", "Cy"), ("dee@example.org", "Dee")], true)).unwrap();
    // A list that couldn't be read whole takes nothing away.
    apply_ews_directory(&mut s, &found(account, &[("dee@example.org", "Dee")], false)).unwrap();
    assert_eq!(opened(&s, account).len(), 2);
    let answer = apply_ews_directory(&mut s, &found(account, &[("dee@example.org", "Dee")], true)).unwrap();
    assert_eq!(answer["sharedCalendarsRemoved"], 1);
    assert_eq!(opened(&s, account), vec![("dee@example.org".into(), "Dee".into(), false, true)]);
}

#[test]
fn a_shortcut_doesnt_add_past_the_limit_or_for_another_kind_of_account() {
    let (mut s, account) = session();
    let many: Vec<(String, String)> = (0..40).map(|n| (format!("p{n}@example.org"), format!("P{n}"))).collect();
    let many: Vec<(&str, &str)> = many.iter().map(|(a, n)| (a.as_str(), n.as_str())).collect();
    apply_ews_directory(&mut s, &found(account, &many, true)).unwrap();
    assert_eq!(opened(&s, account).len(), mailcraft_model::MAX_OPENED_CALENDARS);
    // Not an Exchange server account: nothing.
    let mut s = Session::empty();
    let other = s.run("account.add", &json!({"email": "bo@example.org", "kind": "imap", "imap": {"host": "imap.example.org"}})).unwrap()["id"]
        .as_u64()
        .unwrap();
    assert_eq!(
        crate::cmd::calendar_opened::from_shortcuts(&mut s, other, &found(other, &[("cy@example.org", "Cy")], true).shared_calendars.unwrap()),
        (0, 0)
    );
}
