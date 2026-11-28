// SPDX-FileCopyrightText: MailCraft contributors
// SPDX-License-Identifier: MIT OR Apache-2.0
//! A Send/Receive of an Exchange server account's calendar reads Outlook's shortcuts to the
//! calendars other people shared with the owner: the Common Views folder found by its name under
//! the root, its hidden messages, and each person's DN turned into an address by the directory.
//! The stand-in answers everything else; this transport stands in for those three.

use std::future::Future;
use std::sync::Mutex;

use super::*;
use crate::shortcuts::tests::{CALENDAR, entry_id, shortcut};

const CY_DN: &str = "/o=Lumen/ou=Exchange Administrative Group (FYDIBOHF23SPDLT)/cn=Recipients/cn=cy";
const DEE_DN: &str = "/o=Lumen/ou=Exchange Administrative Group (FYDIBOHF23SPDLT)/cn=Recipients/cn=dee";

struct Shortcuts {
    standin: crate::standin::StandIn,
    /// Whether the mailbox has a Common Views folder; whether Dee's DN finds anybody.
    views: bool,
    dee_found: bool,
    asked: Mutex<Vec<String>>,
}

impl crate::http::HttpExchange for Shortcuts {
    fn exchange(&self, request: &crate::http::HttpRequest) -> impl Future<Output = Result<crate::http::HttpResponse, crate::http::TransportError>> {
        let body = String::from_utf8_lossy(request.body()).into_owned();
        let ok = r#"ResponseClass="Success"><m:ResponseCode>NoError</m:ResponseCode>"#;
        let answer = if body.contains(r#"<m:FindFolder Traversal="Shallow""#) && body.contains(r#"<t:DistinguishedFolderId Id="root"/>"#) {
            let views =
                if self.views { r#"<t:Folder><t:FolderId Id="cv-folder"/><t:DisplayName>Common Views</t:DisplayName></t:Folder>"# } else { "" };
            Some(crate::standin::response(
                "FindFolder",
                &format!(
                    r#"<m:FindFolderResponseMessage {ok}<m:RootFolder IncludesLastItemInRange="true"><t:Folders><t:Folder><t:FolderId Id="finder"/><t:DisplayName>Finder</t:DisplayName></t:Folder>{views}</t:Folders></m:RootFolder></m:FindFolderResponseMessage>"#
                ),
            ))
        } else if body.contains(r#"<m:FindItem Traversal="Associated""#) && body.contains(r#"<t:FolderId Id="cv-folder"/>"#) {
            let items = [
                shortcut("IPM.Microsoft.WunderBar.Link", "Cy Lumen", 2, 3, Some(&CALENDAR), Some(&entry_id(CY_DN))),
                shortcut("IPM.Microsoft.WunderBar.Link", "Dee", 2, 3, Some(&CALENDAR), Some(&entry_id(DEE_DN))),
                shortcut("IPM.Microsoft.WunderBar.Link", "Calendar", 0, 3, Some(&CALENDAR), None),
            ]
            .concat();
            Some(crate::standin::response(
                "FindItem",
                &format!(
                    r#"<m:FindItemResponseMessage {ok}<m:RootFolder IncludesLastItemInRange="true"><t:Items>{items}</t:Items></m:RootFolder></m:FindItemResponseMessage>"#
                ),
            ))
        } else if body.contains("<m:ResolveNames") {
            self.asked.lock().unwrap_or_else(|p| p.into_inner()).push(body.clone());
            let person = |name: &str, email: &str| {
                format!(
                    "<t:Resolution><t:Mailbox><t:Name>{name}</t:Name><t:EmailAddress>{email}</t:EmailAddress><t:RoutingType>SMTP</t:RoutingType><t:MailboxType>Mailbox</t:MailboxType></t:Mailbox></t:Resolution>"
                )
            };
            let set = if body.contains("cn=cy") {
                person("Cy Lumen", "cy@example.org")
            } else if self.dee_found {
                person("Dee", "dee@example.org")
            } else {
                String::new()
            };
            let (class, code) = if set.is_empty() { ("Error", "ErrorNameResolutionNoResults") } else { ("Success", "NoError") };
            Some(crate::standin::response(
                "ResolveNames",
                &format!(
                    r#"<m:ResolveNamesResponseMessage ResponseClass="{class}"><m:ResponseCode>{code}</m:ResponseCode><m:ResolutionSet TotalItemsInView="1" IncludesLastItemInRange="true">{set}</m:ResolutionSet></m:ResolveNamesResponseMessage>"#
                ),
            ))
        } else {
            None
        };
        let fallback = answer.is_none().then(|| self.standin.exchange(request));
        async move {
            match (answer, fallback) {
                (Some(answer), _) => Ok(crate::http::HttpResponse::new(200, answer.into_bytes())),
                (None, Some(fallback)) => fallback.await,
                (None, None) => Err(crate::http::TransportError::Unreachable),
            }
        }
    }
}

fn run(views: bool, dee_found: bool) -> (Option<mailcraft_model::sync::EwsSharedCalendars>, Vec<String>) {
    let server = Shortcuts { standin: crate::standin::StandIn::new(), views, dee_found, asked: Mutex::new(Vec::new()) };
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
    let result = crate::standin::block_on(sync_directory(&client, &server, &request, &cancel, &mut |_| true)).unwrap();
    let asked = server.asked.lock().unwrap_or_else(|p| p.into_inner()).clone();
    (result.shared_calendars, asked)
}

#[test]
fn a_send_receive_brings_the_calendars_outlook_lists_as_shared() {
    let (found, asked) = run(true, true);
    let found = found.expect("the shortcuts were read");
    assert!(found.complete);
    let people: Vec<(&str, &str)> = found.calendars.iter().map(|c| (c.address.as_str(), c.name.as_str())).collect();
    assert_eq!(people, vec![("cy@example.org", "Cy Lumen"), ("dee@example.org", "Dee")]);
    // Each person by the DN the shortcut names, in the directory alone.
    assert_eq!(asked.len(), 2);
    assert!(asked.iter().all(|body| body.contains("SearchScope=\"ActiveDirectory\"") && body.contains("/o=Lumen/")), "{asked:?}");
}

#[test]
fn a_person_the_directory_doesnt_find_leaves_the_list_incomplete() {
    let (found, _) = run(true, false);
    let found = found.unwrap();
    assert!(!found.complete, "Dee may still be in Outlook's list: nothing may be taken away for her");
    assert_eq!(found.calendars.len(), 1);
}

#[test]
fn a_mailbox_without_common_views_has_no_list() {
    let (found, asked) = run(false, true);
    assert_eq!(found, None);
    assert!(asked.is_empty());
}
