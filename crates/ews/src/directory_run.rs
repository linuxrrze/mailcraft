// SPDX-FileCopyrightText: MailCraft contributors
// SPDX-License-Identifier: MIT OR Apache-2.0
//! A directory run for an Exchange server account: the calendar changes, contact changes and
//! to-do changes made here go up, then the account's calendars, address books and to-do lists
//! (and those it was given by address) come down ([MS-OXWSCORE], [MS-OXWSCAL], [MS-OXWSCONT],
//! [MS-OXWSTASK]).
//!
//! It speaks through a [`Client`] over any [`HttpExchange`] and reads no clock and starts no
//! thread. A busy server is waited for as in a mail run ([`crate::run::sync_mail`]).
//!
//! **The calendar** is read as a `CalendarView` for the stretch asked for: a recurring series
//! comes as its occurrences, and the series itself is read beside them in a second pass so that
//! a rule the mailbox can keep is kept. **The contacts and to-dos** are read whole, a page at a
//! time.
//!
//! **Another mailbox's calendar or address book** is read with the owner's sign-in by putting its
//! address in every folder request (`t:Mailbox`, [MS-OXWSFOLD]), never by an administrator's
//! right: the server answers only what the owner was given access to.

use std::sync::atomic::AtomicBool;

use mailcraft_model::CalendarOwner;
use mailcraft_model::remote::{
    CalendarsDownload, CategoriesDownload, ContactOutcome, ContactsDownload, ContactsUploaded, EventOutcome, EventResult, EventsDownload,
    EventsUploaded, RemoteAddressBook, RemoteCalendar, RemoteContact, RemoteEvent, RemoteEventKind,
};
use mailcraft_model::remote_tasks::{RemoteTask, RemoteTaskList, TaskOpKind, TaskOutcome, TaskResult, TasksDownload, TasksUploaded};

use crate::client::Client;
use crate::directory::{self, Answer, Item, ItemUp, Kind};
use crate::error::ResponseCode;
use crate::folder::{DistinguishedFolder, FindFolder, FolderInfo, GetFolder};
use crate::http::HttpExchange;
use crate::run::{Asked, Run, Stop};
use crate::soap::Element;

/// How many pages of one listing are followed, and how many items one run takes in.
const MAX_PAGES: usize = 200;
const MAX_ITEMS: usize = 20_000;
/// Most changes one run carries of each kind.
const MAX_CHANGES: usize = 5_000;
/// Most `GetUserAvailability` questions for one calendar shared for free and busy times alone:
/// the stretch read (a month back and a year ahead, or the days on screen) in stretches of 42
/// days, with room to spare.
const MAX_AVAILABILITY_ASKS: usize = 10;

/// Read the directory of `request`: the changes first, then the folders and what they hold.
pub async fn sync_directory<E: HttpExchange>(
    client: &Client,
    exchange: &E,
    request: &mailcraft_model::sync::EwsDirectoryRequest,
    cancel: &AtomicBool,
    wait: &mut dyn FnMut(u32) -> bool,
) -> mailcraft_model::sync::EwsDirectoryResult {
    let mut run = Run {
        client,
        exchange,
        cancel,
        wait,
        shape: crate::items::Shape::Full,
        // Calendars, address books and to-dos are the mailbox's own, never its archive's (#1818).
        store: crate::folder::Store::Primary,
        flag_lacking: false,
        read_unsaid: false,
        own: Default::default(),
        reread_folders: false,
        needs_change_key: false,
        recipients_unsaid: false,
    };
    let mut out = mailcraft_model::sync::EwsDirectoryResult {
        account: request.account,
        email: request.email.clone(),
        url: request.url.clone(),
        ..Default::default()
    };
    if let Err(stop) = run.directory(request, &mut out).await {
        // The remaining shared folders might already have cached items on this device.
        if let Some(list) = out.calendars.as_mut() {
            list.complete = false;
        }
        if let Some(contacts) = out.contacts.as_mut() {
            contacts.complete = false;
        }
        match stop {
            Asked::Stop(why) => stop_line(why, &mut out),
            Asked::Code(_) | Asked::Bad | Asked::Lost | Asked::Unreadable => {
                out.errors.push("The server's answer for the calendar, contacts or to-dos couldn't be used".to_owned());
            }
        }
    }
    out.errors.truncate(mailcraft_model::sync::MAX_EWS_ERRORS);
    out
}

/// What a stop says, in words that name nothing of the server.
fn stop_line(stop: Stop, out: &mut mailcraft_model::sync::EwsDirectoryResult) {
    match stop {
        Stop::Cancelled => {}
        Stop::Auth => out.auth_refused = true,
        Stop::Busy(seconds) => out.busy_for = Some(seconds.max(1)),
        Stop::Tls => out.errors.push("Couldn't set up a secure connection with the server".to_owned()),
        Stop::Unreachable => out.errors.push("Couldn't reach the server".to_owned()),
        Stop::Line(text) => out.errors.push(text.to_owned()),
    }
}

