// SPDX-FileCopyrightText: MailCraft contributors
// SPDX-License-Identifier: MIT OR Apache-2.0
//! The calendars other people shared with the owner, as Outlook keeps them: its navigation
//! shortcuts ([MS-OXOCFG] 2.2.9), hidden messages (`IPM.Microsoft.WunderBar.Link`) of the mailbox's
//! Common Views folder ([MS-OXOSFLD] 3.1.1.1, a folder of the root that EWS has no distinguished
//! name for, so it is found by its name). A shortcut of the calendar section
//! (`PidTagWlinkSection` 3) to a folder another user owns (`PidTagWlinkType` 2) names that user by
//! their Address Book EntryID (`PidTagWlinkAddressBookEID`, [MS-OXCDATA] 2.2.5.2), whose X500 DN
//! the server is asked to turn into an address (`ResolveNames`, the directory alone).
//!
//! Reading changes nothing, and only the owner's own mailbox is read. What the server says is
//! hostile: at most [`MAX_SHORTCUTS`] shortcuts are read, binary values are decoded to a bound,
//! a DN is taken only as printable ASCII of [`MAX_DN`] bytes at most, and a name is one line.

use crate::error::Error;
use crate::reply::Reply;
use crate::soap::{Element, MESSAGES, TYPES};
use crate::xml::Node;

/// The most shortcuts read.
pub const MAX_SHORTCUTS: usize = 1_000;
/// The most shortcuts asked for in one page.
pub const PAGE: u32 = 200;
/// The longest X500 DN taken.
pub const MAX_DN: usize = 1_024;
/// The longest binary value decoded.
const MAX_BINARY: usize = 4_096;
/// The longest name kept, in characters.
const MAX_NAME: usize = 256;
/// The name of the folder the shortcuts are in ([MS-OXOSFLD] 3.1.1.1).
pub const COMMON_VIEWS: &str = "Common Views";
/// The message class of a navigation shortcut.
const CLASS: &str = "IPM.Microsoft.WunderBar.Link";

/// The properties read ([MS-OXPROPS]): `PidTagNormalizedSubject`, `PidTagWlinkType`,
/// `PidTagWlinkSection`, `PidTagWlinkFolderType`, `PidTagWlinkAddressBookEID`.
const SUBJECT: u32 = 0x0E1D;
const TYPE: u32 = 0x6849;
const SECTION: u32 = 0x6852;
const FOLDER_TYPE: u32 = 0x684F;
const ADDRESS_BOOK_EID: u32 = 0x6854;

/// `wblSharedFolder`: the shortcut points to a folder another user owns.
const SHARED_FOLDER: i64 = 2;
/// `wbsidCalendar`: the shortcut is grouped under Calendar.
const CALENDAR_SECTION: i64 = 3;
/// `CLSID_CalendarFolder` as the bytes of the property ({00067802-0000-0000-C000-000000000046}).
const CALENDAR_FOLDER: [u8; 16] = [0x02, 0x78, 0x06, 0x00, 0, 0, 0, 0, 0xC0, 0, 0, 0, 0, 0, 0, 0x46];
/// The provider of an Address Book EntryID ([MS-OXCDATA] 2.2.5.2).
const AB_PROVIDER: [u8; 16] = [0xDC, 0xA7, 0x40, 0xC8, 0xC0, 0x42, 0x10, 0x1A, 0xB4, 0xB9, 0x08, 0x00, 0x2B, 0x2F, 0xE1, 0x82];

/// A calendar another user shared with the owner, as a shortcut names it: what the shortcut is
/// called, and the X500 DN of the user whose calendar it is.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct SharedCalendar {
    pub name: String,
    pub dn: String,
}

/// One page of shortcuts: the shared calendars on it, and where the next page starts (`None`: the
/// last).
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Page {
    pub shared: Vec<SharedCalendar>,
    pub read: usize,
    pub next: Option<u32>,
}

/// `FindItem` for the hidden messages of the folder `folder` (the Common Views folder), `PAGE` at
/// a time from `offset`, with the properties a shortcut is read by.
pub fn find_shortcuts(folder: &str, offset: u32) -> Element {
    let extended = |tag: u32, kind: &str| Element::new("t:ExtendedFieldURI").attr("PropertyTag", &format!("0x{tag:04X}")).attr("PropertyType", kind);
    let shape = Element::new("m:ItemShape").child(Element::new("t:BaseShape").text("IdOnly")).child(
        Element::new("t:AdditionalProperties")
            .child(Element::new("t:FieldURI").attr("FieldURI", "item:ItemClass"))
            .child(extended(SUBJECT, "String"))
            .child(extended(TYPE, "Integer"))
            .child(extended(SECTION, "Integer"))
            .child(extended(FOLDER_TYPE, "Binary"))
            .child(extended(ADDRESS_BOOK_EID, "Binary")),
    );
    let page = Element::new("m:IndexedPageItemView")
        .attr("MaxEntriesReturned", &PAGE.to_string())
        .attr("Offset", &offset.to_string())
        .attr("BasePoint", "Beginning");
    Element::new("m:FindItem")
        .attr("Traversal", "Associated")
        .child(shape)
        .child(page)
        .child(Element::new("m:ParentFolderIds").child(Element::new("t:FolderId").attr("Id", folder)))
}

