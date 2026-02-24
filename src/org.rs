use std::{
    collections::{HashMap, HashSet},
    fmt::Debug,
    hash::Hash,
    sync::MutexGuard,
};

use evmap::{ReadHandle, WriteHandle};
use itertools::Itertools;
use orgize::{
    ast::{Headline, Token},
    export::{from_fn, Container, Event},
    Org,
};

pub(crate) mod calendar;
pub(crate) mod conflict;
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
pub(crate) struct ByETag<T>(pub(super) T)
where
    T: Debug + Clone;

type Id = String;

macro_rules! def_org_meta {
    ($ty:ty { $($field:ident : $field_ty:ty),* }) => {
        paste::paste!{
            struct [<$ty Inner>] {
                $(
                    $field: $field_ty,
                )*
            }

            #[derive(Clone)]
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

#[derive(Debug)]
pub(crate) struct Move {
    pub(crate) id: Token,
    pub(crate) before: Option<Token>,
    pub(crate) after: Option<Token>,
}

#[derive(Debug)]
pub(crate) struct Diff {
    pub(crate) added: MaybeIdMap,
    pub(crate) removed: MaybeIdMap,
    pub(crate) changed: HashMap<Token, Headline>,
    pub(crate) moves: Vec<Move>,
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

    pub(crate) fn diff(mut self, mut other: MaybeIdMap) -> Diff {
        let intersection = self
            .map
            .keys()
            .filter(|k| other.map.contains_key(*k))
            .cloned()
            .collect::<Vec<_>>();
        let moves = {
            let permutation = intersection
                .iter()
                .sorted_unstable_by_key(|k| other.map[*k].start())
                .enumerate()
                .sorted_unstable_by_key(|(_, k)| self.map[*k].start())
                .collect::<Vec<_>>();
            fn longest_increasing_subsequence<T: Debug, K: Ord, F: Fn(&T) -> K>(
                sequence: &[T],
                key: F,
            ) -> Vec<K> {
                let mut iter = sequence.iter().enumerate();
                let (i, x) = iter.next().unwrap();

                let mut min_ending_of_length = Vec::new();
                let mut predecessor = vec![None; sequence.len()];
                min_ending_of_length.push((i, x));
                for (i, x) in iter {
                    let (prev_i, prev_longest) = min_ending_of_length.last().copied().unwrap();
                    if key(x) > key(prev_longest) {
                        predecessor[i] = Some(prev_i);
                        min_ending_of_length.push((i, x));
                    } else {
                        let j = min_ending_of_length.partition_point(|(_, y)| key(y) < key(x));
                        // j is the first such that key(x) <= key(min_ending_of_length[j].1)
                        predecessor[i] = predecessor[j];
                        min_ending_of_length[j] = (i, x);
                    }
                }
                let mut lis = Vec::with_capacity(min_ending_of_length.len());
                let (mut i, _) = min_ending_of_length.pop().unwrap();
                lis.push(key(&sequence[i]));
                while let Some(prev_i) = predecessor[i] {
                    lis.push(key(&sequence[prev_i]));
                    i = prev_i;
                }
                lis.reverse();
                lis
            }
            let mut lis = longest_increasing_subsequence(&permutation, |(i, _)| *i);
            let mut moves = Vec::with_capacity(permutation.len() - lis.len());
            for (i, id) in &permutation {
                if lis.binary_search(i).is_err() {
                    let j = lis.partition_point(|&x| x < *i);
                    // j first such that lis[j] >= i
                    moves.push(Move {
                        id: (*id).clone(),
                        before: j
                            .checked_sub(1)
                            .and_then(|j| lis.get(j))
                            .map(|t| permutation.iter().find(|(k, _)| k == t).unwrap().1.clone()),
                        after: lis
                            .get(j)
                            .map(|t| permutation.iter().find(|(k, _)| k == t).unwrap().1.clone()),
                    });
                    lis.insert(j, *i);
                }
            }
            moves
        };
        let changed: HashMap<Token, Headline> = intersection
            .into_iter()
            .filter_map(|k| {
                let old = self.map.remove(&k).unwrap();
                let new = other.map.remove(&k).unwrap();
                (old.raw().trim() != new.raw().trim()).then_some((k, new))
            })
            .collect();

        let _ = other.fresh.extract_if(|h| self.fresh.remove(h));

        Diff {
            added: other,
            removed: self,
            changed,
            moves,
        }
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

pub(crate) trait MetaPendingContainer
where
    ByETag<Self::Item>: Eq + Hash,
{
    type Meta: Clone + 'static;
    type Item: Debug + Clone;
    type Insert: Clone + Eq + Hash;
    type Modify: Clone;

    fn with_meta<T>(&self, f: impl FnOnce(&Self::Meta) -> T) -> T;
    fn with_pending<T>(
        &self,
        f: impl FnOnce(&(HashSet<Self::Insert>, HashMap<Id, Self::Modify>)) -> T,
    ) -> T;
    fn read(&self) -> ReadHandle<Id, Box<ByETag<Self::Item>>, Self::Meta>;
    #[allow(clippy::type_complexity)]
    fn write(&self) -> MutexGuard<'_, WriteHandle<Id, Box<ByETag<Self::Item>>, Self::Meta>>;
    fn update_pending(
        meta: &Self::Meta,
        pending: (HashSet<Self::Insert>, HashMap<Id, Self::Modify>),
    ) -> Self::Meta;

    fn get_id(&self, id: &str) -> Option<Box<ByETag<Self::Item>>> {
        self.read().get_one(id).as_deref().cloned()
    }

    fn add_id(&self, id: &str, item: Self::Item) {
        let mut guard = self.write();
        tracing::debug!("Adding item: {id}");
        guard.insert(id.to_owned(), Box::new(ByETag(item)));
        guard.refresh();
    }

    fn update_id(&self, id: &str, item: Self::Item) {
        let mut guard = self.write();
        tracing::debug!("Updating item: {id}");
        guard.update(id.to_owned(), Box::new(ByETag(item)));
        guard.refresh();
    }

    fn delete_id(&self, id: &str) {
        let mut guard = self.write();
        tracing::debug!("Deleting item: {id}");
        guard.empty(id.to_owned());
        guard.refresh();
    }

    fn push_pending_insert(&self, insert: Self::Insert) {
        let mut guard = self.write();
        let mut new_pending = self.with_pending(Clone::clone);
        new_pending.0.insert(insert);
        guard.set_meta(self.with_meta(|m| Self::update_pending(m, new_pending)));
        guard.refresh();
    }

    fn push_pending_modify(&self, id: Id, modify: Self::Modify) {
        let mut guard = self.write();
        let mut new_pending = self.with_pending(Clone::clone);
        new_pending.1.insert(id, modify);
        guard.set_meta(self.with_meta(|m| Self::update_pending(m, new_pending)));
        guard.refresh();
    }

    fn clear_pending(&self) -> Self::Meta {
        let mut guard = self.write();
        let new_pending = Default::default();
        let meta = guard.set_meta(self.with_meta(|m| Self::update_pending(m, new_pending)));
        guard.refresh();
        meta
    }
}
