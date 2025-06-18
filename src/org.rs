use std::fmt::Debug;

pub(crate) mod calendar;
pub(crate) mod tasklist;

pub(crate) trait ToOrg {
    fn to_org(&self) -> String;
}

impl ToOrg for String {
    fn to_org(&self) -> String {
        self.clone()
    }
}

impl ToOrg for &str {
    fn to_org(&self) -> String {
        self.to_string()
    }
}

#[derive(Debug, Clone)]
struct ByETag<T>(T)
where
    T: Debug + Clone;

type Id = String;
