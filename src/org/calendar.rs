use std::str::FromStr;
use std::sync::Mutex;
use std::time::SystemTime;
use std::{hash::Hash, sync::Arc};

use chrono::Local;
use chrono_tz::Tz;
use evmap::{ReadHandleFactory, WriteHandle};
use google_calendar3::api::{CalendarListEntry, Event, EventDateTime, Events};
use itertools::Itertools;

use crate::org::timestamp::Timestamp;

use super::{def_org_meta, ByETag, Id, ToOrg};

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
    CalendarMeta { calendar: CalendarListEntry, updated: SystemTime }
}

#[derive(Debug, Clone)]
pub(crate) struct OrgCalendar(
    ReadHandleFactory<Id, Box<ByETag<Event>>, CalendarMeta>,
    #[allow(clippy::type_complexity)] Arc<Mutex<WriteHandle<Id, Box<ByETag<Event>>, CalendarMeta>>>,
);

impl OrgCalendar {
    pub fn meta(&self) -> CalendarMeta {
        self.0.handle().meta().expect("meta not found").clone()
    }

    pub fn sync(&self, es: Events) {
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
        guard.refresh();
    }
}

impl From<(CalendarListEntry, Events)> for OrgCalendar {
    fn from(es: (CalendarListEntry, Events)) -> Self {
        let (rh, mut wh) = evmap::with_meta(
            (
                es.0,
                es.1.updated
                    .as_ref()
                    .copied()
                    .map(|dt| dt.into())
                    .unwrap_or(std::time::UNIX_EPOCH),
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
        self.0
            .handle()
            .read()
            .unwrap()
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
                // HEADLINE
                let mut str = "* ".to_owned();
                if let Some(summary) = &event.0.summary {
                    str.push_str(summary);
                } else {
                    str.push_str("Untitled Event");
                }
                str.push('\n');
                match (&event.0.start, &event.0.end) {
                    (Some(start), Some(end)) => {
                        str.push_str(
                            format!(
                                "{}--{}\n",
                                Timestamp::from(start.clone()).to_org_string(),
                                Timestamp::from(end.clone()).to_org_string()
                            )
                            .as_str(),
                        );
                    }
                    (_, _) => unreachable!(),
                }

                // PROPERTIES
                str.push_str(":PROPERTIES:\n");
                macro_rules! print_property {
                    ($p:ident, $e:expr) => {
                        if let Some($p) = &event.0.$p {
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

                // SECTION
                if let Some(description) = &event.0.description {
                    str.push('\n');
                    str.push_str(description);
                    str.push('\n');
                }

                Some(str)
            })
            .collect::<Vec<_>>()
            .join("\n")
    }
}
