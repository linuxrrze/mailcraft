// SPDX-FileCopyrightText: MailCraft contributors
// SPDX-License-Identifier: MIT OR Apache-2.0
//! A shared mailbox opened through an Exchange server account (#482) and the online archive
//! (#1818) together: a shared mailbox's run never asks about an archive and none is added for it
//! (Outlook shows one only for an auto-mapped mailbox), and no request is of both.

use mailcraft_model::Id;
use mailcraft_model::sync::{ArchiveSeen, EwsMailResult};
use serde_json::json;

use crate::Session;
use crate::cmd::ews_mail::{apply_ews_mail, ews_mail_request};

const OWNER: &str = "ada@example.org";
const SHARED: &str = "support@example.org";
const URL: &str = "https://exchange.example.org/EWS/Exchange.asmx";

/// An Exchange server account with its online archive and a shared mailbox opened through it.
fn session() -> (Session, Id, Id, Id) {
    let mut s = Session::empty();
    let owner = s.run("account.add", &json!({"email": OWNER, "kind": "exchange", "ews": {"url": URL, "username": "CORP\\ada"}})).unwrap()["id"]
        .as_u64()
        .unwrap();
    let shared = s.run("account.addSharedMailbox", &json!({"account": owner, "address": SHARED})).unwrap()["id"].as_u64().unwrap();
    let present = EwsMailResult { account: owner, email: OWNER.into(), url: URL.into(), archive: Some(ArchiveSeen::Present), ..Default::default() };
    apply_ews_mail(&mut s, present).unwrap();
    let archive = s.mailbox.archive_of(owner).map(|a| a.id).expect("the owner's archive");
    (s, owner, shared, archive)
}

#[test]
fn a_shared_mailbox_never_asks_about_an_online_archive_nor_gets_one() {
    let (mut s, owner, shared, archive) = session();
    let request = ews_mail_request(&s, shared, false).unwrap();
    assert_eq!(request.mailbox, SHARED);
    assert!(!request.probe_archive && !request.archive, "{request:?}");
    // (Its owner still asks, and its owner's archive is read as its owner's, naming no other mailbox.)
    assert!(ews_mail_request(&s, owner, false).unwrap().probe_archive);
    let of_archive = ews_mail_request(&s, archive, false).unwrap();
    assert!(of_archive.archive && of_archive.mailbox.is_empty() && !of_archive.probe_archive);
    assert_eq!((of_archive.email.as_str(), of_archive.username.as_str()), (OWNER, "CORP\\ada"));
    // A result that says the shared mailbox has one (a host may say anything) adds none.
    let says = EwsMailResult { account: shared, email: SHARED.into(), url: URL.into(), archive: Some(ArchiveSeen::Present), ..Default::default() };
    apply_ews_mail(&mut s, says).unwrap();
    assert!(s.mailbox.archive_of(shared).is_none());
    assert_eq!(s.mailbox.accounts.iter().filter(|a| a.is_archive()).count(), 1);
    // And Show Online Archive is no way to it.
    assert!(s.run("account.archive.show", &json!({"account": shared})).is_err());
    assert!(!ews_mail_request(&s, shared, false).unwrap().probe_archive);
    // The folder pane: the owner, its archive, then the shared mailbox.
    let order: Vec<Id> = s.mailbox.pane_order().iter().map(|a| a.id).collect();
    assert_eq!(order, vec![owner, archive, shared]);
}

#[test]
fn no_request_is_of_a_shared_mailbox_and_an_archive_at_once() {
    let (mut s, owner, shared, archive) = session();
    // An online archive opens no shared mailbox.
    assert!(s.run("account.addSharedMailbox", &json!({"account": archive, "address": "team@example.org"})).is_err());
    // A file may say an account is both: it has no run.
    s.mailbox.account_mut(shared).unwrap().archive_of = Some(owner);
    assert!(ews_mail_request(&s, shared, false).is_err());
    // A shared mailbox said to be opened through an archive has none either.
    let a = s.mailbox.account_mut(shared).unwrap();
    (a.archive_of, a.shared_of) = (None, Some(archive));
    assert!(s.mailbox.ews_owner_of(shared).is_none());
    assert!(ews_mail_request(&s, shared, false).is_err());
}