/// The shared calendars of one page of [`find_shortcuts`]'s answer.
pub fn read_shortcuts(reply: &Reply) -> Result<Page, Error> {
    let messages = reply.messages("FindItem")?;
    let [message] = messages.as_slice() else { return Err(Error::Unexpected) };
    let root = message.ok()?.child_in(MESSAGES, "RootFolder").ok_or(Error::Unexpected)?;
    let mut page = Page::default();
    if let Some(items) = root.child_in(TYPES, "Items") {
        for item in items.children.iter().filter(|item| *item.namespace == *TYPES).take(PAGE as usize) {
            page.read = page.read.saturating_add(1);
            if let Some(shared) = shared_calendar(item)
                && !page.shared.iter().any(|known| known.dn.eq_ignore_ascii_case(&shared.dn))
            {
                page.shared.push(shared);
            }
        }
    }
    let last = root.attribute("IncludesLastItemInRange").is_none_or(|last| !last.trim().eq_ignore_ascii_case("false"));
    page.next = if last { None } else { root.attribute("IndexedPagingOffset").and_then(|next| next.trim().parse::<u32>().ok()) };
    Ok(page)
}

/// The shared calendar a shortcut points to, when it is one: of the calendar section, to a folder
/// another user owns, of a calendar folder (when it says), with a DN that reads.
fn shared_calendar(item: &Node) -> Option<SharedCalendar> {
    let class = item.child_in(TYPES, "ItemClass").map(|class| class.text.trim()).unwrap_or_default();
    if !class.eq_ignore_ascii_case(CLASS) {
        return None;
    }
    let property = |tag: u32| {
        item.children
            .iter()
            .filter(|child| child.is(TYPES, "ExtendedProperty"))
            .find(|p| {
                p.child_in(TYPES, "ExtendedFieldURI").and_then(|uri| uri.attribute("PropertyTag")).and_then(crate::folder::property_tag) == Some(tag)
            })
            .and_then(|p| p.child_in(TYPES, "Value"))
            .map(|value| value.text.trim())
    };
    let number = |tag: u32| property(tag).and_then(|text| text.parse::<i64>().ok());
    if number(TYPE)? != SHARED_FOLDER || number(SECTION)? != CALENDAR_SECTION {
        return None;
    }
    if let Some(kind) = property(FOLDER_TYPE).and_then(|text| crate::b64::decode(text, MAX_BINARY))
        && kind.as_slice() != CALENDAR_FOLDER
    {
        return None;
    }
    let dn = property(ADDRESS_BOOK_EID).and_then(|text| crate::b64::decode(text, MAX_BINARY)).and_then(|eid| x500_dn(&eid))?;
    let name: String = property(SUBJECT)
        .unwrap_or_default()
        .split(|c: char| c.is_whitespace() || c.is_control())
        .filter(|w| !w.is_empty())
        .collect::<Vec<_>>()
        .join(" ");
    Some(SharedCalendar { name: name.chars().take(MAX_NAME).collect(), dn })
}

/// The X500 DN of an Address Book EntryID ([MS-OXCDATA] 2.2.5.2): four bytes of flags that are
/// nought, the address book's provider, version 1, a type, and the DN ending in a NUL. `None`
/// for anything else, and for a DN that isn't printable ASCII or is longer than [`MAX_DN`].
pub fn x500_dn(eid: &[u8]) -> Option<String> {
    let flags = eid.get(..4)?;
    let provider = eid.get(4..20)?;
    let version = eid.get(20..24)?;
    let _kind = eid.get(24..28)?;
    if flags != [0, 0, 0, 0] || provider != AB_PROVIDER || version != [1, 0, 0, 0] {
        return None;
    }
    let rest = eid.get(28..)?;
    let end = rest.iter().position(|b| *b == 0).unwrap_or(rest.len());
    let dn = rest.get(..end)?;
    if dn.is_empty() || dn.len() > MAX_DN || !dn.iter().all(|b| (0x20..0x7F).contains(b)) || dn.first() != Some(&b'/') {
        return None;
    }
    String::from_utf8(dn.to_vec()).ok()
}

#[cfg(test)]
#[path = "shortcuts_tests.rs"]
pub(crate) mod tests;
