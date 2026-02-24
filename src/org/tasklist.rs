use std::collections::{HashMap, HashSet};
use std::sync::atomic::Ordering;
use std::time::SystemTime;
use std::{
    hash::Hash,
    sync::{Arc, Mutex},
};

use atomic_time::AtomicSystemTime;
use chrono::Local;
use evmap::{ReadHandle, ReadHandleFactory, WriteHandle};
use google_tasks1::api::{Task, TaskList, Tasks};
use itertools::Itertools;
use orgize::ast::Headline;
use orgize::export::{from_fn_with_ctx, Container, Event};
use orgize::Org;

use crate::org::conflict::push_conflict_str;
use crate::org::timestamp::Timestamp;
use crate::org::{Diff, MetaPendingContainer, Move};
use crate::streaming::{digit_stream_to_string, streaming_add, string_to_digit_stream};
use crate::write::{TaskInsert, TaskModify, TaskWrite, WriteCommand};

use super::{def_org_meta, text_from_property_drawer, ByETag, Id, ToOrg};

impl PartialEq for ByETag<Task> {
    fn eq(&self, other: &Self) -> bool {
        self.0.id == other.0.id && self.0.etag == other.0.etag
    }
}

impl Eq for ByETag<Task> {}

impl Hash for ByETag<Task> {
    fn hash<H: std::hash::Hasher>(&self, state: &mut H) {
        self.0.id.hash(state);
        self.0.etag.hash(state);
    }
}

def_org_meta! {
    TaskListMeta {
        tasklist: TaskList,
        updated: AtomicSystemTime,
        pending: (HashSet<TaskInsert>, HashMap<String, TaskModify>)
    }
}

#[derive(Clone)]
pub(crate) struct OrgTaskList(
    ReadHandleFactory<Id, Box<ByETag<Task>>, TaskListMeta>,
    #[allow(clippy::type_complexity)] Arc<Mutex<WriteHandle<Id, Box<ByETag<Task>>, TaskListMeta>>>,
);

impl OrgTaskList {
    pub fn sync(&self, ts: Tasks, updated: SystemTime) {
        let mut guard = self.1.lock().unwrap();
        for mut t in ts.items.unwrap_or_default() {
            bump_position(&mut t);
            let Some(id) = &t.id else {
                tracing::warn!("Task without id found: {:?}", t);
                continue;
            };
            if guard.contains_key(id) {
                // Update existing task
                match t.deleted {
                    Some(true) => {
                        tracing::info!("Removing task: {id}");
                        guard.empty(id.clone());
                    }
                    _ => {
                        tracing::info!("Updating task: {id}");
                        guard.update(id.clone(), Box::new(ByETag(t)));
                    }
                }
            } else {
                // Add new task
                tracing::info!("Adding new task: {id}");
                guard.insert(id.clone(), Box::new(ByETag(t)));
            }
        }
        guard
            .meta()
            .unwrap()
            .updated()
            .store(updated, Ordering::Release);
        guard.refresh();
    }

    pub fn parse_task(headline: &Headline) -> Task {
        Task {
            completed: headline
                .closed()
                .and_then(|p| p.start_to_chrono())
                .map(|dt| dt.and_local_timezone(Local).unwrap().to_rfc3339()),
            due: headline
                .deadline()
                .and_then(|p| p.start_to_chrono())
                .map(|dt| dt.and_local_timezone(Local).unwrap().to_rfc3339()),
            notes: headline.section().map(|s| s.raw().trim().to_owned()),
            status: if headline.is_done() {
                Some("completed".to_owned())
            } else {
                Some("needsAction".to_owned())
            },
            title: Some(headline.title_raw()),
            etag: text_from_property_drawer!(headline, "etag"),
            id: text_from_property_drawer!(headline, "id"),
            ..Task::default()
        }
    }

