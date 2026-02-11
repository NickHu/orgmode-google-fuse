use std::collections::{HashMap, HashSet};
use std::str::FromStr;
use std::sync::atomic::Ordering;
use std::sync::Mutex;
use std::time::SystemTime;
use std::{hash::Hash, sync::Arc};

use atomic_time::AtomicSystemTime;
use chrono::Local;
use chrono_tz::Tz;
use evmap::{ReadHandle, ReadHandleFactory, WriteHandle};
use google_calendar3::api::{CalendarListEntry, Event, EventDateTime, Events};
use itertools::Itertools;
use orgize::ast::Headline;

use crate::org::conflict::push_conflict_str;
use crate::org::timestamp::Timestamp;
use crate::org::MetaPendingContainer;
use crate::write::{CalendarEventInsert, CalendarEventModify};

use super::{def_org_meta, text_from_property_drawer, ByETag, Id, ToOrg};

impl PartialEq for ByETag<Event> {
    fn eq(&self, other: &Self) -> bool {
        self.0.id == other.0.id && self.0.etag == other.0.etag
    }
}

impl Eq for ByETag<Event> {}

impl Hash for ByETag<Event> {
    fn hash<H: std::hash::Hasher>(&self, state: &mut H) {
        self.0.id.hash(state);
        self.0.etag.hash(state);
    }
}

def_org_meta! {
    CalendarMeta {
        calendar: CalendarListEntry,
        updated: AtomicSystemTime,
        pending: (HashSet<CalendarEventInsert>, HashMap<String, CalendarEventModify>)
    }
}

#[derive(Clone)]
pub(crate) struct OrgCalendar(
    ReadHandleFactory<Id, Box<ByETag<Event>>, CalendarMeta>,
    #[allow(clippy::type_complexity)] Arc<Mutex<WriteHandle<Id, Box<ByETag<Event>>, CalendarMeta>>>,
);

impl OrgCalendar {
    pub fn sync(&self, es: Events, updated: SystemTime) {
        let mut guard = self.1.lock().unwrap();
        for e in es.items.unwrap_or_default() {
            let Some(id) = &e.id else {
                tracing::warn!("Event without id found: {:?}", e);
                continue;
            };
            if guard.contains_key(id) {
                {
                    let v = guard.get_one(id).unwrap();
                    if v.0.etag == e.etag {
                        tracing::debug!("Event {id} is up to date, skipping update");
                        continue;
                    }
                }
                // Update existing event
                match e.status.as_deref() {
                    Some("cancelled") => {
                        tracing::info!("Removing event: {id}");
                        guard.empty(id.clone());
                    }
                    _ => {
                        tracing::info!("Updating event: {id}");
                        guard.insert(id.clone(), Box::new(ByETag(e)));
                    }
                }
            } else {
                // Insert new event
                tracing::info!("Inserting new event: {id}");
                guard.insert(id.clone(), Box::new(ByETag(e)));
            }
        }
        guard
            .meta()
            .unwrap()
            .updated()
            .store(updated, Ordering::Release);
        guard.refresh();
    }

    pub fn parse_event(headline: &Headline) -> Event {
        Event {
            description: headline.section().map(|s| s.raw().trim().to_owned()),
            end: headline.scheduled().and_then(|p| {
                let dt = p.end_to_chrono()?;
                if p.hour_end().is_some() {
                    Some(EventDateTime {
                        date: None,
                        date_time: Some(dt.and_utc()),
                        time_zone: iana_time_zone::get_timezone().ok(),
                    })
                } else {
                    Some(EventDateTime {
                        date: Some(dt.date()),
                        date_time: None,
                        time_zone: None,
                    })
                }
            }),
            start: headline.scheduled().and_then(|p| {
                let dt = p.start_to_chrono()?;
                if p.hour_start().is_some() {
                    Some(EventDateTime {
                        date: None,
                        date_time: Some(dt.and_utc()),
                        time_zone: iana_time_zone::get_timezone().ok(),
                    })
                } else {
                    Some(EventDateTime {
                        date: Some(dt.date()),
                        date_time: None,
                        time_zone: None,
                    })
                }
            }),
            summary: Some(headline.title_raw()),
            color_id: text_from_property_drawer!(headline, "color_id"),
            etag: text_from_property_drawer!(headline, "etag"),
            id: text_from_property_drawer!(headline, "id"),
            location: text_from_property_drawer!(headline, "location"),
            status: text_from_property_drawer!(headline, "status"),
            transparency: text_from_property_drawer!(headline, "transparency"),
            ..Event::default()
        }
    }
}

impl MetaPendingContainer for OrgCalendar {
    type Meta = CalendarMeta;
    type Item = Event;
    type Insert = CalendarEventInsert;
    type Modify = CalendarEventModify;

    fn with_meta<T>(&self, f: impl FnOnce(&Self::Meta) -> T) -> T {
        f(&self.0.handle().meta().expect("meta not found"))
    }

    fn with_pending<T>(
        &self,
        f: impl FnOnce(&(HashSet<Self::Insert>, HashMap<Id, Self::Modify>)) -> T,
    ) -> T {
        self.with_meta(|m| f(m.pending()))
    }

    fn read(&self) -> ReadHandle<Id, Box<ByETag<Self::Item>>, Self::Meta> {
        self.0.handle()
    }

