// SPDX-FileCopyrightText: MailCraft contributors
// SPDX-License-Identifier: MIT OR Apache-2.0
//! The stand-in's rooms: `GetRoomLists` and `GetRooms` over [`State::room_lists`], as
//! [MS-OXWSROOM] documents them. Each answer is a message of its own (no `ResponseMessages`); a
//! list the stand-in doesn't have is `ErrorNameResolutionNoResults`.

use crate::soap::{MESSAGES, TYPES};
use crate::standin::{Check, Refusal, State, envelope, invalid};
use crate::xml::{Node, escape};

/// A list of rooms: its name and address, and its rooms', each a name and an address.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct RoomList {
    pub name: String,
    pub email: String,
    pub rooms: Vec<(String, String)>,
}

fn address(name: &str, email: &str, kind: &str) -> String {
    format!(
        "<t:Name>{}</t:Name><t:EmailAddress>{}</t:EmailAddress><t:RoutingType>SMTP</t:RoutingType><t:MailboxType>{kind}</t:MailboxType>",
        escape(name),
        escape(email)
    )
}

fn answer(operation: &str, class: &str, code: &str, inside: &str) -> String {
    envelope(&format!(
        "<m:{operation}Response ResponseClass=\"{class}\" xmlns:m=\"{MESSAGES}\" xmlns:t=\"{TYPES}\"><m:ResponseCode>{code}</m:ResponseCode>{inside}</m:{operation}Response>"
    ))
}

impl State {
    pub(crate) fn get_room_lists(&self, operation: &Node) -> Result<(u16, String, String), Refusal> {
        Check::is(operation, MESSAGES, "GetRoomLists", &[])?;
        Check::empty(operation)?;
        let lists: String =
            self.room_lists.iter().map(|list| format!("<t:Address>{}</t:Address>", address(&list.name, &list.email, "PublicDL"))).collect();
        Ok((200, "NoError".into(), answer("GetRoomLists", "Success", "NoError", &format!("<m:RoomLists>{lists}</m:RoomLists>"))))
    }

    pub(crate) fn get_rooms(&self, operation: &Node) -> Result<(u16, String, String), Refusal> {
        Check::is(operation, MESSAGES, "GetRooms", &[])?;
        let parts = Check::children(operation, MESSAGES, &["RoomList"])?;
        let list = Check::required(operation, parts.first().copied().flatten(), "RoomList")?;
        let inside = Check::children(list, TYPES, &["Name", "EmailAddress", "RoutingType", "MailboxType", "ItemId", "OriginalDisplayName"])?;
        let email = Check::required(list, inside.get(1).copied().flatten(), "EmailAddress")?.text.trim();
        if email.is_empty() {
            return Err(invalid("RoomList names an address"));
        }
        let Some(list) = self.room_lists.iter().find(|list| list.email.eq_ignore_ascii_case(email)) else {
            return Ok((200, "ErrorNameResolutionNoResults".into(), answer("GetRooms", "Error", "ErrorNameResolutionNoResults", "")));
        };
        let rooms: String =
            list.rooms.iter().map(|(name, email)| format!("<t:Room><t:Id>{}</t:Id></t:Room>", address(name, email, "Mailbox"))).collect();
        Ok((200, "NoError".into(), answer("GetRooms", "Success", "NoError", &format!("<m:Rooms>{rooms}</m:Rooms>"))))
    }
}