impl<E: HttpExchange> Run<'_, E> {
    async fn directory(
        &mut self,
        request: &mailcraft_model::sync::EwsDirectoryRequest,
        out: &mut mailcraft_model::sync::EwsDirectoryResult,
    ) -> Result<(), Asked> {
        let mailbox = request.mailbox.trim();
        let mailbox = (!mailbox.is_empty()).then_some(mailbox);
        // What was changed here goes first, as mail does.
        if let Some(upload) = &request.changes {
            let mut uploaded = EventsUploaded { account: request.account, email: request.email.clone(), ..Default::default() };
            for op in upload.ops.iter().take(MAX_CHANGES) {
                uploaded.outcomes.push(self.push_event(op, &upload.zone, &request.email).await);
            }
            out.uploaded = Some(uploaded);
        }
        if let Some(upload) = &request.contact_changes {
            let mut uploaded = ContactsUploaded { account: request.account, email: request.email.clone(), ..Default::default() };
            for op in upload.ops.iter().take(MAX_CHANGES) {
                uploaded.outcomes.push(self.push_contact(op).await);
            }
            out.contacts_uploaded = Some(uploaded);
        }
        if let Some(upload) = &request.task_changes {
            let mut uploaded = TasksUploaded { account: request.account, email: request.email.clone(), ..Default::default() };
            for op in upload.ops.iter().take(MAX_CHANGES) {
                uploaded.outcomes.push(self.push_task(op).await);
            }
            out.tasks_uploaded = Some(uploaded);
        }
        // The account's own calendars, books and to-dos.
        if request.calendars {
            match self.read_calendars(request, mailbox, None, false).await {
                Ok((list, events, failed)) => {
                    out.calendars = Some(list);
                    out.events.extend(events);
                    if failed > 0 {
                        out.errors.push(calendars_failed(failed));
                    }
                }
                Err(Asked::Stop(stop)) => return Err(Asked::Stop(stop)),
                Err(_) => out.errors.push("Couldn't read the calendars from the server".to_owned()),
            }
        }
        // The working hours the owner has set in the mailbox, with a Send/Receive of the account's
        // own calendar (not a read of the days on screen, nor a shared mailbox's).
        if request.calendars && mailbox.is_none() && !request.background {
            out.working_hours = self.working_hours(&request.email).await;
            // And the mailbox's own categories with their colours (never worth failing for).
            match self.ask(&crate::categories::get_category_list(), &|reply| crate::categories::read_category_list(reply)).await {
                Ok(categories) => {
                    out.categories = Some(CategoriesDownload { account: request.account, email: request.email.clone(), categories, complete: true });
                }
                Err(Asked::Stop(stop)) => return Err(Asked::Stop(stop)),
                Err(_) => log::debug!("ews: the mailbox's categories couldn't be had"),
            }
            // And the calendars other people shared, as Outlook lists them.
            match self.shared_calendars().await {
                Ok(found) => out.shared_calendars = found,
                Err(Asked::Stop(stop)) => return Err(Asked::Stop(stop)),
                Err(_) => log::debug!("ews: Outlook's shortcuts to shared calendars couldn't be read"),
            }
        }
        if request.contacts {
            match self.read_contacts(request.account, &request.email, mailbox, None).await {
                Ok(download) => out.contacts = Some(download),
                Err(Asked::Stop(stop)) => return Err(Asked::Stop(stop)),
                Err(_) => out.errors.push("Couldn't read the contacts from the server".to_owned()),
            }
        }
        if request.tasks {
            match self.read_tasks(request.account, &request.email, mailbox).await {
                Ok(download) => out.tasks = Some(download),
                Err(Asked::Stop(stop)) => return Err(Asked::Stop(stop)),
                Err(_) => out.errors.push("Couldn't read the to-dos from the server".to_owned()),
            }
        }
        // Calendars and address books given by another mailbox's address.
        for shared in &request.shared {
            let address = shared.address.trim();
            if address.is_empty() {
                continue;
            }
            if shared.calendar {
                let read = match self.read_calendars(request, Some(address), Some(address), true).await {
                    // Not to be listed, only its free and busy times may be had (a calendar
                    // shared for those alone): drawn from them instead, as Outlook shows one.
                    Err(Asked::Code(ResponseCode::AccessDenied)) => {
                        self.free_busy_calendar(request, address).await.map(|(list, events)| (list, vec![events], 0))
                    }
                    other => other,
                };
                match read {
                    Ok((mut list, events, failed)) => {
                        if failed > 0 {
                            out.errors.push("Couldn't read a calendar that was shared with the account".to_owned());
                        }
                        if let Some(own) = out.calendars.as_mut() {
                            own.complete &= list.complete;
                            own.calendars.append(&mut list.calendars);
                            // More than are kept: those past the cap aren't listed, and a list that
                            // isn't whole takes nothing away.
                            if own.calendars.len() > mailcraft_model::remote::MAX_REMOTE_CALENDARS {
                                own.complete = false;
                            }
                        } else {
                            // Shared folders alone never describe the whole account.
                            list.complete = false;
                            out.calendars = Some(list);
                        }
                        out.events.extend(events);
                    }
                    Err(Asked::Stop(stop)) => return Err(Asked::Stop(stop)),
                    Err(_) => {
                        if let Some(list) = out.calendars.as_mut() {
                            list.complete = false;
                        }
                        out.errors.push("Couldn't read a calendar that was shared with the account".to_owned());
                    }
                }
            }
            if shared.contacts {
                match self.read_contacts(request.account, &request.email, Some(address), Some(address)).await {
                    Ok(mut download) => match out.contacts.as_mut() {
                        Some(all) => {
                            all.complete &= download.complete;
                            all.books.extend(download.books);
                            all.contacts.extend(download.contacts);
                        }
                        None => {
                            download.complete = false;
                            out.contacts = Some(download);
                        }
                    },
                    Err(Asked::Stop(stop)) => return Err(Asked::Stop(stop)),
                    Err(_) => {
                        if let Some(all) = out.contacts.as_mut() {
                            all.complete = false;
                        }
                        out.errors.push("Couldn't read an address book that was shared with the account".to_owned());
                    }
                }
            }
        }
        Ok(())
    }

    /// The folder lists of a kind: the default folder, and every folder of that class below the
    /// top of the mailbox.
    async fn directory_folders(&mut self, kind: Kind, mailbox: Option<&str>) -> Result<(Option<FolderInfo>, Vec<FolderInfo>, bool), Asked> {
        let mut get = GetFolder::new(&[kind.distinguished()]).full();
        if let Some(address) = mailbox {
            get = get.in_mailbox(address);
        }
        let default = match self
            .ask(&get.element(), &|reply| {
                let mut answers = get.answer(reply)?;
                if answers.len() != 1 {
                    return Err(crate::Error::Unexpected);
                }
                answers.pop().ok_or(crate::Error::Unexpected)?
            })
            .await
        {
            Ok(folder) => Some(folder),
            Err(Asked::Code(ResponseCode::FolderNotFound | ResponseCode::NonExistentMailbox)) => None,
            Err(other) => return Err(other),
        };
        let deleted = self.deleted_items(mailbox).await;
        let mut all: Vec<FolderInfo> = Vec::new();
        // Where every folder listed lies, to tell the ones inside Deleted Items.
        let mut parents: std::collections::HashMap<crate::folder::FolderId, crate::folder::FolderId> = std::collections::HashMap::new();
        let mut complete = false;
        let mut offset = 0u32;
        for _ in 0..MAX_PAGES {
            let mut find = FindFolder::new(DistinguishedFolder::MessageRoot, true, offset, 512);
            if let Some(address) = mailbox {
                find = find.in_mailbox(address);
            }
            let page = match self.ask(&find.element(), &|reply| find.answer(reply)).await {
                Ok(page) => page,
                // (A server that lets the owner into the folder it is given but not list the
                // mailbox — Grommunio does — still has that folder.)
                Err(Asked::Code(ResponseCode::AccessDenied | ResponseCode::FolderNotFound | ResponseCode::NonExistentMailbox))
                    if mailbox.is_some() && default.is_some() =>
                {
                    let default = default.clone().into_iter().collect::<Vec<_>>();
                    return Ok((default.first().cloned(), default, false));
                }
                Err(other) => return Err(other),
            };
            let before = page.folders.len();
            for folder in &page.folders {
                if let Some(parent) = &folder.parent
                    && parents.len() < MAX_PARENTS
                {
                    parents.insert(folder.id.clone(), parent.clone());
                }
            }
            // (A hidden folder is the mailbox's own or a program's: Outlook doesn't show it.)
            all.extend(page.folders.into_iter().filter(|folder| folder.class.eq_ignore_ascii_case(kind.class()) && !folder.hidden));
            if page.last || before == 0 {
                complete = page.last;
                break;
            }
            offset = offset.saturating_add(u32::try_from(before).unwrap_or(u32::MAX));
        }
        // What was deleted (a calendar in Deleted Items is a calendar still) is no list of the
        // account's, as in Outlook.
        if let Some(deleted) = &deleted {
            all.retain(|folder| !lies_in(&parents, &folder.id, deleted));
        }
        // The default folder first, wherever the listing put it: what is read is cut to a number of
        // folders, and the default one is never the one cut.
        if let Some(default) = &default {
            all.retain(|folder| folder.id != default.id);
            all.insert(0, default.clone());
        }
        Ok((default, all, complete))
    }

    /// The calendars other people shared with the owner, from Outlook's navigation shortcuts
    /// ([`crate::shortcuts`]): `None` when the mailbox has no Common Views folder; incomplete when
    /// a page of shortcuts or a person couldn't be had.
    async fn shared_calendars(&mut self) -> Result<Option<mailcraft_model::sync::EwsSharedCalendars>, Asked> {
        let find = FindFolder::new(DistinguishedFolder::Root, false, 0, 500);
        let folders = self.ask(&find.element(), &|reply| find.answer(reply)).await?;
        let Some(views) = folders.folders.iter().find(|f| f.display_name.trim().eq_ignore_ascii_case(crate::shortcuts::COMMON_VIEWS)) else {
            return Ok(None);
        };
        let mut found: Vec<crate::shortcuts::SharedCalendar> = Vec::new();
        let mut complete = true;
        let (mut offset, mut read) = (0u32, 0usize);
        loop {
            let element = crate::shortcuts::find_shortcuts(views.id.as_str(), offset);
            let page = match self.ask(&element, &|reply| crate::shortcuts::read_shortcuts(reply)).await {
                Ok(page) => page,
                Err(Asked::Stop(stop)) => return Err(Asked::Stop(stop)),
                Err(_) => {
                    complete = false;
                    break;
                }
            };
            read = read.saturating_add(page.read);
            for shared in page.shared {
                if !found.iter().any(|known| known.dn.eq_ignore_ascii_case(&shared.dn)) {
                    found.push(shared);
                }
            }
            match page.next {
                Some(next) if next > offset && read < crate::shortcuts::MAX_SHORTCUTS && page.read > 0 => offset = next,
                Some(_) => {
                    complete = false;
                    break;
                }
                None => break,
            }
        }
        // Each person, by the DN the shortcut names, as the directory has them.
        let mut calendars: Vec<mailcraft_model::sync::EwsSharedCalendar> = Vec::new();
        for shared in found.iter().take(mailcraft_model::MAX_OPENED_CALENDARS) {
            let Some(ask) = crate::resolve::ResolveNames::new(&shared.dn) else {
                complete = false;
                continue;
            };
            let resolved = match self.ask(&ask.element(), &|reply| ask.answer(reply)).await {
                Ok(resolved) => resolved,
                Err(Asked::Stop(stop)) => return Err(Asked::Stop(stop)),
                Err(_) => {
                    complete = false;
                    continue;
                }
            };
            let people: Vec<&crate::resolve::Candidate> = resolved
                .candidates
                .iter()
                .filter(|c| c.kind == crate::resolve::RecipientKind::Mailbox && mailcraft_model::mailbox_address(&c.email).is_some())
                .collect();
            let [person] = people.as_slice() else {
                complete = false;
                continue;
            };
            let address = mailcraft_model::mailbox_address(&person.email).unwrap_or_default();
            if calendars.iter().any(|known| known.address.eq_ignore_ascii_case(&address)) {
                continue;
            }
            let name = if shared.name.is_empty() { person.name.clone() } else { shared.name.clone() };
            calendars.push(mailcraft_model::sync::EwsSharedCalendar { address, name });
        }
        if found.len() > mailcraft_model::MAX_OPENED_CALENDARS {
            complete = false;
        }
        Ok(Some(mailcraft_model::sync::EwsSharedCalendars { calendars, complete }))
    }

    /// The id of the mailbox's Deleted Items, when the server gives it: `None` when it doesn't
    /// (the folders inside it are then not told apart).
    async fn deleted_items(&mut self, mailbox: Option<&str>) -> Option<crate::folder::FolderId> {
        let mut get = GetFolder::new(&[DistinguishedFolder::DeletedItems]);
        if let Some(address) = mailbox {
            get = get.in_mailbox(address);
        }
        let found = self
            .ask(&get.element(), &|reply| {
                let mut answers = get.answer(reply)?;
                if answers.len() != 1 {
                    return Err(crate::Error::Unexpected);
                }
                answers.pop().ok_or(crate::Error::Unexpected)?
            })
            .await;
        found.ok().map(|folder| folder.id)
    }

    async fn read_calendars(
        &mut self,
        request: &mailcraft_model::sync::EwsDirectoryRequest,
        mailbox: Option<&str>,
        owner_address: Option<&str>,
        shared: bool,
    ) -> Result<(CalendarsDownload, Vec<EventsDownload>, usize), Asked> {
        let (default, folders, complete) = self.directory_folders(Kind::Calendar, mailbox).await?;
        // How many calendars' events couldn't be read.
        let mut failed = 0usize;
        let mut list = CalendarsDownload {
            account: request.account,
            email: request.email.clone(),
            complete: complete && folders.len() <= mailcraft_model::remote::MAX_REMOTE_CALENDARS,
            ..Default::default()
        };
        let mut events = Vec::new();
        for folder in folders.iter().take(mailcraft_model::remote::MAX_REMOTE_CALENDARS) {
            let main = default.as_ref().is_some_and(|default| default.id == folder.id);
            let remote_id = match (owner_address, main) {
                (Some(address), true) => mailcraft_model::remote::opened_id(address),
                (Some(_), false) => folder.id.as_str().to_owned(),
                (None, true) => String::new(),
                (None, false) => folder.id.as_str().to_owned(),
            };
            let name = if main && !shared { String::new() } else { clean_name(&folder.display_name, owner_address) };
            // (The list names a calendar by a real id even when its events come as the account's
            // main one, whose remote id is empty; `apply_calendars` still makes the main one's
            // origin empty.)
            let list_id = if main && !shared { folder.id.as_str().to_owned() } else { remote_id.clone() };
            list.calendars.push(RemoteCalendar {
                id: list_id,
                name,
                group: if shared { "Shared Calendars".to_owned() } else { "My Calendars".to_owned() },
                rights: folder.rights,
                owner: owner_address.map(|address| CalendarOwner { address: address.to_owned(), name: address.to_owned() }),
                main: !shared && main,
                ..Default::default()
            });
            // (As the request has it, before 1970 too: only a window that doesn't move forward is
            // given one second.)
            let from = request.from;
            let to = request.to.max(from.saturating_add(1));
            // A calendar whose events can't be read is passed over: it stays in the list (and its
            // events here as they were), the list says it isn't whole, and the others are read.
            let (mut download, complete) = match self.read_events(folder, &remote_id, from, to, shared).await {
                Ok(read) => read,
                Err(Asked::Stop(stop)) => return Err(Asked::Stop(stop)),
                // Somebody else's calendar that may be seen but not listed: for whoever asked to
                // draw it from its free and busy times.
                Err(Asked::Code(ResponseCode::AccessDenied)) if shared && main => return Err(Asked::Code(ResponseCode::AccessDenied)),
                Err(_) => {
                    list.complete = false;
                    failed = failed.saturating_add(1);
                    continue;
                }
            };
            if !complete {
                list.complete = false;
            }
            download.account = request.account;
            download.email = request.email.clone();
            download.has_categories = true;
            download.has_options = true;
            events.push(download);
        }
        Ok((list, events, failed))
    }

    /// The main calendar of `address`, shared with the account for its free and busy times alone,
    /// as those (`GetUserAvailability`, the stretch of the request in stretches of 42 days): each
    /// block of time taken an event without a title or a place, only when and how it shows (busy,
    /// tentative, out of office, working elsewhere), known again by those. The calendar is listed
    /// as one that only says when its owner is busy ([`Rights::FreeBusy`]). A person whose time
    /// the server won't tell is [`ResponseCode::AccessDenied`] still.
    async fn free_busy_calendar(
        &mut self,
        request: &mailcraft_model::sync::EwsDirectoryRequest,
        address: &str,
    ) -> Result<(CalendarsDownload, EventsDownload), Asked> {
        let id = mailcraft_model::remote::opened_id(address);
        let mut download = EventsDownload {
            account: request.account,
            email: request.email.clone(),
            calendar: id.clone(),
            from: request.from,
            to: request.to,
            complete: true,
            free_busy: true,
            ..Default::default()
        };
        let mut seen = std::collections::HashSet::new();
        let mut at = request.from;
        for n in 0..MAX_AVAILABILITY_ASKS {
            if at >= request.to {
                break;
            }
            let end = request.to.min(at.saturating_add(crate::availability::MAX_WINDOW));
            let ask = mailcraft_model::remote::FreeBusyAsk { people: vec![address.to_owned()], from: at, to: end };
            let Some(question) = crate::availability::GetUserAvailability::new(&ask) else { return Err(Asked::Bad) };
            let answer = self.ask(&question.element(), &|reply| question.answer(reply)).await?;
            let person = answer.people.first().ok_or(Asked::Code(ResponseCode::AccessDenied))?;
            if !person.error.is_empty() {
                return Err(Asked::Code(ResponseCode::AccessDenied));
            }
            for busy in &person.busy {
                let id = format!("freebusy:{}:{}:{}", busy.start, busy.end, busy.show_as.code());
                if download.events.len() < MAX_ITEMS && seen.insert(id.clone()) {
                    download.events.push(RemoteEvent {
                        id,
                        start: busy.start,
                        end: busy.end,
                        show_as: busy.show_as,
                        is_organizer: Some(false),
                        ..Default::default()
                    });
                }
            }
            at = end;
            // Not all of the stretch was asked for: nothing is taken away for being missing.
            if n.saturating_add(1) == MAX_AVAILABILITY_ASKS && at < request.to {
                download.complete = false;
            }
        }
        let list = CalendarsDownload {
            account: request.account,
            email: request.email.clone(),
            // (A calendar opened by address never describes the whole account.)
            complete: false,
            calendars: vec![RemoteCalendar {
                id,
                name: address.to_owned(),
                group: "Shared Calendars".to_owned(),
                rights: mailcraft_model::Rights::FreeBusy,
                owner: Some(CalendarOwner { address: address.to_owned(), name: address.to_owned() }),
                main: false,
                ..Default::default()
            }],
            ..Default::default()
        };
        Ok((list, download))
    }

    /// The working hours of the mailbox at `email`, as `GetUserAvailability` says them of the owner
    /// for the day to come ([`crate::availability::working_hours`]); nothing when the server
    /// won't say (which is never worth failing for).
    async fn working_hours(&mut self, email: &str) -> Option<mailcraft_model::sync::EwsWorkingHours> {
        let now = mailcraft_time::now();
        let ask = mailcraft_model::remote::FreeBusyAsk { people: vec![email.to_owned()], from: now, to: now.saturating_add(86_400) };
        let question = crate::availability::GetUserAvailability::new(&ask)?;
        match self.ask(&question.element(), &|reply| Ok(crate::availability::working_hours(reply, now))).await {
            Ok(hours) => hours,
            Err(_) => {
                log::debug!("ews: the mailbox's working hours couldn't be had");
                None
            }
        }
    }

    async fn read_events(&mut self, folder: &FolderInfo, remote_id: &str, from: i64, to: i64, plain: bool) -> Result<(EventsDownload, bool), Asked> {
        let mut masters: Vec<RemoteEvent> = Vec::new();
        let mut by_uid: std::collections::HashMap<String, String> = std::collections::HashMap::new();
        // The series themselves: `FindItem` without a view returns them, and their occurrences
        // are the `CalendarView` below.
        let mut offset = 0u32;
        let mut masters_complete = false;
        for _ in 0..MAX_PAGES {
            let find = directory::FindItems::new(Kind::Calendar, folder.id.as_str(), offset, 512);
            let find = if plain { find.plain() } else { find };
            let page = self.ask(&find.element(), &|reply| find.answer(reply)).await?;
            let next = page.next_offset(offset);
            let last = page.last;
            for item in page.items {
                if let Item::Event(event) = item
                    && event.kind == RemoteEventKind::SeriesMaster
                {
                    if !event.uid.is_empty() {
                        by_uid.insert(event.uid.clone(), event.id.clone());
                    }
                    masters.push(*event);
                }
            }
            let Some(next) = next.filter(|_| !last) else {
                masters_complete = last;
                break;
            };
            offset = next;
        }
        let mut download = EventsDownload {
            account: 0,
            email: String::new(),
            calendar: remote_id.to_owned(),
            from,
            to,
            complete: masters_complete,
            events: Vec::new(),
            has_categories: true,
            has_options: true,
            ..Default::default()
        };
        download.events.extend(masters.into_iter().filter(|event| event.recurrence.is_some()));
        // CalendarView has no offset. A long appointment can overlap both halves of a
        // truncated view, so cover both time ranges and keep each returned item only once.
        let mut windows = vec![(from, to)];
        let mut seen = std::collections::HashSet::new();
        for _ in 0..MAX_PAGES {
            let Some((start, end)) = windows.pop() else { break };
            let find = directory::FindItems::new(Kind::Calendar, folder.id.as_str(), 0, 512).view(start, end);
            let find = if plain { find.plain() } else { find };
            let page = self.ask(&find.element(), &|reply| find.answer(reply)).await?;
            if !page.last {
                let middle = start.midpoint(end);
                if middle > start && middle < end {
                    windows.push((middle, end));
                    windows.push((start, middle));
                } else {
                    download.complete = false;
                }
            }
            for item in page.items {
                if let Item::Event(mut event) = item {
                    if !seen.insert(event.id.clone()) {
                        continue;
                    }
                    if matches!(event.kind, RemoteEventKind::Occurrence | RemoteEventKind::Exception) {
                        if let Some(master) = by_uid.get(&event.uid) {
                            event.series.clone_from(master);
                        } else {
                            // (A series master that could not be read: what is here is an event of
                            // its own, so that it is never taken for a day of a series that isn't
                            // known here.)
                            event.kind = RemoteEventKind::Single;
                        }
                    }
                    if event.kind != RemoteEventKind::SeriesMaster {
                        download.events.push(*event);
                    }
                }
                if download.events.len() >= MAX_ITEMS {
                    download.complete = false;
                    return Ok((download, false));
                }
            }
        }
        download.complete &= windows.is_empty();
        let complete = download.complete;
        Ok((download, complete))
    }

    async fn read_contacts(
        &mut self,
        account: mailcraft_model::Id,
        email: &str,
        mailbox: Option<&str>,
        owner_address: Option<&str>,
    ) -> Result<ContactsDownload, Asked> {
        let (_, folders, complete) = self.directory_folders(Kind::Contacts, mailbox).await?;
        let mut download = ContactsDownload {
            account,
            email: email.to_owned(),
            complete: complete && folders.len() <= mailcraft_model::remote::MAX_REMOTE_CALENDARS,
            books: Vec::new(),
            contacts: Vec::new(),
            ..Default::default()
        };
        for folder in folders.iter().take(mailcraft_model::remote::MAX_REMOTE_CALENDARS) {
            let book = clean_name(&folder.display_name, owner_address);
            download.books.push(RemoteAddressBook { id: folder.id.as_str().to_owned(), name: book.clone(), rights: folder.rights });
            let mut offset = 0u32;
            let mut finished = false;
            for _ in 0..MAX_PAGES {
                let find = directory::FindItems::new(Kind::Contacts, folder.id.as_str(), offset, 512);
                let find = if mailbox.is_some() { find.plain() } else { find };
                let page = self.ask(&find.element(), &|reply| find.answer(reply)).await?;
                let next = page.next_offset(offset);
                let last = page.last;
                for item in page.items {
                    if let Item::Contact(mut contact) = item {
                        contact.book = book.clone();
                        contact.book_id = folder.id.as_str().to_owned();
                        download.contacts.push(*contact);
                        if download.contacts.len() >= MAX_ITEMS {
                            download.complete = false;
                            return Ok(download);
                        }
                    }
                }
                let Some(next) = next.filter(|_| !last) else {
                    finished = last;
                    break;
                };
                offset = next;
            }
            download.complete &= finished;
        }
        Ok(download)
    }

    async fn read_tasks(&mut self, account: mailcraft_model::Id, email: &str, mailbox: Option<&str>) -> Result<TasksDownload, Asked> {
        let (_, folders, complete) = self.directory_folders(Kind::Tasks, mailbox).await?;
        let mut download = TasksDownload {
            account,
            email: email.to_owned(),
            complete: complete && folders.len() <= mailcraft_model::remote_tasks::MAX_TASK_LISTS,
            lists: Vec::new(),
            tasks: Vec::new(),
        };
        for folder in folders.iter().take(mailcraft_model::remote_tasks::MAX_TASK_LISTS) {
            download.lists.push(RemoteTaskList { id: folder.id.as_str().to_owned(), complete: false, shared: false });
            let mut offset = 0u32;
            let mut finished = false;
            for _ in 0..MAX_PAGES {
                let find = directory::FindItems::new(Kind::Tasks, folder.id.as_str(), offset, 512);
                let find = if mailbox.is_some() { find.plain() } else { find };
                let page = self.ask(&find.element(), &|reply| find.answer(reply)).await?;
                let next = page.next_offset(offset);
                let last = page.last;
                for item in page.items {
                    if let Item::Task(mut task) = item {
                        task.list = folder.id.as_str().to_owned();
                        download.tasks.push(*task);
                        if download.tasks.len() >= mailcraft_model::remote_tasks::MAX_TASKS {
                            download.complete = false;
                            return Ok(download);
                        }
                    }
                }
                let Some(next) = next.filter(|_| !last) else {
                    finished = last;
                    break;
                };
                offset = next;
            }
            if let Some(list) = download.lists.last_mut() {
                list.complete = finished;
            }
            download.complete &= finished;
        }
        Ok(download)
    }

    async fn push_event(&mut self, op: &mailcraft_model::remote::EventOp, zone: &str, email: &str) -> EventOutcome {
        let zone = op.fields.zones.as_ref().map(|zones| zones.start.as_str()).filter(|zone| !zone.trim().is_empty()).unwrap_or(zone);
        let zone = windows_zone(zone);
        let zone = zone.as_str();
        let mut outcome = EventOutcome { op: op.clone(), ..Default::default() };
        // One occurrence of a series that has no name of its own here: the server is asked which
        // of its items it is, and the change goes to that one.
        let mut occurrence_id = String::new();
        let resolved;
        let op = if op.remote.is_empty() && !op.series.is_empty() && op.occurrence != 0 && op.kind != mailcraft_model::remote::EventChangeKind::Create
        {
            match self.find_occurrence(op).await {
                Ok(Some(id)) => {
                    occurrence_id.clone_from(&id);
                    resolved = mailcraft_model::remote::EventOp { remote: id, ..op.clone() };
                    &resolved
                }
                // (The server's series has no such day: it was taken away there.)
                Ok(None) => {
                    outcome.result = EventResult::Gone;
                    return outcome;
                }
                Err(error) => return settle(outcome, Err(error)),
            }
        } else {
            op
        };
        if op.remote.is_empty() && op.kind != mailcraft_model::remote::EventChangeKind::Create {
            outcome.result = EventResult::Refused;
            outcome.error = "the server's copy of this event is not known here".to_owned();
            return outcome;
        }
        let result = match op.kind {
            mailcraft_model::remote::EventChangeKind::Respond => {
                if op.remote.is_empty() {
                    Err(Asked::Bad)
                } else {
                    let answer = match op.response {
                        mailcraft_model::Response::Accepted => Answer::Accept,
                        mailcraft_model::Response::Tentative => Answer::Tentative,
                        mailcraft_model::Response::Declined => Answer::Decline,
                        mailcraft_model::Response::None => Answer::Tentative,
                    };
                    let proposed = op.proposed.map(|time| (time.start, time.end));
                    let element = directory::meeting_answer(answer, &op.remote, &op.comment, op.send, proposed);
                    match self.ask_once(&element, &|reply| directory::answered(reply)).await {
                        // A server older than Exchange 2013 has no place for a proposed time: the
                        // answer isn't sent without it, and that is said.
                        Err(Asked::Code(ResponseCode::SchemaValidation | ResponseCode::InvalidRequest)) if proposed.is_some() => {
                            outcome.result = EventResult::Refused;
                            outcome.error = "the server can't take a proposed time; answer again without proposing one".to_owned();
                            return outcome;
                        }
                        other => other.map(|()| (op.remote.clone(), String::new())),
                    }
                }
            }
            mailcraft_model::remote::EventChangeKind::Forward => {
                if op.remote.is_empty() || op.to.is_empty() {
                    Err(Asked::Bad)
                } else {
                    let element = forward_item(&op.remote, &op.to, &op.comment);
                    self.ask_once(&element, &|reply| directory::answered(reply)).await.map(|()| (op.remote.clone(), String::new()))
                }
            }
            mailcraft_model::remote::EventChangeKind::Delete => {
                let element = directory::delete_item(Kind::Calendar, &op.remote, "", op.cancel, false);
                self.ask(&element, &|reply| directory::deleted(reply)).await.map(|()| (String::new(), String::new()))
            }
            mailcraft_model::remote::EventChangeKind::Create | mailcraft_model::remote::EventChangeKind::Update => {
                let event = event_from_fields(op, zone);
                if op.kind == mailcraft_model::remote::EventChangeKind::Create {
                    let folder = (!op.calendar.is_empty()).then_some(op.calendar.as_str());
                    // The appointment is the account's own: its organizer is its owner, which a
                    // server doesn't always fill in by itself (see `calendar_item`); in a calendar
                    // opened by address, the person whose calendar it is.
                    let organizer = mailcraft_model::remote::opened_address(&op.calendar).unwrap_or_else(|| email.to_owned());
                    let sender = mailcraft_model::Address::new(&organizer, &organizer);
                    let element = directory::create_item(Kind::Calendar, folder, directory::calendar_item(&event, zone, Some(&sender)), op.send);
                    let made = self.ask_once(&element, &|reply| directory::saved(reply, "CreateItem")).await;
                    let made = match made {
                        // A server that doesn't take those properties still takes the appointment.
                        Err(Asked::Code(
                            ResponseCode::InvalidPropertyRequest | ResponseCode::InvalidPropertyForOperation | ResponseCode::SchemaValidation,
                        )) => {
                            log::info!("ews calendar: the server refused an appointment's sender properties; it is made without them");
                            let element = directory::create_item(Kind::Calendar, folder, directory::calendar_item(&event, zone, None), op.send);
                            self.ask_once(&element, &|reply| directory::saved(reply, "CreateItem")).await
                        }
                        other => other,
                    };
                    match made {
                        // Once more as a change: a server that ignored them in the create (some
                        // Grommunio versions do) then keeps the organizer, and the appointment is
                        // not left without one.
                        Ok((id, key)) if !id.is_empty() => {
                            let change = directory::sender_change(&id, &key, &sender);
                            let _ = self.ask_once(&change, &|reply| directory::saved(reply, "UpdateItem")).await;
                            Ok((id, key))
                        }
                        other => other,
                    }
                } else {
                    let element = directory::update_event(&op.fields, &op.remote, "", op.send, zone);
                    self.ask(&element, &|reply| directory::acknowledged(reply, "UpdateItem")).await.map(|()| (String::new(), String::new()))
                }
            }
        };
        if let Ok((id, _)) = &result
            && id.is_empty()
            && !occurrence_id.is_empty()
        {
            // The occurrence's own name, as the server gave it: it is known by it from now on.
            outcome.remote = occurrence_id;
        }
        settle(outcome, result)
    }

    /// The server's id of the occurrence of `op`'s series that falls at `op.occurrence` (as the
    /// series has it: an occurrence moved since is found by where it was), in `op`'s calendar.
    /// The series' `UID` is asked for, then the calendar a week either side; `None` when no item
    /// there is that occurrence. A calendar opened by address is looked in in its owner's mailbox,
    /// with no property beyond the base shape asked for (as it is read).
    async fn find_occurrence(&mut self, op: &mailcraft_model::remote::EventOp) -> Result<Option<String>, Asked> {
        let uid = self.ask(&directory::get_uid(&op.series), &|reply| directory::read_uid(reply)).await?;
        if uid.is_empty() {
            return Ok(None);
        }
        const AROUND: i64 = 7 * 86_400;
        let find = directory::FindItems::new(Kind::Calendar, &op.calendar, 0, 512)
            .view(op.occurrence.saturating_sub(AROUND), op.occurrence.saturating_add(AROUND));
        let find = if mailcraft_model::remote::opened_address(&op.calendar).is_some() { find.plain() } else { find };
        let page = self.ask(&find.element(), &|reply| find.answer(reply)).await?;
        Ok(page.items.into_iter().find_map(|item| match item {
            Item::Event(event)
                if event.uid == uid
                    && matches!(event.kind, RemoteEventKind::Occurrence | RemoteEventKind::Exception)
                    && event.original.unwrap_or(event.start) == op.occurrence
                    && !event.id.is_empty() =>
            {
                Some(event.id)
            }
            _ => None,
        }))
    }

    async fn push_contact(&mut self, op: &mailcraft_model::remote::ContactOp) -> ContactOutcome {
        let mut outcome = ContactOutcome { op: op.clone(), ..Default::default() };
        // (A deletion carries no card: it names the server's copy by its id alone.)
        let contact = if op.kind == mailcraft_model::remote::ContactChangeKind::Delete {
            RemoteContact::default()
        } else {
            match contact_from_card(&op.card) {
                Some(contact) => contact,
                None => {
                    outcome.result = EventResult::Refused;
                    outcome.error = "the contact couldn't be read".to_owned();
                    return outcome;
                }
            }
        };
        let result = match op.kind {
            mailcraft_model::remote::ContactChangeKind::Delete => {
                if op.remote.is_empty() {
                    Err(Asked::Bad)
                } else {
                    let element = directory::delete_item(Kind::Contacts, &op.remote, "", false, false);
                    self.ask(&element, &|reply| directory::deleted(reply)).await.map(|()| (String::new(), String::new()))
                }
            }
            mailcraft_model::remote::ContactChangeKind::Create => {
                let folder = (!op.book.is_empty()).then_some(op.book.as_str());
                let element = directory::create_item(Kind::Contacts, folder, directory::contact_item(&contact), false);
                self.ask_once(&element, &|reply| directory::saved(reply, "CreateItem")).await
            }
            mailcraft_model::remote::ContactChangeKind::Update => {
                if op.remote.is_empty() {
                    Err(Asked::Bad)
                } else {
                    let element = directory::update_item(Kind::Contacts, &ItemUp::Contact(Box::new(contact)), &op.remote, "", false, "");
                    self.ask(&element, &|reply| directory::acknowledged(reply, "UpdateItem")).await.map(|()| (op.remote.clone(), String::new()))
                }
            }
        };
        match result {
            Ok((id, key)) => {
                outcome.result = EventResult::Done;
                if !id.is_empty() {
                    outcome.remote = id;
                } else if op.kind == mailcraft_model::remote::ContactChangeKind::Create {
                    outcome.result = EventResult::Failed;
                    outcome.error = "the server didn't say which contact it made".to_owned();
                }
                let _ = key;
            }
            Err(Asked::Code(ResponseCode::ItemNotFound | ResponseCode::FolderNotFound)) => outcome.result = EventResult::Gone,
            Err(Asked::Code(ResponseCode::AccessDenied)) => outcome.result = EventResult::Denied,
            Err(Asked::Stop(Stop::Busy(seconds))) => {
                outcome.result = EventResult::Failed;
                outcome.error = format!("the server is busy for {seconds} s");
            }
            Err(Asked::Code(_)) => {
                outcome.result = EventResult::Refused;
                outcome.error = "the server won't do it".to_owned();
            }
            Err(_) => {
                outcome.result = EventResult::Failed;
                outcome.error = "the server's answer couldn't be used".to_owned();
            }
        }
        outcome
    }

    async fn push_task(&mut self, op: &mailcraft_model::remote_tasks::TaskOp) -> TaskOutcome {
        // (Named by the to-do's id from the start: a deletion done, or found done already, is
        // taken off the list of those waiting by it.)
        let mut outcome = TaskOutcome { kind: op.kind, task: op.task, patch: op.patch.clone(), id: op.id.clone(), ..Default::default() };
        if op.id.is_empty() && op.kind != TaskOpKind::Create {
            outcome.result = TaskResult::Refused;
            outcome.error = "the server's copy of this to-do is not known here".to_owned();
            return outcome;
        }
        let task = task_from_patch(op);
        let result = match op.kind {
            TaskOpKind::Delete => {
                let element = directory::delete_item(Kind::Tasks, &op.id, "", false, false);
                self.ask(&element, &|reply| directory::deleted(reply)).await.map(|()| (String::new(), String::new()))
            }
            TaskOpKind::Create => {
                let folder = (!op.list.is_empty()).then_some(op.list.as_str());
                let element = directory::create_item(Kind::Tasks, folder, directory::task_item(&task), false);
                self.ask_once(&element, &|reply| directory::saved(reply, "CreateItem")).await
            }
            TaskOpKind::Update => {
                let element = directory::update_task(&op.patch, &op.id, "");
                self.ask(&element, &|reply| directory::acknowledged(reply, "UpdateItem")).await.map(|()| (String::new(), String::new()))
            }
        };
        match result {
            Ok((id, key)) => {
                outcome.result = TaskResult::Done;
                if !id.is_empty() {
                    outcome.id = id;
                } else if op.kind == TaskOpKind::Create {
                    outcome.result = TaskResult::Failed;
                    outcome.error = "the server didn't say which to-do it made".to_owned();
                }
                outcome.change_key = key;
                // (A to-do made in the server's default list has no id of its own for the list
                // here; the next download gives it the folder's id.)
                outcome.list = if op.list.is_empty() { "tasks".to_owned() } else { op.list.clone() };
            }
            Err(Asked::Code(ResponseCode::ItemNotFound | ResponseCode::FolderNotFound)) => outcome.result = TaskResult::Gone,
            Err(Asked::Code(ResponseCode::AccessDenied)) => outcome.result = TaskResult::Denied,
            Err(Asked::Stop(Stop::Busy(_))) => outcome.result = TaskResult::Failed,
            Err(Asked::Code(_)) => {
                outcome.result = TaskResult::Refused;
                outcome.error = "the server won't do it".to_owned();
            }
            Err(_) => {
                outcome.result = TaskResult::Failed;
                outcome.error = "the server's answer couldn't be used".to_owned();
            }
        }
        outcome
    }
}

