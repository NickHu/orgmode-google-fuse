use google_tasks1::api::{Task, TaskList};

#[derive(Debug, Clone)]
pub(crate) struct OrgTaskList(pub(crate) TaskList, pub(crate) Vec<Task>);

impl From<(TaskList, Vec<Task>)> for OrgTaskList {
    fn from(ts: (TaskList, Vec<Task>)) -> Self {
        Self(ts.0, ts.1)
    }
}

impl OrgTaskList {
    pub(crate) fn to_org(&self) -> String {
        self.1
            .iter()
            .map(|task| {
                // HEADLINE
                let mut str = "* ".to_owned();
                let mut planning = String::new();
                if let Some(done) = &task.completed {
                    planning.push_str("  CLOSED: ");
                    planning.push('[');
                    planning.push_str(done);
                    planning.push(']');
                } else {
                    str.push_str("TODO ");
                    if let Some(due) = &task.due {
                        planning.push_str("  DEADLINE: ");
                        planning.push('<');
                        planning.push_str(due);
                        planning.push('>');
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

                // PROPERTIES
                str.push_str("  :PROPERTIES:");
                str.push('\n');
                macro_rules! print_property {
                    ($p:ident) => {
                        if let Some($p) = &task.$p {
                            str.push_str("  :");
                            str.push_str(stringify!($p));
                            str.push_str(": ");
                            str.push_str($p);
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
                    str.push_str(&format!("  :links: {:#?}", links));
                    str.push('\n');
                }
                str.push_str("  :END:");
                str.push('\n');

                // SECTION
                if let Some(notes) = &task.notes {
                    str.push_str("  ");
                    str.push_str(notes);
                    str.push('\n');
                }

                str.push('\n');
                str
            })
            .collect()
    }
}