    pub fn generate_commands(
        tasklist_id: &str,
        diff: Diff,
        tx_wcmd: &tokio::sync::mpsc::UnboundedSender<WriteCommand>,
        new_org: &Org,
    ) -> bool {
        let Diff {
            added,
            removed,
            changed,
            moves,
        } = diff;

        let mut did_write = false;
        for id in removed.map().keys() {
            tracing::info!("Removing task with id {:?}", id);
            tx_wcmd
                .send(WriteCommand::Task {
                    tasklist_id: tasklist_id.to_owned(),
                    cmd: TaskWrite::Modify {
                        task_id: id.to_string(),
                        modification: TaskModify::Delete,
                    },
                })
                .expect("Failed to send task delete command");
            did_write = true;
        }
        for Move {
            id: from,
            before: pred,
            after: succ,
        } in moves
        {
            tracing::info!("Moving task {from:?} between ({pred:?}, {succ:?})");
            tx_wcmd
                .send(WriteCommand::Task {
                    tasklist_id: tasklist_id.to_owned(),
                    cmd: TaskWrite::Move {
                        task_id: from.to_string(),
                        new_predecessor: pred.map(|x| x.to_string()),
                        new_successor: succ.map(|x| x.to_string()),
                    },
                })
                .expect("Failed to send task move command");
            did_write = true;
        }
        for (id, updated) in changed {
            let task = OrgTaskList::parse_task(&updated).into();
            tracing::info!("Modifying task with id {:?}: {:?}", id, task);
            tx_wcmd
                .send(WriteCommand::Task {
                    tasklist_id: tasklist_id.to_owned(),
                    cmd: TaskWrite::Modify {
                        task_id: id.to_string(),
                        modification: TaskModify::Patch { task },
                    },
                })
                .expect("Failed to send task modify command");
            did_write = true;
        }
        for headline in added.fresh().sorted_by_key(|h| h.start()).rev() {
            let task = OrgTaskList::parse_task(headline).into();
            tracing::info!("Adding new task: {:?}", task);
            // TODO: currently, we can only add subtasks to tasks which are
            // already on the server (they have ids)
            let mut new_parent = None;
            let mut new_predecessor = None;
            let mut new_successor = None;
            let mut prev = None;
            let mut handler = from_fn_with_ctx(|event, ctx| {
                // find the last headline on the same level with an id before this one
                if let Event::Enter(Container::Headline(cur)) = event {
                    if &cur == headline {
                        ctx.skip();
                    } else if prev.as_ref() == Some(headline) {
                        new_successor = cur
                            .properties()
                            .and_then(|props| props.get("id"))
                            .map(|id| id.to_string());
                        ctx.stop();
                    } else {
                        match cur.level().cmp(&headline.level()) {
                            std::cmp::Ordering::Less => {
                                if let Some(id) = cur.properties().and_then(|props| props.get("id"))
                                {
                                    new_parent.replace(id.to_string());
                                }
                            }
                            std::cmp::Ordering::Equal => {
                                if let Some(id) = cur.properties().and_then(|props| props.get("id"))
                                {
                                    new_predecessor.replace(id.to_string());
                                }
                            }
                            std::cmp::Ordering::Greater => {
                                ctx.up();
                            }
                        }
                    }
                    prev.replace(cur);
                }
            });
            new_org.traverse(&mut handler);
            tx_wcmd
                .send(WriteCommand::Task {
                    tasklist_id: tasklist_id.to_owned(),
                    cmd: TaskWrite::Insert(TaskInsert::Insert { task }),
                })
                .expect("Failed to send task insert command");
            did_write = true;
        }

        did_write
    }
}

impl MetaPendingContainer for OrgTaskList {
    type Meta = TaskListMeta;
    type Item = Task;
    type Insert = TaskInsert;
    type Modify = TaskModify;

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
            meta.tasklist().clone(),
            AtomicSystemTime::new(meta.updated().load(Ordering::Acquire)),
            pending,
        )
            .into()
    }
}

impl From<(TaskList, Tasks)> for OrgTaskList {
    fn from(ts: (TaskList, Tasks)) -> Self {
        let updated = AtomicSystemTime::new(
            ts.0.updated
                .as_ref()
                .and_then(|str| {
                    chrono::DateTime::parse_from_rfc3339(str)
                        .ok()
                        .map(|x| x.into())
                })
                .unwrap_or(std::time::UNIX_EPOCH),
        );
        let (rh, mut wh) = evmap::with_meta((ts.0, updated, Default::default()).into());
        wh.extend(ts.1.items.unwrap_or_default().into_iter().map(|mut task| {
            let id = task.id.clone().unwrap_or_default();
            bump_position(&mut task);
            (id, Box::new(ByETag(task)))
        }));
        wh.refresh();
        Self(rh.factory(), Arc::new(Mutex::new(wh)))
    }
}

