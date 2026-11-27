// SPDX-FileCopyrightText: MailCraft contributors
// SPDX-License-Identifier: MIT OR Apache-2.0
//! The mailbox's own list of categories, with their colours: the configuration item
//! `CategoryList` of the calendar folder, read with `GetUserConfiguration` ([MS-OXWSUSRCFG]); its
//! `XmlData` is the list as [MS-OXOCFG] writes it, a `categories` element with a `category` for
//! each, whose `name` and `color` (-1 for none, 0 to 24 for the preset colours) are all that is
//! read. Reading it changes nothing.
//!
//! What the server sends is hostile: the data is decoded to [`MAX_DATA`] bytes at most, parsed by
//! the crate's bounded reader, and [`MAX_CATEGORIES`] names are kept, each one line of
//! [`MAX_NAME`] characters at most.

use mailcraft_model::Category;

use crate::error::Error;
use crate::reply::Reply;
use crate::soap::{Element, MESSAGES, TYPES};

/// The most base64 text of the list that is decoded: what the reader keeps of an `XmlData`.
pub const MAX_DATA: usize = crate::xml::MAX_XML_DATA;
/// The most categories kept.
pub const MAX_CATEGORIES: usize = 500;
/// The longest name kept, in characters.
const MAX_NAME: usize = 255;

/// The request `GetUserConfiguration` for the list of categories, its `XmlData` alone.
pub fn get_category_list() -> Element {
    Element::new("m:GetUserConfiguration")
        .child(
            Element::new("m:UserConfigurationName")
                .attr("Name", "CategoryList")
                .child(Element::new("t:DistinguishedFolderId").attr("Id", "calendar")),
        )
        .child(Element::new("m:UserConfigurationProperties").text("XmlData"))
}

/// The categories of the answer, in the list's order, each name once (whatever its case). A
/// mailbox that has no list yet (`ErrorItemNotFound`) has none; a list that doesn't read is
/// [`Error::Unexpected`].
pub fn read_category_list(reply: &Reply) -> Result<Vec<Category>, Error> {
    let messages = reply.messages("GetUserConfiguration")?;
    let [message] = messages.as_slice() else { return Err(Error::Unexpected) };
    let node = match message.ok() {
        Ok(node) => node,
        Err(Error::Server(crate::error::ResponseCode::ItemNotFound)) => return Ok(Vec::new()),
        Err(error) => return Err(error),
    };
    let data = node.child_in(MESSAGES, "UserConfiguration").and_then(|config| config.child_in(TYPES, "XmlData"));
    let Some(data) = data else { return Ok(Vec::new()) };
    let bytes = crate::b64::decode(data.text.trim(), MAX_DATA).ok_or(Error::Unexpected)?;
    // (A byte-order mark the server may write first is no part of the document.)
    let bytes = bytes.strip_prefix(b"\xEF\xBB\xBF".as_slice()).unwrap_or(&bytes);
    let root = crate::xml::parse(bytes).map_err(|_| Error::Unexpected)?;
    if root.name != "categories" {
        return Err(Error::Unexpected);
    }
    let mut out: Vec<Category> = Vec::new();
    for category in root.children.iter().filter(|child| child.name == "category") {
        if out.len() >= MAX_CATEGORIES {
            break;
        }
        let name: String = category.attribute("name").unwrap_or_default().chars().filter(|c| !c.is_control()).take(MAX_NAME).collect();
        let name = name.trim();
        if name.is_empty() || out.iter().any(|have| have.name.to_lowercase() == name.to_lowercase()) {
            continue;
        }
        let color = match category.attribute("color").and_then(|color| color.trim().parse::<i32>().ok()) {
            Some(preset @ 0..=24) => mailcraft_model::outlook_colors::outlook_color(&format!("preset{preset}")),
            _ => mailcraft_model::outlook_colors::NO_COLOR,
        };
        out.push(Category { name: name.to_owned(), color });
    }
    Ok(out)
}