/// What is said of calendars whose events couldn't be read.
fn calendars_failed(failed: usize) -> String {
    if failed == 1 {
        "Couldn't read one of the calendars from the server".to_owned()
    } else {
        format!("Couldn't read {failed} of the calendars from the server")
    }
}

/// A name a folder is shown under; a shared one says whose it is.
fn clean_name(name: &str, owner: Option<&str>) -> String {
    let name = name.chars().filter(|c| !c.is_control()).take(255).collect::<String>();
    match owner {
        Some(address) if !name.is_empty() => format!("{name} ({address})"),
        Some(address) => address.to_owned(),
        None if name.is_empty() => "Calendar".to_owned(),
        None => name,
    }
}

/// An event from the fields an upload says.
fn event_from_fields(op: &mailcraft_model::remote::EventOp, _zone: &str) -> RemoteEvent {
    let fields = &op.fields;
    let (start, end, all_day) = fields.time.map_or((0, 0, false), |time| (time.start, time.end.max(time.start), time.all_day));
    let recurrence = fields.repeat.as_ref().and_then(|repeat| repeat.rule.clone());
    RemoteEvent {
        id: op.remote.clone(),
        token: op.token.clone(),
        title: fields.title.clone().unwrap_or_default(),
        location: fields.location.clone().unwrap_or_default(),
        description: fields.description.clone().unwrap_or_default(),
        start,
        end,
        all_day,
        show_as: fields.show_as.unwrap_or_default(),
        private: fields.private.unwrap_or(false),
        reminder_minutes: fields.reminder.as_ref().map(|reminder| reminder.minutes),
        recurrence,
        series_day: fields.repeat.as_ref().map(|repeat| repeat.first),
        attendees: fields.attendees.clone().unwrap_or_default(),
        categories: fields.categories.clone().unwrap_or_default(),
        importance: fields.importance.unwrap_or_default(),
        options: mailcraft_model::ResponseOptions {
            request_responses: fields.request_responses.unwrap_or(true),
            allow_proposals: fields.allow_proposals.unwrap_or(true),
        },
        ..RemoteEvent::default()
    }
}

