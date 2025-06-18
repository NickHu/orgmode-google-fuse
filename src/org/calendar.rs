use std::hash::Hash;
use std::str::FromStr;

use chrono::{DateTime, Local, NaiveDate, TimeZone};
use chrono_tz::Tz;
use evmap::{ReadHandle, WriteHandle};
use google_calendar3::api::{CalendarListEntry, Event, EventDateTime};

use super::{ByETag, Id, ToOrg};

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

#[derive(Debug)]
pub(crate) struct OrgCalendar(
    ReadHandle<Id, Box<ByETag<Event>>, CalendarListEntry>,
    WriteHandle<Id, Box<ByETag<Event>>, CalendarListEntry>,
);

impl OrgCalendar {
    pub fn calendar(&self) -> impl AsRef<CalendarListEntry> + use<'_> {
        self.0.meta().expect("CalendarListEntry meta not found")
    }
}

impl From<(CalendarListEntry, Vec<Event>)> for OrgCalendar {
    fn from(es: (CalendarListEntry, Vec<Event>)) -> Self {
        let (rh, mut wh) = evmap::with_meta(es.0);
        wh.extend(es.1.into_iter().map(|event| {
            let id = event.id.clone().unwrap_or_default();
            (id, Box::new(ByETag(event)))
        }));
        wh.refresh();
        Self(rh, wh)
    }
}

impl ToOrg for NaiveDate {
    fn to_org(&self) -> String {
        self.format("%Y-%m-%d").to_string()
    }
}

impl<Tz: TimeZone> ToOrg for DateTime<Tz> {
    fn to_org(&self) -> String {
        self.with_timezone(&Local)
            .format("%Y-%m-%d %H:%M")
            .to_string()
    }
}

#[derive(Debug, Copy, Clone)]
enum Timestamp<'a, Tz: TimeZone> {
    Active(&'a DateTime<Tz>),
    Inactive(&'a DateTime<Tz>),
}

impl<Tz: TimeZone> ToOrg for Timestamp<'_, Tz> {
    fn to_org(&self) -> String {
        match self {
            Timestamp::Active(datetime) => format!("<{}>", datetime.to_org()),
            Timestamp::Inactive(datetime) => format!("[{}]", datetime.to_org()),
        }
    }
}

impl ToOrg for EventDateTime {
    fn to_org(&self) -> String {
        match (self.date, self.date_time, &self.time_zone) {
            (Some(ymd), _, _) => {
                ymd.to_org() // all day event
            }
            (_, Some(datetime), None) => {
                // normal event with date and time
                Timestamp::Active(&datetime).to_org()
            }
            (_, Some(utc), Some(tz_str)) => {
                // event with specified timezone
                let tz = Tz::from_str(tz_str).expect("Invalid timezone");
                let datetime = utc.naive_utc().and_local_timezone(tz).unwrap();
                Timestamp::Active(&datetime).to_org()
            }
            (_, _, _) => unreachable!(),
        }
    }
}

impl ToOrg for OrgCalendar {
    fn to_org(&self) -> String {
        self.0
            .map_into::<_, Vec<_>, _>(|id, events| {
                let event = events
                    .iter()
                    .max_by_key(|e| e.0.updated)
                    .expect(&format!("No events found for id: {id}"));
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
                        str.push_str(format!("{}--{}\n", start.to_org(), end.to_org()).as_str());
                    }
                    (_, _) => unreachable!(),
                }

                // PROPERTIES
                str.push_str(":PROPERTIES:\n");
                macro_rules! print_property {
                    ($p:ident) => {
                        if let Some($p) = &event.0.$p {
                            str.push_str(":");
                            str.push_str(stringify!($p));
                            str.push_str(": ");
                            str.push_str(&$p.to_org());
                            str.push('\n');
                        }
                    };
                }
                print_property!(id);
                print_property!(etag);
                print_property!(created);
                print_property!(updated);
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
            .into_iter()
            .flatten()
            .collect::<Vec<_>>()
            .join("\n")
    }
}
