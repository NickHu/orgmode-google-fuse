use std::str::FromStr;

use chrono::{DateTime, Local, NaiveDate, TimeZone};
use chrono_tz::Tz;
use google_calendar3::api::{CalendarListEntry, Event, EventDateTime};

use super::ToOrg;

#[derive(Debug, Clone)]
pub(crate) struct OrgCalendar(pub(crate) CalendarListEntry, pub(crate) Vec<Event>);

impl From<(CalendarListEntry, Vec<Event>)> for OrgCalendar {
    fn from(es: (CalendarListEntry, Vec<Event>)) -> Self {
        Self(es.0, es.1)
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
        self.1
            .iter()
            .filter(|event| {
                // Filter out cancelled events
                event.status.as_deref() != Some("cancelled")
            })
            .map(|event| {
                // HEADLINE
                let mut str = "* ".to_owned();
                if let Some(summary) = &event.summary {
                    str.push_str(summary);
                } else {
                    str.push_str("Untitled Event");
                }
                str.push('\n');
                match (&event.start, &event.end) {
                    (Some(start), Some(end)) => {
                        str.push_str(format!("{}--{}\n", start.to_org(), end.to_org()).as_str());
                    }
                    (_, _) => unreachable!(),
                }

                // PROPERTIES
                str.push_str(":PROPERTIES:\n");
                macro_rules! print_property {
                    ($p:ident) => {
                        if let Some($p) = &event.$p {
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
                if let Some(description) = &event.description {
                    str.push('\n');
                    str.push_str(description);
                    str.push('\n');
                }

                str
            })
            .collect::<Vec<_>>()
            .join("\n")
    }
}
