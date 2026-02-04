use chrono::{DateTime, Local, NaiveDate, NaiveDateTime, TimeZone};

use crate::org::ToOrg;

impl ToOrg for NaiveDate {
    fn to_org_string(&self) -> String {
        self.format("%Y-%m-%d %a").to_string()
    }
}

impl<Tz: TimeZone> ToOrg for DateTime<Tz> {
    fn to_org_string(&self) -> String {
        self.with_timezone(&Local)
            .format("%Y-%m-%d %a %H:%M")
            .to_string()
    }
}

#[derive(Debug, Copy, Clone)]
pub(crate) enum Timestamp<Tz: TimeZone>
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
    #[allow(unused)]
    pub(crate) fn activate(self) -> Timestamp<Tz> {
        match self {
            Timestamp::ActiveDate(date) => Timestamp::ActiveDate(date),
            Timestamp::InactiveDate(date) => Timestamp::ActiveDate(date),
            Timestamp::ActiveDateTime(datetime) => Timestamp::ActiveDateTime(datetime),
            Timestamp::InactiveDateTime(datetime) => Timestamp::ActiveDateTime(datetime),
        }
    }

    pub(crate) fn deactivate(self) -> Timestamp<Tz> {
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
    fn to_org_string(&self) -> String {
        match self {
            Timestamp::ActiveDate(naive_date) => format!("<{}>", naive_date.to_org_string()),
            Timestamp::InactiveDate(naive_date) => format!("[{}]", naive_date.to_org_string()),
            Timestamp::ActiveDateTime(datetime) => format!("<{}>", datetime.to_org_string()),
            Timestamp::InactiveDateTime(datetime) => format!("[{}]", datetime.to_org_string()),
        }
    }
}