pub(crate) fn bump_position(task: &mut Task) {
    // increment Task position to free up 00000000000000000000
    if let Some(p) = task.position.iter_mut().next() {
        *p = digit_stream_to_string(streaming_add(
            string_to_digit_stream(&*p),
            std::iter::chain(std::iter::repeat_n(0, 19), std::iter::once(1)),
        ));
    }
}

impl ToOrg for OrgTaskList {
    fn to_org_string(&self) -> String {
        let handle = self.0.handle();
        let meta = handle.meta().expect("meta not found");
        let pending = meta.pending();
        let read_ref = handle.read().unwrap();
        [
            read_ref
                .iter()
                .sorted_by_key(|(id, tasks)| {
                    let task = tasks
                        .get_one()
                        .unwrap_or_else(|| panic!("No tasks found for id: {id}"));
                    format!(
                        "{}{}",
                        task.0
                            .parent
                            .as_ref()
                            .and_then(|id| {
                                let parent = read_ref[id]
                                    .get_one()
                                    .unwrap_or_else(|| panic!("No tasks found for id: {id}"));
                                parent.0.position.clone()
                            })
                            .unwrap_or_default(),
                        task.0.position.as_deref().unwrap_or_default(),
                    )
                })
                .map(|(id, tasks)| {
                    let task = tasks
                        .get_one()
                        .unwrap_or_else(|| panic!("No tasks found for id: {id}"));
                    let level = if task.0.parent.is_some() { "**" } else { "*" };
                    let mut str = String::new();
                    match pending.1.get(id) {
                        Some(TaskModify::Patch { task: new_task }) => {
                            push_conflict_str(
                                &mut str,
                                &render_task(&task.0, format!("{level} COMMENT "), true),
                                &render_task(new_task, format!("{level} "), false),
                            );
                        }
                        Some(TaskModify::Delete) => {
                            push_conflict_str(
                                &mut str,
                                &render_task(&task.0, format!("{level} COMMENT "), true),
                                "",
                            );
                        }
                        None => str.push_str(&render_task(&task.0, format!("{level} "), true)),
                    }
                    str
                })
                .collect::<Vec<_>>(),
            pending
                .0
                .iter()
                .map(|TaskInsert::Insert { task }| {
                    let mut str = String::new();
                    push_conflict_str(&mut str, "", &render_task(task, "* ".to_owned(), false));
                    str
                })
                .collect::<Vec<_>>(),
        ]
        .concat()
        .join("\n")
    }
}

fn render_task(task: &Task, prefix: String, with_properties: bool) -> String {
    // HEADLINE
    let mut str = prefix;
    let mut planning = String::new();
    if let Some(done) = &task
        .completed
        .as_ref()
        .and_then(|str| chrono::DateTime::parse_from_rfc3339(str).ok())
        .map(|dt| dt.with_timezone(&Local))
    {
        planning.push_str("CLOSED: ");
        planning.push_str(&Timestamp::from(*done).deactivate().to_org_string());
    } else {
        str.push_str("TODO ");
        if let Some(due) = &task
            .due
            .as_ref()
            .and_then(|str| chrono::DateTime::parse_from_rfc3339(str).ok())
            .map(|dt| dt.with_timezone(&Local))
        {
            planning.push_str("DEADLINE: ");
            planning.push_str(&Timestamp::from(*due).to_org_string());
        }
    }
    if let Some(title) = &task.title {
        str.push_str(title);
    }
    str.push('\n');

    // PLANNING
    if !planning.is_empty() {
        str.push_str(&planning);
        str.push('\n');
    }

    if with_properties {
        // PROPERTIES
        str.push_str(":PROPERTIES:");
        str.push('\n');
        macro_rules! print_property {
            ($p:ident) => {
                if let Some($p) = &task.$p {
                    str.push_str(":");
                    str.push_str(stringify!($p));
                    str.push_str(": ");
                    str.push_str(&$p.to_org_string());
                    str.push('\n');
                }
            };
        }
        print_property!(etag);
        print_property!(id);
        print_property!(updated);
        print_property!(self_link);
        print_property!(web_view_link);
        if let Some(links) = &task.links {
            str.push_str(&format!(":links: {:?}", links));
            str.push('\n');
        }
        str.push_str(":END:");
        str.push('\n');
    }

    // SECTION
    if let Some(notes) = &task.notes {
        str.push('\n');
        str.push_str(notes);
        str.push('\n');
    }

    str
}
