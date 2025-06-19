use std::str::FromStr;
use std::sync::Mutex;
use std::{hash::Hash, sync::Arc};

use chrono::{DateTime, Local, NaiveDate, NaiveDateTime, TimeZone};
use chrono_tz::Tz;
use evmap::{ReadHandleFactory, WriteHandle};
use google_calendar3::api::{CalendarListEntry, Event, EventDateTime};
use itertools::Itertools;

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

#[derive(Debug, Clone)]
pub(crate) struct OrgCalendar(
    ReadHandleFactory<Id, Box<ByETag<Event>>, Arc<CalendarListEntry>>,
    #[allow(clippy::type_complexity)]
    Arc<Mutex<WriteHandle<Id, Box<ByETag<Event>>, Arc<CalendarListEntry>>>>,
);

impl OrgCalendar {
    pub fn calendar(&self) -> impl AsRef<CalendarListEntry> {
        self.0
            .handle()
            .meta()
            .expect("CalendarListEntry meta not found")
            .clone()
    }

    pub fn sync(&self, es: impl IntoIterator<Item = Event>) {
        let mut guard = self.1.lock().unwrap();
        for e in es {
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

impl From<(CalendarListEntry, Vec<Event>)> for OrgCalendar {
    fn from(es: (CalendarListEntry, Vec<Event>)) -> Self {
        let (rh, mut wh) = evmap::with_meta(Arc::new(es.0));
        wh.extend(es.1.into_iter().map(|event| {
            let id = event.id.clone().unwrap_or_default();
            (id, Box::new(ByETag(event)))
        }));
        wh.refresh();
        Self(rh.factory(), Arc::new(Mutex::new(wh)))
    }
}

impl ToOrg for NaiveDate {
    fn to_org(&self) -> String {
        self.format("%Y-%m-%d %a").to_string()
    }
}

impl<Tz: TimeZone> ToOrg for DateTime<Tz> {
    fn to_org(&self) -> String {
        self.with_timezone(&Local)
            .format("%Y-%m-%d %a %H:%M")
            .to_string()
    }
}

#[derive(Debug, Copy, Clone)]
enum Timestamp<Tz: TimeZone>
where
    Tz::Offset: Copy,
{
    ActiveDate(NaiveDate),
    InactiveDate(NaiveDate),
    ActiveDateTime(DateTime<Tz>),
    InactiveDateTime(DateTime<Tz>),
}

impl<Tz: TimeZone> PartialEq for Timestamp<Tz>
where
    Tz::Offset: Copy,
{
    fn eq(&self, other: &Self) -> bool {
        match (self, other) {
            (Timestamp::ActiveDate(date1), Timestamp::ActiveDate(date2)) => date1 == date2,
            (Timestamp::InactiveDate(date1), Timestamp::InactiveDate(date2)) => date1 == date2,
            (Timestamp::ActiveDateTime(dt1), Timestamp::ActiveDateTime(dt2)) => dt1 == dt2,
            (Timestamp::InactiveDateTime(dt1), Timestamp::InactiveDateTime(dt2)) => dt1 == dt2,
            _ => false,
        }
    }
}

impl<Tz: TimeZone> Eq for Timestamp<Tz> where Tz::Offset: Copy {}

impl PartialOrd for Timestamp<Local> {
    fn partial_cmp(&self, other: &Self) -> Option<std::cmp::Ordering> {
        Some(self.cmp(other))
    }
}

impl Ord for Timestamp<Local> {
    fn cmp(&self, other: &Self) -> std::cmp::Ordering {
        let self_active = matches!(
            self,
            Timestamp::ActiveDate(_) | Timestamp::ActiveDateTime(_)
        );
        let other_active = matches!(
            other,
            Timestamp::ActiveDate(_) | Timestamp::ActiveDateTime(_)
        );
        let self_inner = match self {
            Timestamp::ActiveDate(date) | Timestamp::InactiveDate(date) => {
                NaiveDateTime::from(*date)
                    .and_local_timezone(Local)
                    .unwrap()
            }
            Timestamp::ActiveDateTime(datetime) | Timestamp::InactiveDateTime(datetime) => {
                *datetime
            }
        };
        let other_inner = match other {
            Timestamp::ActiveDate(date) | Timestamp::InactiveDate(date) => {
                NaiveDateTime::from(*date)
                    .and_local_timezone(Local)
                    .unwrap()
            }
            Timestamp::ActiveDateTime(datetime) | Timestamp::InactiveDateTime(datetime) => {
                *datetime
            }
        };
        (self_inner, self_active).cmp(&(other_inner, other_active))
    }
}

impl<Tz: TimeZone> Timestamp<Tz>
where
    Tz::Offset: Copy,
{
    fn activate(self) -> Timestamp<Tz> {
        match self {
            Timestamp::ActiveDate(date) => Timestamp::ActiveDate(date),
            Timestamp::InactiveDate(date) => Timestamp::ActiveDate(date),
            Timestamp::ActiveDateTime(datetime) => Timestamp::ActiveDateTime(datetime),
            Timestamp::InactiveDateTime(datetime) => Timestamp::ActiveDateTime(datetime),
        }
    }

    fn inactivate(self) -> Timestamp<Tz> {
        match self {
            Timestamp::ActiveDate(date) => Timestamp::InactiveDate(date),
            Timestamp::InactiveDate(date) => Timestamp::InactiveDate(date),
            Timestamp::ActiveDateTime(datetime) => Timestamp::InactiveDateTime(datetime),
            Timestamp::InactiveDateTime(datetime) => Timestamp::InactiveDateTime(datetime),
        }
    }
}

impl From<NaiveDate> for Timestamp<Local> {
    fn from(date: NaiveDate) -> Self {
        Timestamp::ActiveDate(date)
    }
}

impl<Tz: TimeZone> From<DateTime<Tz>> for Timestamp<Tz>
where
    Tz::Offset: Copy,
{
    fn from(datetime: DateTime<Tz>) -> Self {
        Timestamp::ActiveDateTime(datetime)
    }
}

impl<Tz: TimeZone> ToOrg for Timestamp<Tz>
where
    Tz::Offset: Copy,
{
    fn to_org(&self) -> String {
        match self {
            Timestamp::ActiveDate(naive_date) => format!("<{}>", naive_date.to_org()),
            Timestamp::InactiveDate(naive_date) => format!("[{}]", naive_date.to_org()),
            Timestamp::ActiveDateTime(datetime) => format!("<{}>", datetime.to_org()),
            Timestamp::InactiveDateTime(datetime) => format!("[{}]", datetime.to_org()),
        }
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
    fn to_org(&self) -> String {
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
                                Timestamp::from(start.clone()).to_org(),
                                Timestamp::from(end.clone()).to_org()
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
                            str.push_str(&$e.to_org());
                            str.push('\n');
                        }
                    };
                    ($p:ident) => {
                        print_property!($p, $p);
                    };
                }
                print_property!(id);
                print_property!(etag);
                print_property!(created, Timestamp::from(*created).inactivate());
                print_property!(updated, Timestamp::from(*updated).inactivate());
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
