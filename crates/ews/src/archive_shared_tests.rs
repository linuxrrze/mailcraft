// SPDX-FileCopyrightText: MailCraft contributors
// SPDX-License-Identifier: MIT OR Apache-2.0
//! A shared mailbox (#482) and the online archive (#1818) together: a shared mailbox's run never
//! asks whether it has an archive, and a request that says it is of a shared mailbox's archive is
//! refused before anything is asked. (Outlook shows a shared mailbox's archive only when the
//! mailbox is auto-mapped, which MailCraft doesn't do.)

use std::sync::atomic::AtomicBool;

use mailcraft_model::sync::{ArchiveSeen, EwsMailRequest, EwsMailResult};

use crate::standin::{AUTHORIZATION, StandIn, block_on};
use crate::*;

const URL: &str = "https://mail.example.org/EWS/Exchange.asmx";
const SHARED: &str = "support@example.org";

fn client() -> Client {
    Client::new(URL, Secret::new(AUTHORIZATION)).unwrap()
}

fn request() -> EwsMailRequest {
    EwsMailRequest { email: "ada@example.org".into(), url: URL.into(), list_folders: true, probe_archive: true, ..Default::default() }
}

fn run_as(client: &Client, standin: &StandIn, request: &EwsMailRequest) -> Vec<EwsMailResult> {
    let mut pieces = Vec::new();
    block_on(sync_mail(client, standin, request, &AtomicBool::new(false), &mut |_| true, &mut |piece| pieces.push(piece))).unwrap();
    pieces
}

/// How many requests named the archive's top of the mail.
fn archive_asks(standin: &StandIn) -> usize {
    standin.state().requests.iter().filter(|r| r.body.contains("archivemsgfolderroot")).count()
}

#[test]
fn a_shared_mailboxs_run_never_asks_whether_it_has_an_online_archive() {
    let standin = StandIn::new();
    standin.state().mailbox = Some(SHARED.into());
    standin.state().add_archive(true);
    let pieces = run_as(&client().with_mailbox(SHARED), &standin, &EwsMailRequest { mailbox: SHARED.into(), ..request() });
    let last = pieces.last().unwrap();
    assert!(last.whole && last.errors.is_empty(), "{last:?}");
    assert_eq!(last.archive, None, "nothing is said of an archive");
    assert_eq!(archive_asks(&standin), 0, "the server wasn't asked about one");
    // (The same request of the caller's own mailbox does ask: the probe is there to be kept out.)
    let own = StandIn::new();
    own.state().add_archive(true);
    let pieces = run_as(&client(), &own, &request());
    assert_eq!(pieces.last().unwrap().archive, Some(ArchiveSeen::Present));
    assert!(archive_asks(&own) > 0);
}

#[test]
fn a_run_of_a_shared_mailboxs_archive_is_refused_before_anything_is_asked() {
    let standin = StandIn::new();
    standin.state().mailbox = Some(SHARED.into());
    standin.state().add_archive(true);
    let both = EwsMailRequest { mailbox: SHARED.into(), archive: true, ..request() };
    let pieces = run_as(&client().with_mailbox(SHARED), &standin, &both);
    assert_eq!(pieces.len(), 1);
    let last = pieces.last().unwrap();
    assert_eq!(last.errors, vec![crate::run::ARCHIVE_OF_SHARED.to_owned()]);
    assert!(!last.whole && last.archive.is_none() && last.folders.is_none() && last.pages.is_empty(), "{last:?}");
    assert!(standin.state().requests.is_empty(), "nothing was sent");
}
