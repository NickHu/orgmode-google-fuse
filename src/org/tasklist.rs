use std::time::SystemTime;
use std::{
    hash::Hash,
    sync::{Arc, Mutex},
};

use evmap::{ReadHandleFactory, WriteHandle};
use google_tasks1::api::{Task, TaskList, Tasks};

use super::{def_org_meta, ByETag, Id, ToOrg};

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
    TaskListMeta { tasklist: TaskList, updated: SystemTime }
}

#[derive(Debug, Clone)]
pub(crate) struct OrgTaskList(
    ReadHandleFactory<Id, Box<ByETag<Task>>, TaskListMeta>,
    #[allow(clippy::type_complexity)] Arc<Mutex<WriteHandle<Id, Box<ByETag<Task>>, TaskListMeta>>>,
);

impl OrgTaskList {
    pub fn meta(&self) -> TaskListMeta {
        self.0.handle().meta().expect("meta not found").clone()
    }

    pub fn sync(&self, ts: Tasks) {
        let mut guard = self.1.lock().unwrap();
        for t in ts.items.unwrap_or_default() {
            let Some(id) = &t.id else {
                tracing::warn!("Task without id found: {:?}", t);
                continue;
            };
            if guard.contains_key(id) {
                {
                    let v = guard.get_one(id).unwrap();
                    if v.0.etag == t.etag {
                        tracing::debug!("Task {id} is up to date, skipping update");
                        continue;
                    }
                }
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
        guard.refresh();
    }
}

impl From<(TaskList, Tasks)> for OrgTaskList {
    fn from(ts: (TaskList, Tasks)) -> Self {
        let updated =
            ts.0.updated
                .as_ref()
                .and_then(|str| {
                    chrono::DateTime::parse_from_rfc3339(str)
                        .ok()
                        .map(|x| x.into())
                })
                .unwrap_or(std::time::UNIX_EPOCH);
        let (rh, mut wh) = evmap::with_meta((ts.0, updated).into());
        wh.extend(ts.1.items.unwrap_or_default().into_iter().map(|task| {
            let id = task.id.clone().unwrap_or_default();
            (id, Box::new(ByETag(task)))
        }));
        wh.refresh();
        Self(rh.factory(), Arc::new(Mutex::new(wh)))
    }
}

impl ToOrg for OrgTaskList {
    fn to_org_string(&self) -> String {
        self.0
            .handle()
            .map_into::<_, Vec<_>, _>(|id, tasks| {
                let task = tasks
                    .get_one()
                    .unwrap_or_else(|| panic!("No tasks found for id: {id}"));
                // HEADLINE
                let mut str = "* ".to_owned();
                let mut planning = String::new();
                if let Some(done) = &task.0.completed {
                    planning.push_str("CLOSED: ");
                    planning.push('[');
                    planning.push_str(done);
                    planning.push(']');
                } else {
                    str.push_str("TODO ");
                    if let Some(due) = &task.0.due {
                        planning.push_str("DEADLINE: ");
                        planning.push('<');
                        planning.push_str(due);
                        planning.push('>');
                    }
                }
                if let Some(title) = &task.0.title {
                    str.push_str(title);
                }
                str.push('\n');

                // PLANNING
                if !planning.is_empty() {
                    str.push_str(&planning);
                    str.push('\n');
                }

                // PROPERTIES
                str.push_str(":PROPERTIES:");
                str.push('\n');
                macro_rules! print_property {
                    ($p:ident) => {
                        if let Some($p) = &task.0.$p {
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
                if let Some(links) = &task.0.links {
                    str.push_str(&format!(":links: {:?}", links));
                    str.push('\n');
                }
                str.push_str(":END:\n");

                // SECTION
                if let Some(notes) = &task.0.notes {
                    str.push('\n');
                    str.push_str(notes);
                    str.push('\n');
                }

                str
            })
            .join("\n")
    }
}
