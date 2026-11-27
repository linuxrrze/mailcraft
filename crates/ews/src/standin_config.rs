// SPDX-FileCopyrightText: MailCraft contributors
// SPDX-License-Identifier: MIT OR Apache-2.0
//! The stand-in's configuration items: `GetUserConfiguration` ([MS-OXWSUSRCFG]) of the calendar
//! folder's `CategoryList`, held to the schema (a `UserConfigurationName` with its `Name` and the
//! folder it is in, then `UserConfigurationProperties`). [`State::category_list`] is the list's
//! `XmlData`, as base64; without one the mailbox has none, which Exchange answers with
//! `ErrorItemNotFound`, as a mailbox whose categories were never changed does.

use crate::soap::{MESSAGES, TYPES};
use crate::standin::{Check, Refusal, State, error_message, invalid, response};
use crate::xml::{Node, escape};

impl State {
    pub(crate) fn get_user_configuration(&self, operation: &Node) -> Result<(u16, String, String), Refusal> {
        Check::is(operation, MESSAGES, "GetUserConfiguration", &[])?;
        let parts = Check::children(operation, MESSAGES, &["UserConfigurationName", "UserConfigurationProperties"])?;
        let name = Check::required(operation, parts.first().copied().flatten(), "UserConfigurationName")?;
        Check::is(name, MESSAGES, "UserConfigurationName", &["Name"])?;
        let folder = Check::children(name, TYPES, &["FolderId", "DistinguishedFolderId"])?;
        let properties = Check::required(operation, parts.get(1).copied().flatten(), "UserConfigurationProperties")?;
        let asked: Vec<&str> = properties.text.split_whitespace().collect();
        if asked.is_empty() || !asked.iter().all(|one| matches!(*one, "Id" | "Dictionary" | "XmlData" | "BinaryData" | "All")) {
            return Err(invalid("UserConfigurationProperties names what the schema has"));
        }
        let in_calendar = folder.get(1).copied().flatten().is_some_and(|node| node.attribute("Id") == Some("calendar"));
        let known = Check::attribute(name, "Name")? == "CategoryList" && in_calendar;
        let message = match self.category_list.as_deref().filter(|_| known) {
            None => error_message("GetUserConfiguration", "ErrorItemNotFound"),
            Some(data) => format!(
                "<m:GetUserConfigurationResponseMessage ResponseClass=\"Success\"><m:ResponseCode>NoError</m:ResponseCode><m:UserConfiguration>\
                 <t:UserConfigurationName Name=\"CategoryList\"><t:DistinguishedFolderId Id=\"calendar\"/></t:UserConfigurationName>\
                 <t:XmlData>{}</t:XmlData></m:UserConfiguration></m:GetUserConfigurationResponseMessage>",
                escape(data)
            ),
        };
        let code = if message.contains("ErrorItemNotFound") { "ErrorItemNotFound" } else { "NoError" };
        Ok((200, code.into(), response("GetUserConfiguration", &message)))
    }
}
