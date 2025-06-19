use std::fmt::Debug;

use orgize::Org;

pub(crate) mod calendar;
pub(crate) mod tasklist;

pub(crate) trait ToOrg {
    fn to_org(&self) -> Org<'static> {
        Org::parse_string(self.to_org_string())
    }
    fn to_org_string(&self) -> String {
        let org = self.to_org();
        let mut str = Vec::new();
        let _ = org.write_org(&mut str);
        String::from_utf8(str).expect("Failed to convert org to String")
    }
}

impl ToOrg for String {
    fn to_org(&self) -> Org<'static> {
        Org::parse_string(self.to_owned())
    }
}

impl ToOrg for &str {
    fn to_org(&self) -> Org<'static> {
        Org::parse_string((*self).to_owned())
    }
}

impl ToOrg for Org<'_> {
    fn to_org_string(&self) -> String {
        let mut str = Vec::new();
        let _ = self.write_org(&mut str);
        String::from_utf8(str).expect("Failed to convert org to String")
    }
}

#[derive(Debug, Clone)]
struct ByETag<T>(T)
where
    T: Debug + Clone;

type Id = String;
