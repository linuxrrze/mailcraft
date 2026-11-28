// SPDX-FileCopyrightText: MailCraft contributors
// SPDX-License-Identifier: MIT OR Apache-2.0
//! Outlook's navigation shortcuts: the request, and the shared calendars made of the answer.

use crate::reply::Reply;
use crate::shortcuts::*;
use crate::soap::{MESSAGES, SOAP, TYPES};

pub(crate) fn encode(bytes: &[u8]) -> String {
    const ALPHABET: &[u8] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut out = String::new();
    for chunk in bytes.chunks(3) {
        let n = chunk.iter().enumerate().fold(0u32, |n, (i, b)| n | (u32::from(*b) << (16 - 8 * i)));
        for i in 0..4 {
            if i <= chunk.len() {
                out.push(ALPHABET[((n >> (18 - 6 * i)) & 63) as usize] as char);
            } else {
                out.push('=');
            }
        }
    }
    out
}

/// An Address Book EntryID for `dn`, as [MS-OXCDATA] 2.2.5.2 lays it out.
pub(crate) fn entry_id(dn: &str) -> Vec<u8> {
    let mut eid = vec![0, 0, 0, 0];
    eid.extend([0xDC, 0xA7, 0x40, 0xC8, 0xC0, 0x42, 0x10, 0x1A, 0xB4, 0xB9, 0x08, 0x00, 0x2B, 0x2F, 0xE1, 0x82]);
    eid.extend([1, 0, 0, 0, 0, 0, 0, 0]);
    eid.extend(dn.as_bytes());
    eid.push(0);
    eid
}

pub(crate) const CALENDAR: [u8; 16] = [0x02, 0x78, 0x06, 0x00, 0, 0, 0, 0, 0xC0, 0, 0, 0, 0, 0, 0, 0x46];

/// A shortcut as a server sends it: its class, name, type, section, folder type and owner.
pub(crate) fn shortcut(class: &str, name: &str, kind: i32, section: i32, folder: Option<&[u8]>, owner: Option<&[u8]>) -> String {
    let property = |tag: &str, kind: &str, value: &str| {
        format!(
            "<t:ExtendedProperty><t:ExtendedFieldURI PropertyTag=\"{tag}\" PropertyType=\"{kind}\"/><t:Value>{value}</t:Value></t:ExtendedProperty>"
        )
    };
    let mut out = format!("<t:Message><t:ItemId Id=\"s{kind}{section}\"/><t:ItemClass>{class}</t:ItemClass>");
    out.push_str(&property("0xe1d", "String", name));
    out.push_str(&property("0x6849", "Integer", &kind.to_string()));
    out.push_str(&property("0x6852", "Integer", &section.to_string()));
    if let Some(folder) = folder {
        out.push_str(&property("0x684f", "Binary", &encode(folder)));
    }
    if let Some(owner) = owner {
        out.push_str(&property("0x6854", "Binary", &encode(owner)));
    }
    out.push_str("</t:Message>");
    out
}

pub(crate) fn answer(items: &str, last: bool) -> Reply {
    let body = format!(
        "<?xml version=\"1.0\"?><s:Envelope xmlns:s=\"{SOAP}\" xmlns:m=\"{MESSAGES}\" xmlns:t=\"{TYPES}\"><s:Body><m:FindItemResponse><m:ResponseMessages>\
         <m:FindItemResponseMessage ResponseClass=\"Success\"><m:ResponseCode>NoError</m:ResponseCode>\
         <m:RootFolder IndexedPagingOffset=\"200\" IncludesLastItemInRange=\"{last}\"><t:Items>{items}</t:Items></m:RootFolder>\
         </m:FindItemResponseMessage></m:ResponseMessages></m:FindItemResponse></s:Body></s:Envelope>"
    );
    Reply::parse(body.as_bytes()).unwrap()
}

const CY: &str = "/o=Lumen/ou=Exchange Administrative Group (FYDIBOHF23SPDLT)/cn=Recipients/cn=cy";