/// A contact from the vCard the engine wrote for it.
fn contact_from_card(card: &str) -> Option<RemoteContact> {
    let contacts = mailcraft_vcard::parse(card);
    let contact = contacts.first()?;
    Some(RemoteContact {
        display_name: contact.display_name.clone(),
        first: contact.first.clone(),
        middle: contact.middle.clone(),
        last: contact.last.clone(),
        nickname: contact.nickname.clone(),
        company: contact.company.clone(),
        title: contact.title.clone(),
        department: contact.department.clone(),
        emails: contact.emails.clone(),
        phones: contact.phones.clone(),
        addresses: contact.addresses.clone(),
        websites: contact.websites.clone(),
        birthday: contact.birthday,
        notes: contact.notes.clone(),
        ..RemoteContact::default()
    })
}

/// A to-do from the patch an upload says.
fn task_from_patch(op: &mailcraft_model::remote_tasks::TaskOp) -> RemoteTask {
    let patch = &op.patch;
    RemoteTask {
        id: op.id.clone(),
        list: op.list.clone(),
        title: patch.title.clone().unwrap_or_default(),
        notes: patch.notes.clone().unwrap_or_default(),
        start: patch.start.flatten(),
        due: patch.due.flatten(),
        completed: patch.completed.flatten(),
        importance: patch.importance.unwrap_or_default(),
        categories: patch.categories.clone().unwrap_or_default(),
        reminder: patch.reminder.flatten(),
        change_key: String::new(),
    }
}

