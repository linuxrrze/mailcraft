// SPDX-FileCopyrightText: MailCraft contributors
// SPDX-License-Identifier: MIT OR Apache-2.0
//! The organisation's rooms (Room Finder): the requests, the readers, and the client against the
//! strict stand-in.

use crate::reply::Reply;
use crate::soap::{MESSAGES, SOAP, TYPES};
use crate::standin::{AUTHORIZATION, StandIn, block_on};
use crate::standin_rooms::RoomList;
use crate::*;

const URL: &str = "http://127.0.0.1:1/EWS/Exchange.asmx";

fn client() -> Client {
    Client::new(URL, Secret::new(AUTHORIZATION)).unwrap()
}

fn building() -> StandIn {
    let standin = StandIn::new();
    standin.state().room_lists = vec![
        RoomList {
            name: "Building 1".into(),
            email: "building-1@example.org".into(),
            rooms: vec![("Room 1.01".into(), "room-101@example.org".into()), ("Room 1.02".into(), "room-102@example.org".into())],
        },
        RoomList { name: "Building 2".into(), email: "building-2@example.org".into(), rooms: Vec::new() },
    ];
    standin
}

#[test]
fn the_lists_and_the_rooms_of_one_come_by_name_and_address() {
    let standin = building();
    let lists = block_on(client().room_lists(&standin)).unwrap().unwrap();
    let said: Vec<(&str, &str)> = lists.iter().map(|a| (a.name.as_str(), a.email.as_str())).collect();
    assert_eq!(said, [("Building 1", "building-1@example.org"), ("Building 2", "building-2@example.org")]);
    let rooms = block_on(client().rooms(&standin, "building-1@example.org")).unwrap().unwrap();
    let said: Vec<(&str, &str)> = rooms.iter().map(|a| (a.name.as_str(), a.email.as_str())).collect();
    assert_eq!(said, [("Room 1.01", "room-101@example.org"), ("Room 1.02", "room-102@example.org")]);
    assert!(block_on(client().rooms(&standin, "building-2@example.org")).unwrap().unwrap().is_empty());
    assert!(block_on(client().rooms(&standin, "nowhere@example.org")).unwrap().unwrap().is_empty(), "a list the server hasn't");
}

#[test]
fn an_organisation_without_room_lists_has_none() {
    assert!(block_on(client().room_lists(&StandIn::new())).unwrap().unwrap().is_empty());
}

fn reply(body: &str) -> Reply {
    let envelope = format!(
        "<?xml version=\"1.0\"?><s:Envelope xmlns:s=\"{SOAP}\" xmlns:m=\"{MESSAGES}\" xmlns:t=\"{TYPES}\"><s:Body>{body}</s:Body></s:Envelope>"
    );
    Reply::parse(envelope.as_bytes()).unwrap()
}

/// What doesn't read as a room's address is left out: no address, another routing, the same
/// address twice; a name is one line.
#[test]
fn a_hostile_answer_keeps_only_rooms_with_an_address_once() {
    let room = |name: &str, email: &str, routing: &str| {
        format!(
            "<t:Room><t:Id><t:Name>{name}</t:Name><t:EmailAddress>{email}</t:EmailAddress><t:RoutingType>{routing}</t:RoutingType></t:Id></t:Room>"
        )
    };
    let rooms = [
        room("No address", "", "SMTP"),
        room("Exchange's own", "/o=Org/cn=Room", "EX"),
        room("Room\nA", "room-a@example.org", "SMTP"),
        room("Room A again", "ROOM-A@example.org", "SMTP"),
    ]
    .concat();
    let answer = reply(&format!(
        "<m:GetRoomsResponse ResponseClass=\"Success\"><m:ResponseCode>NoError</m:ResponseCode><m:Rooms>{rooms}</m:Rooms></m:GetRoomsResponse>"
    ));
    let read = crate::rooms::read_rooms(&answer).unwrap();
    let said: Vec<(&str, &str)> = read.iter().map(|a| (a.name.as_str(), a.email.as_str())).collect();
    assert_eq!(said, [("RoomA", "room-a@example.org")]);
    let refused =
        reply("<m:GetRoomListsResponse ResponseClass=\"Error\"><m:ResponseCode>ErrorAccessDenied</m:ResponseCode></m:GetRoomListsResponse>");
    assert!(crate::rooms::read_room_lists(&refused).is_err());
}

#[test]
fn the_rooms_request_names_the_list_by_its_address() {
    assert_eq!(
        crate::rooms::get_rooms("building-1@example.org").write(),
        "<m:GetRooms><m:RoomList><t:EmailAddress>building-1@example.org</t:EmailAddress></m:RoomList></m:GetRooms>"
    );
    assert_eq!(crate::rooms::get_room_lists().write(), "<m:GetRoomLists/>");
}