    fn write(
        &self,
    ) -> std::sync::MutexGuard<'_, WriteHandle<Id, Box<ByETag<Self::Item>>, Self::Meta>> {
        self.1.lock().unwrap()
    }

    fn update_pending(
        meta: &Self::Meta,
        pending: (HashSet<Self::Insert>, HashMap<Id, Self::Modify>),
    ) -> Self::Meta {
        (
            meta.calendar().clone(),
            AtomicSystemTime::new(meta.updated().load(Ordering::Acquire)),
            pending,
        )
            .into()
    }
}

impl From<(CalendarListEntry, Events)> for OrgCalendar {
    fn from(es: (CalendarListEntry, Events)) -> Self {
        let (rh, mut wh) = evmap::with_meta(
            (
                es.0,
                AtomicSystemTime::new(
                    es.1.updated
                        .as_ref()
                        .copied()
                        .map(|dt| dt.into())
                        .unwrap_or(std::time::UNIX_EPOCH),
                ),
                Default::default(),
            )
                .into(),
        );
        wh.extend(es.1.items.unwrap_or_default().into_iter().map(|event| {
            let id = event.id.clone().unwrap_or_default();
            (id, Box::new(ByETag(event)))
        }));
        wh.refresh();
        Self(rh.factory(), Arc::new(Mutex::new(wh)))
    }
}

impl From<EventDateTime> for Timestamp<Local> {
    fn from(edt: EventDateTime) -> Self {
        match (edt.date, edt.date_time, &edt.time_zone) {
            (Some(ymd), _, _) => {
                Timestamp::ActiveDate(ymd) // all day event
            }
            (_, Some(datetime), None) => {
                // normal event with date and time
                Timestamp::ActiveDateTime(datetime.with_timezone(&Local))
            }
            (_, Some(utc), Some(tz_str)) => {
                // event with specified timezone
                let tz = Tz::from_str(tz_str).expect("Invalid timezone");
                let datetime = utc.naive_utc().and_local_timezone(tz).unwrap();
                Timestamp::ActiveDateTime(datetime.with_timezone(&Local))
            }
            (_, _, _) => unreachable!(),
        }
    }
}

impl ToOrg for OrgCalendar {
    fn to_org_string(&self) -> String {
        let handle = self.0.handle();
        let meta = handle.meta().expect("meta not found");
        let pending = meta.pending();
        let read_ref = handle.read().unwrap();
        [
            read_ref
                .iter()
                .sorted_by_key(|(id, events)| {
                    let event = events
                        .get_one()
                        .unwrap_or_else(|| panic!("No events found for id: {id}"));
                    (
                        event.0.start.as_ref().cloned().map(Timestamp::from),
                        event.0.end.as_ref().cloned().map(Timestamp::from),
                    )
                })
                .flat_map(|(id, events)| {
                    let event = events
                        .get_one()
                        .unwrap_or_else(|| panic!("No events found for id: {id}"));
                    if event.0.status.as_deref() == Some("cancelled") {
                        return None; // Skip cancelled events
                    }

                    let mut str = String::new();
                    match pending.1.get(id) {
                        Some(CalendarEventModify::Patch { event: new_event }) => {
                            push_conflict_str(
                                &mut str,
                                &render_event(&event.0, "* COMMENT ".to_owned(), true),
                                &render_event(new_event, "* ".to_owned(), false),
                            );
                        }
                        Some(CalendarEventModify::Delete) => {
                            push_conflict_str(
                                &mut str,
                                &render_event(&event.0, "* COMMENT ".to_owned(), true),
                                "",
                            );
                        }
                        None => str.push_str(&render_event(&event.0, "* ".to_owned(), true)),
                    }
                    Some(str)
                })
                .collect::<Vec<_>>(),
            pending
                .0
                .iter()
                .map(|CalendarEventInsert::Insert { event }| {
                    let mut str = String::new();
                    push_conflict_str(&mut str, "", &render_event(event, "* ".to_owned(), false));
                    str
                })
                .collect::<Vec<_>>(),
        ]
        .concat()
        .join("\n")
    }
}

fn render_event(event: &Event, prefix: String, with_properties: bool) -> String {
    // HEADLINE
    let mut str = prefix;
    if let Some(summary) = &event.summary {
        str.push_str(summary.trim());
    } else {
        str.push_str("Untitled Event");
    }
    str.push('\n');
    match (&event.start, &event.end) {
        (Some(start), Some(end)) => {
            str.push_str(
                format!(
                    "SCHEDULED: {}--{}\n",
                    Timestamp::from(start.clone()).to_org_string(),
                    Timestamp::from(end.clone()).to_org_string()
                )
                .as_str(),
            );
        }
        (_, _) => unreachable!(),
    }

    if with_properties {
        // PROPERTIES
        str.push_str(":PROPERTIES:\n");
        macro_rules! print_property {
            ($p:ident, $e:expr) => {
                if let Some($p) = &event.$p {
                    str.push_str(":");
                    str.push_str(stringify!($p));
                    str.push_str(": ");
                    str.push_str(&$e.to_org_string());
                    str.push('\n');
                }
            };
            ($p:ident) => {
                print_property!($p, $p);
            };
        }
        print_property!(id);
        print_property!(etag);
        print_property!(created, Timestamp::from(*created).deactivate());
        print_property!(updated, Timestamp::from(*updated).deactivate());
        print_property!(html_link);
        print_property!(visibility);
        print_property!(status);
        print_property!(location);
        str.push_str(":END:\n");
    }

    // SECTION
    if let Some(description) = &event.description {
        str.push('\n');
        str.push_str(description);
        str.push('\n');
    }

    str
}