/// `CreateItem` with a `ForwardItem` for a meeting sent on to other people.
fn forward_item(id: &str, to: &[mailcraft_model::Address], comment: &str) -> Element {
    let mut item = Element::new("t:ForwardItem").child(Element::new("t:ReferenceItemId").attr("Id", id));
    let recipients = to.iter().filter(|address| !address.email.trim().is_empty()).fold(Element::new("t:ToRecipients"), |list, address| {
        let mut mailbox = Element::new("t:Mailbox");
        if !address.name.trim().is_empty() {
            mailbox = mailbox.child(Element::new("t:Name").text(address.name.trim()));
        }
        list.child(mailbox.child(Element::new("t:EmailAddress").text(address.email.trim())))
    });
    item = item.child(recipients);
    if !comment.trim().is_empty() {
        item = item.child(Element::new("t:Body").attr("BodyType", "Text").text(comment));
    }
    Element::new("m:CreateItem").child(Element::new("m:Items").child(item))
}

/// `outcome` as the server's answer `result` makes it: done (with the id and change key the
/// server gave, if any), gone, denied, refused, or failed for now.
fn settle(mut outcome: EventOutcome, result: Result<(String, String), Asked>) -> EventOutcome {
    match result {
        Ok((id, key)) => {
            outcome.result = EventResult::Done;
            if !id.is_empty() {
                outcome.remote = id;
            } else if outcome.op.kind == mailcraft_model::remote::EventChangeKind::Create {
                // (A create always gives an id; without one nothing can find it again.)
                outcome.result = EventResult::Failed;
                outcome.error = "the server didn't say which event it made".to_owned();
            }
            outcome.change_key = key;
        }
        Err(Asked::Code(ResponseCode::ItemNotFound | ResponseCode::FolderNotFound)) => outcome.result = EventResult::Gone,
        Err(Asked::Code(ResponseCode::AccessDenied)) => outcome.result = EventResult::Denied,
        Err(Asked::Code(ResponseCode::ServerBusy)) => outcome.result = EventResult::Failed,
        Err(Asked::Stop(Stop::Busy(seconds))) => {
            outcome.result = EventResult::Failed;
            outcome.retry_after = Some(seconds.max(1));
        }
        Err(Asked::Stop(Stop::Auth)) => {
            outcome.result = EventResult::Failed;
            outcome.error = "the server turned the sign-in down".to_owned();
        }
        Err(Asked::Stop(Stop::Unreachable | Stop::Tls)) => {
            outcome.result = EventResult::Failed;
            outcome.error = "the server couldn't be reached".to_owned();
        }
        Err(Asked::Code(_)) => {
            outcome.result = EventResult::Refused;
            outcome.error = "the server won't do it".to_owned();
        }
        Err(_) => {
            outcome.result = EventResult::Failed;
            outcome.error = "the server's answer couldn't be used".to_owned();
        }
    }
    outcome
}

