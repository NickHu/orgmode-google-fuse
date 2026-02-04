use std::{
    collections::{HashMap, HashSet},
    fmt::Debug,
};

use orgize::{
    ast::{Headline, Token},
    export::{from_fn, Container, Event},
    Org,
};

pub(crate) mod calendar;
pub(crate) mod tasklist;
pub(crate) mod timestamp;

pub(crate) trait ToOrg {
    fn to_org(&self) -> Org {
        Org::parse(self.to_org_string())
    }
    fn to_org_string(&self) -> String {
        let org = self.to_org();
        org.to_org()
    }
}

impl ToOrg for String {
    fn to_org(&self) -> Org {
        Org::parse(self)
    }

    fn to_org_string(&self) -> String {
        self.clone()
    }
}

impl ToOrg for &str {
    fn to_org(&self) -> Org {
        Org::parse(*self)
    }

    fn to_org_string(&self) -> String {
        (*self).to_owned()
    }
}

impl ToOrg for Org {
    fn to_org_string(&self) -> String {
        self.to_org()
    }
}

#[derive(Debug, Clone)]
struct ByETag<T>(T)
where
    T: Debug + Clone;

type Id = String;

macro_rules! def_org_meta {
    ($ty:ty { $($field:ident : $field_ty:ty),* }) => {
        paste::paste!{
            #[derive(Debug, Clone)]
            struct [<$ty Inner>] {
                $(
                    $field: $field_ty,
                )*
            }

            #[derive(Debug, Clone)]
            pub(crate) struct $ty(std::sync::Arc<[<$ty Inner>]>);

            impl From<($($field_ty),*)> for $ty {
                fn from(($($field),*): ($($field_ty),*)) -> Self {
                    $ty(std::sync::Arc::new([<$ty Inner>] {
                        $($field),*
                    }))
                }
            }

            impl $ty {
                $(
                    pub fn $field(&self) -> &$field_ty {
                        &self.0.$field
                    }
                )*
            }
        }
    };
}

use def_org_meta;

#[derive(Debug, Clone, Default)]
pub(crate) struct MaybeIdMap {
    fresh: HashSet<Headline>,
    map: HashMap<Token, Headline>,
}

impl MaybeIdMap {
    fn insert(&mut self, id: Option<Token>, v: Headline) -> Option<Headline> {
        match id {
            Some(id) => self.map.insert(id, v),
            None => self.fresh.replace(v),
        }
    }

    pub(crate) fn len(&self) -> usize {
        self.fresh.len() + self.map.len()
    }

    pub(crate) fn map(&self) -> &HashMap<Token, Headline> {
        &self.map
    }

    #[allow(unused)]
    pub(crate) fn map_mut(&mut self) -> &mut HashMap<Token, Headline> {
        &mut self.map
    }

    #[allow(unused)]
    pub(crate) fn into_map(self) -> HashMap<Token, Headline> {
        self.map
    }

    pub(crate) fn fresh(&self) -> impl Iterator<Item = &Headline> {
        self.fresh.iter()
    }

    pub(crate) fn diff(
        mut self,
        mut other: MaybeIdMap,
    ) -> (MaybeIdMap, MaybeIdMap, HashMap<Token, Headline>) {
        let intersection = self
            .map
            .keys()
            .filter(|k| other.map.contains_key(*k))
            .cloned()
            .collect::<Vec<_>>();
        let changed: HashMap<Token, Headline> = intersection
            .into_iter()
            .filter_map(|k| {
                let old = self.map.remove(&k).unwrap();
                let new = other.map.remove(&k).unwrap();
                (old.raw().trim() != new.raw().trim()).then_some((k, new))
            })
            .collect();

        let _ = other.fresh.extract_if(|h| self.fresh.remove(h));

        (self, other, changed)
    }
}

impl From<&Org> for MaybeIdMap {
    fn from(org: &Org) -> Self {
        let mut map = MaybeIdMap::default();
        let mut handler = from_fn(|event| {
            if let Event::Enter(Container::Headline(headline)) = event {
                let id = headline.properties().and_then(|drawer| drawer.get("id"));
                map.insert(id, headline);
            }
        });
        org.traverse(&mut handler);
        map
    }
}

macro_rules! text_from_property_drawer {
    ($headline:ident, $field:literal) => {
        $headline
            .properties()
            .and_then(|drawer| drawer.get($field))
            .map(|t| t.as_ref().to_owned())
    };
}

use text_from_property_drawer;
