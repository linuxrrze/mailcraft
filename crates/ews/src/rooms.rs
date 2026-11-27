// SPDX-FileCopyrightText: MailCraft contributors
// SPDX-License-Identifier: MIT OR Apache-2.0
//! The organisation's rooms (Room Finder): `GetRoomLists`, the lists of rooms the organisation's
//! address list has (one for each building, as a rule), and `GetRooms`, the rooms of one list
//! ([MS-OXWSROOM], Exchange 2010 and later). Each is a name and an address, which is what a
//! meeting books a room by. Asking reads; it changes nothing.
//!
//! What the server sends is hostile: at most [`MAX_ROOMS`] of either, each address once, each name
//! and address one line of bounded length.

use mailcraft_model::Address;

use crate::error::{Error, ResponseClass, ResponseCode};
use crate::reply::Reply;
use crate::soap::{Element, MESSAGES, TYPES};
use crate::xml::Node;

/// The most room lists, or rooms of a list, read from one answer.
pub const MAX_ROOMS: usize = 500;
/// The longest name kept, in characters.
const MAX_NAME: usize = 256;
/// The longest address kept (RFC 5321's path, less the brackets).
const MAX_ADDRESS: usize = 254;

/// The request `GetRoomLists`.
pub fn get_room_lists() -> Element {
    Element::new("m:GetRoomLists")
}

/// The request `GetRooms` for the list of rooms at `list` (its address).
pub fn get_rooms(list: &str) -> Element {
    Element::new("m:GetRooms").child(Element::new("m:RoomList").child(Element::new("t:EmailAddress").text(&one_line(list, MAX_ADDRESS))))
}

/// The room lists of the answer to [`get_room_lists`], in the server's order. An organisation
/// without any has none.
pub fn read_room_lists(reply: &Reply) -> Result<Vec<Address>, Error> {
    let message = reply.single("GetRoomLists")?;
    let node = message.ok()?;
    Ok(addresses(node.child_in(MESSAGES, "RoomLists").map(|lists| lists.children.as_slice()).unwrap_or_default().iter()))
}

/// The rooms of the answer to [`get_rooms`], in the server's order. A list the server doesn't know
/// (`ErrorNameResolutionNoResults`, or one with no rooms) has none.
pub fn read_rooms(reply: &Reply) -> Result<Vec<Address>, Error> {
    let message = reply.single("GetRooms")?;
    if message.class() == ResponseClass::Error && matches!(message.code(), ResponseCode::Other(code) if code == "ErrorNameResolutionNoResults") {
        return Ok(Vec::new());
    }
    let node = message.ok()?;
    let rooms = node.child_in(MESSAGES, "Rooms").map(|rooms| rooms.children.as_slice()).unwrap_or_default();
    // (Each `t:Room` holds its address as `t:Id`.)
    Ok(addresses(rooms.iter().filter(|room| room.is(TYPES, "Room")).filter_map(|room| room.child_in(TYPES, "Id"))))
}

/// The name and address of each `EmailAddressType` element, each address once.
fn addresses<'n>(nodes: impl Iterator<Item = &'n Node>) -> Vec<Address> {
    let mut out: Vec<Address> = Vec::new();
    for node in nodes {
        if out.len() >= MAX_ROOMS {
            break;
        }
        let text = |name: &str| node.child_in(TYPES, name).map_or("", |child| child.text.trim());
        let email = one_line(text("EmailAddress"), MAX_ADDRESS);
        let routing = text("RoutingType");
        if !email.contains('@') || !(routing.is_empty() || routing.eq_ignore_ascii_case("SMTP")) {
            continue;
        }
        if out.iter().any(|have| have.email.eq_ignore_ascii_case(&email)) {
            continue;
        }
        let name = one_line(text("Name"), MAX_NAME);
        out.push(Address::new(&name, &email));
    }
    out
}

/// One line of `text`, at most `most` characters, without control characters.
fn one_line(text: &str, most: usize) -> String {
    text.chars().filter(|c| !c.is_control()).take(most).collect::<String>().trim().to_owned()
}