/// The time zone an event's times are said in, by the name Exchange takes in `StartTimeZone`
/// and `EndTimeZone` (the one Windows has: `W. Europe Standard Time`; Exchange refuses a name of
/// the IANA database, `Europe/Berlin`). `zone` is the event's own, or the calendar's; empty for
/// this computer's. Nothing for a zone Windows has no name for: the times then go as the instants
/// they are, and a series repeats by UTC's clock there.
fn windows_zone(zone: &str) -> String {
    let zone = zone.trim();
    let named = if zone.is_empty() { Some(mailcraft_time::Zone::system()) } else { mailcraft_time::Zone::named(zone) };
    named.and_then(mailcraft_time::Zone::windows_name).map(str::to_owned).unwrap_or_default()
}

/// The most folders whose place is kept while the mailbox's folders are listed.
const MAX_PARENTS: usize = 20_000;

/// Whether the folder `id` lies in `ancestor`, at any depth, as far as `parents` tell (a loop or a
/// tree deeper than any mailbox's is not followed further).
fn lies_in(
    parents: &std::collections::HashMap<crate::folder::FolderId, crate::folder::FolderId>,
    id: &crate::folder::FolderId,
    ancestor: &crate::folder::FolderId,
) -> bool {
    let mut at = id;
    for _ in 0..64 {
        match parents.get(at) {
            Some(parent) if parent == ancestor => return true,
            Some(parent) => at = parent,
            None => return false,
        }
    }
    false
}

#[cfg(test)]
#[path = "shared_write_tests.rs"]
mod shared_write_tests;

#[cfg(test)]
#[path = "shortcuts_run_tests.rs"]
mod shortcuts_run_tests;

#[cfg(test)]
#[path = "directory_run_tests.rs"]
mod tests;
