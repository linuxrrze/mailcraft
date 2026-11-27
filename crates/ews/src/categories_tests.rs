// SPDX-FileCopyrightText: MailCraft contributors
// SPDX-License-Identifier: MIT OR Apache-2.0
//! The mailbox's list of categories: the request, and what is made of the server's answer.

use mailcraft_model::outlook_colors::{NO_COLOR, outlook_color};

use crate::categories::*;
use crate::reply::Reply;
use crate::soap::{MESSAGES, SOAP, TYPES};

fn answer(message: &str) -> Reply {
    let body = format!(
        "<?xml version=\"1.0\"?><s:Envelope xmlns:s=\"{SOAP}\" xmlns:m=\"{MESSAGES}\" xmlns:t=\"{TYPES}\"><s:Body><m:GetUserConfigurationResponse>\
         <m:ResponseMessages>{message}</m:ResponseMessages></m:GetUserConfigurationResponse></s:Body></s:Envelope>"
    );
    Reply::parse(body.as_bytes()).unwrap()
}

/// A list as [MS-OXOCFG] writes it, as the server sends it (base64 in `XmlData`).
fn with_list(xml: &str) -> Reply {
    answer(&format!(
        "<m:GetUserConfigurationResponseMessage ResponseClass=\"Success\"><m:ResponseCode>NoError</m:ResponseCode><m:UserConfiguration>\
         <t:UserConfigurationName Name=\"CategoryList\"><t:DistinguishedFolderId Id=\"calendar\"/></t:UserConfigurationName>\
         <t:XmlData>{}</t:XmlData></m:UserConfiguration></m:GetUserConfigurationResponseMessage>",
        encode(xml.as_bytes())
    ))
}

fn encode(bytes: &[u8]) -> String {
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

#[test]
fn the_request_asks_for_the_calendar_folders_category_list_alone() {
    let written = get_category_list().write();
    assert_eq!(
        written,
        "<m:GetUserConfiguration><m:UserConfigurationName Name=\"CategoryList\"><t:DistinguishedFolderId Id=\"calendar\"/></m:UserConfigurationName>\
         <m:UserConfigurationProperties>XmlData</m:UserConfigurationProperties></m:GetUserConfiguration>"
    );
}

#[test]
fn the_categories_come_with_their_colours_in_order() {
    let list = "\u{feff}<?xml version=\"1.0\"?><categories default=\"Red category\" xmlns=\"CategoryList.xsd\">\
                <category name=\"Red category\" color=\"0\" keyboardShortcut=\"0\"/>\
                <category name=\"Travel\" color=\"7\"/>\
                <category name=\"travel\" color=\"3\"/>\
                <category name=\"Plain\" color=\"-1\"/>\
                <category name=\"Odd\" color=\"99\"/>\
                <category name=\"  \" color=\"1\"/>\
                <category color=\"2\"/>\
                </categories>";
    let read = read_category_list(&with_list(list)).unwrap();
    let said: Vec<_> = read.iter().map(|c| (c.name.as_str(), c.color)).collect();
    assert_eq!(
        said,
        [("Red category", outlook_color("preset0")), ("Travel", outlook_color("preset7")), ("Plain", NO_COLOR), ("Odd", NO_COLOR)],
        "each name once, a colour out of the 25 none, a name of nothing nothing"
    );
}

#[test]
fn a_mailbox_without_a_list_has_none_and_a_list_that_doesnt_read_is_an_error() {
    let none = answer(
        "<m:GetUserConfigurationResponseMessage ResponseClass=\"Error\"><m:ResponseCode>ErrorItemNotFound</m:ResponseCode>\
         </m:GetUserConfigurationResponseMessage>",
    );
    assert_eq!(read_category_list(&none).unwrap(), []);
    assert!(read_category_list(&with_list("<notcategories/>")).is_err());
    assert!(read_category_list(&with_list("<categories><category name=")).is_err());
    let many: String = (0..MAX_CATEGORIES + 50).map(|n| format!("<category name=\"c{n}\" color=\"1\"/>")).collect();
    assert_eq!(read_category_list(&with_list(&format!("<categories>{many}</categories>"))).unwrap().len(), MAX_CATEGORIES);
}