#[test]
fn the_request_asks_the_common_views_folders_hidden_messages_for_what_a_shortcut_is_read_by() {
    let written = find_shortcuts("cv-folder", 0).write();
    assert!(written.starts_with("<m:FindItem Traversal=\"Associated\">"), "{written}");
    for tag in ["0x0E1D", "0x6849", "0x6852", "0x684F", "0x6854"] {
        assert!(written.contains(&format!("PropertyTag=\"{tag}\"")), "{tag}: {written}");
    }
    assert!(written.contains("<t:FolderId Id=\"cv-folder\"/>"));
    assert!(written.contains("MaxEntriesReturned=\"200\""));
}

#[test]
fn a_calendar_another_user_shared_is_read_with_its_owners_dn() {
    let items = [
        shortcut("IPM.Microsoft.WunderBar.Link", "Cy  Lumen", 2, 3, Some(&CALENDAR), Some(&entry_id(CY))),
        // The same person twice is one calendar.
        shortcut("IPM.Microsoft.WunderBar.Link", "Cy again", 2, 3, None, Some(&entry_id(&CY.to_uppercase()))),
        // The owner's own calendar, a shared folder of the mail, a header, another class, no owner.
        shortcut("IPM.Microsoft.WunderBar.Link", "Calendar", 0, 3, Some(&CALENDAR), None),
        shortcut("IPM.Microsoft.WunderBar.Link", "Their inbox", 2, 1, None, Some(&entry_id("/o=Lumen/cn=Recipients/cn=dee"))),
        shortcut("IPM.Microsoft.WunderBar.Link", "People's calendars", 4, 3, None, None),
        shortcut("IPM.Microsoft.WunderBar.SFInfo", "Search", 2, 3, None, Some(&entry_id("/o=Lumen/cn=Recipients/cn=eve"))),
        shortcut("IPM.Microsoft.WunderBar.Link", "Nobody", 2, 3, Some(&CALENDAR), None),
        // A shortcut that says it is to a contact folder is not a calendar, whatever its section.
        shortcut(
            "IPM.Microsoft.WunderBar.Link",
            "Their contacts",
            2,
            3,
            Some(&[0x01, 0x78, 0x06, 0, 0, 0, 0, 0, 0xC0, 0, 0, 0, 0, 0, 0, 0x46]),
            Some(&entry_id("/o=Lumen/cn=Recipients/cn=fay")),
        ),
    ]
    .concat();
    let page = read_shortcuts(&answer(&items, true)).unwrap();
    assert_eq!(page.shared, vec![SharedCalendar { name: "Cy Lumen".into(), dn: CY.into() }]);
    assert_eq!(page.read, 8);
    assert_eq!(page.next, None);
    // Not the last page: where the next starts.
    assert_eq!(read_shortcuts(&answer("", false)).unwrap().next, Some(200));
}

#[test]
fn only_an_address_book_entry_id_gives_a_dn() {
    assert_eq!(x500_dn(&entry_id(CY)).as_deref(), Some(CY));
    // Without its NUL at the end it reads as far as it goes.
    let mut open = entry_id(CY);
    open.pop();
    assert_eq!(x500_dn(&open).as_deref(), Some(CY));
    // Short, flags set, another provider, another version, no DN, not printable, not a DN.
    assert_eq!(x500_dn(&[0; 10]), None);
    let mut flagged = entry_id(CY);
    flagged[0] = 1;
    assert_eq!(x500_dn(&flagged), None);
    let mut other = entry_id(CY);
    other[4] = 0;
    assert_eq!(x500_dn(&other), None);
    let mut version = entry_id(CY);
    version[20] = 2;
    assert_eq!(x500_dn(&version), None);
    assert_eq!(x500_dn(&entry_id("")), None);
    assert_eq!(x500_dn(&entry_id("/o=Lumen/cn=\u{7}bell")), None);
    assert_eq!(x500_dn(&entry_id("cy@lumen.example")), None);
    assert_eq!(x500_dn(&entry_id(&format!("/{}", "a".repeat(2_000)))), None);
}
