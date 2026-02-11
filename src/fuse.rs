use std::{
    collections::HashMap,
    ffi::OsStr,
    sync::{atomic::Ordering, Arc, Mutex},
    time::{Duration, SystemTime, UNIX_EPOCH},
};

use fuser::{
    FileAttr, FileType, Filesystem, ReplyAttr, ReplyData, ReplyEmpty, ReplyEntry, ReplyOpen,
    ReplyWrite, Request, TimeOrNow,
};
use itertools::Itertools;
use libc::{EBADF, EINVAL, ENOENT, ENOTDIR};
use orgize::Org;

use crate::{org::ToOrg, Pid};
use crate::{
    org::{
        calendar::OrgCalendar, conflict::read_conflict_local, tasklist::OrgTaskList, MaybeIdMap,
        MetaPendingContainer,
    },
    write::{
        CalendarEventInsert, CalendarEventModify, CalendarEventWrite, TaskInsert, TaskModify,
        TaskWrite, WriteCommand,
    },
};

const BLKSIZE: u32 = 512;
const DEFAULT_DIR_ATTR: FileAttr = FileAttr {
    ino: 0,
    size: 0,
    blocks: 0,
    atime: UNIX_EPOCH,
    mtime: UNIX_EPOCH,
    ctime: UNIX_EPOCH,
    crtime: UNIX_EPOCH,
    kind: FileType::Directory,
    perm: 0o755,
    nlink: 2,
    uid: 0,
    gid: 0,
    rdev: 0,
    blksize: BLKSIZE,
    flags: 0,
};
const DEFAULT_FILE_ATTR: FileAttr = FileAttr {
    ino: 0,
    size: 0,
    blocks: 0,
    atime: UNIX_EPOCH,
    mtime: UNIX_EPOCH,
    ctime: UNIX_EPOCH,
    crtime: UNIX_EPOCH,
    kind: FileType::RegularFile,
    perm: 0o644,
    nlink: 1,
    uid: 0,
    gid: 0,
    rdev: 0,
    blksize: BLKSIZE,
    flags: 0,
};

type Inode = u64;
type FileHandle = u64;
type Instance = (Inode, Pid);

pub(crate) struct InstanceState {
    pub(super) file_handles: Vec<FileHandle>,
    org: Org,
    write_buffer: Vec<u8>,
    write_time: SystemTime,
}

pub(crate) struct OrgFS {
    pub(crate) uid: u32,
    pub(crate) gid: u32,
    pub(crate) calendars: Vec<(Inode, OrgCalendar)>,
    pub(crate) tasklists: Vec<(Inode, OrgTaskList)>,
    tx_wcmd: tokio::sync::mpsc::UnboundedSender<WriteCommand>,
    tx_fh: tokio::sync::mpsc::UnboundedSender<Pid>,
    #[allow(clippy::type_complexity)]
    pending_fh: Arc<Mutex<HashMap<Instance, InstanceState>>>,
}

const TTL: Duration = Duration::new(0, 0);

const ROOT_DIR_INO: Inode = 1;
const fn root_dir_attr(uid: u32, gid: u32) -> FileAttr {
    FileAttr {
        ino: ROOT_DIR_INO,
        uid,
        gid,
        ..DEFAULT_DIR_ATTR
    }
}

const CALENDAR_DIR_INO: Inode = 2;
const fn calendar_dir_attr(uid: u32, gid: u32) -> FileAttr {
    FileAttr {
        ino: CALENDAR_DIR_INO,
        uid,
        gid,
        ..DEFAULT_DIR_ATTR
    }
}

const TASKS_DIR_INO: Inode = 3;
const fn tasks_dir_attr(uid: u32, gid: u32) -> FileAttr {
    FileAttr {
        ino: TASKS_DIR_INO,
        uid,
        gid,
        ..DEFAULT_DIR_ATTR
    }
}

const fn file_attr(uid: u32, gid: u32, ino: Inode, size: u64, time: SystemTime) -> FileAttr {
    let blocks = size.div_ceil(BLKSIZE as u64);
    FileAttr {
        ino,
        size,
        blocks,
        atime: time,
        mtime: time,
        ctime: time,
        crtime: time,
        uid,
        gid,
        ..DEFAULT_FILE_ATTR
    }
}

const FILE_START_OFFSET: Inode = TASKS_DIR_INO + 1;

impl OrgFS {
    #[allow(clippy::type_complexity)]
    pub(crate) fn new(
        calendars: Arc<Vec<OrgCalendar>>,
        tasklists: Arc<Vec<OrgTaskList>>,
        tx_wcmd: tokio::sync::mpsc::UnboundedSender<WriteCommand>,
        tx_fh: tokio::sync::mpsc::UnboundedSender<Pid>,
        pending_fh: Arc<Mutex<HashMap<Instance, InstanceState>>>,
    ) -> Self {
        let csl = calendars.len();
        Self {
            uid: nix::unistd::getuid().as_raw(),
            gid: nix::unistd::getgid().as_raw(),
            calendars: calendars
                .iter()
                .cloned()
                .enumerate()
                .map(|(i, cal)| (FILE_START_OFFSET + i as u64, cal))
                .collect(),
            tasklists: tasklists
                .iter()
                .cloned()
                .enumerate()
                .map(|(i, tl)| (FILE_START_OFFSET + csl as u64 + i as u64, tl))
                .collect(),
            tx_wcmd,
            tx_fh,
            pending_fh,
        }
    }

    fn is_calendar_file(&self, ino: Inode) -> bool {
        FILE_START_OFFSET <= ino && ino < FILE_START_OFFSET + self.calendars.len() as Inode
    }

    fn is_tasks_file(&self, ino: Inode) -> bool {
        FILE_START_OFFSET + self.calendars.len() as Inode <= ino
            && ino
                < FILE_START_OFFSET + self.calendars.len() as Inode + self.tasklists.len() as Inode
    }

    fn allocate_stateful_file_handle(&mut self, ino: Inode, pid: u32) -> u64 {
        // vim and many other editors open a file, read it into memory, and then release the file
        // handle almost immediately, as opposed to holding a file handle open for a session.
        // We need to reconcile when a file is written from vim but we have no access to vim's
        // buffer contents (vim won't hold any open file handles and the backing data may have
        // changed in the meantime).
        // The idea behind this is to snapshot the state as Org per PID the first time any process
        // opens a file. Subsequent reads will return the **latest data**, but writes from this
        // process are reconciled against this snapshot.
        //
        // Lifecycle:
        // * allocated on `open`, freed on `release` or pid exit
        // * used by `setattr` and `write` for write buffer, and `fsync` to reconcile changes
        // * fast-forwarded on `read`
        if let Some((org, updated)) = match ino {
            i if self.is_calendar_file(i) => {
                self.calendars
                    .iter()
                    .find(|(ino, _)| ino == &i)
                    .map(|(_, cal)| {
                        (
                            cal.to_org(),
                            cal.with_meta(|m| m.updated().load(Ordering::Acquire)),
                        )
                    })
            }
            i if self.is_tasks_file(i) => {
                self.tasklists
                    .iter()
                    .find(|(ino, _)| ino == &i)
                    .map(|(_, tl)| {
                        (
                            tl.to_org(),
                            tl.with_meta(|m| m.updated().load(Ordering::Acquire)),
                        )
                    })
            }
            _ => None,
        } {
            let mut guard = self.pending_fh.lock().unwrap();
            if guard.keys().all(|(_, p)| *p != pid) {
                // newly opened file, watch the pid
                self.tx_fh.send(pid).unwrap();
            }
            let fh = guard
                .iter()
                .filter(|&((_, p), _)| pid == *p)
                .flat_map(|(_, InstanceState { file_handles, .. })| file_handles)
                .sorted()
                .copied()
                .chain(std::iter::once(0))
                .zip(1..)
                .find_map(|(fh, i)| (fh != i).then_some(i))
                .unwrap();
            let write_buffer = org.to_org_string().into();
            guard
                .entry((ino, pid))
                .or_insert(InstanceState {
                    file_handles: Vec::default(),
                    org,
                    write_buffer,
                    write_time: updated,
                })
                .file_handles
                .push(fh);
            fh
        } else {
            0
        }
    }

    fn get_inode(&self, ino: Inode) -> Option<FileAttr> {
        match ino {
            ROOT_DIR_INO => Some(root_dir_attr(self.uid, self.gid)),
            CALENDAR_DIR_INO => Some(calendar_dir_attr(self.uid, self.gid)),
            TASKS_DIR_INO => Some(tasks_dir_attr(self.uid, self.gid)),
            i if self.is_calendar_file(i) => {
                self.calendars
                    .iter()
                    .find(|(ino, _)| ino == &i)
                    .map(|(_, cal)| {
                        file_attr(
                            self.uid,
                            self.gid,
                            i,
                            cal.to_org_string().len() as u64,
                            cal.with_meta(|m| m.updated().load(Ordering::Acquire)),
                        )
                    })
            }
            i if self.is_tasks_file(i) => {
                self.tasklists
                    .iter()
                    .find(|(ino, _)| ino == &i)
                    .map(|(_, tl)| {
                        file_attr(
                            self.uid,
                            self.gid,
                            i,
                            tl.to_org_string().len() as u64,
                            tl.with_meta(|m| m.updated().load(Ordering::Acquire)),
                        )
                    })
            }
            _ => None,
        }
    }
}

impl Filesystem for OrgFS {
    fn lookup(&mut self, _req: &Request, parent: Inode, name: &OsStr, reply: ReplyEntry) {
        if let Some(fileattr) = match parent {
            ROOT_DIR_INO => match name.to_str() {
                Some("calendars") => Some(calendar_dir_attr(self.uid, self.gid)),
                Some("tasks") => Some(tasks_dir_attr(self.uid, self.gid)),
                _ => None,
            },
            CALENDAR_DIR_INO => name.to_str().and_then(|filename| {
                self.calendars.iter().find_map(|(ino, cal)| {
                    cal.with_meta(|m| {
                        m.calendar()
                            .summary
                            .as_ref()
                            .filter(|summary| format!("{}.org", summary) == filename)
                            .map(|_| {
                                file_attr(
                                    self.uid,
                                    self.gid,
                                    *ino,
                                    cal.to_org_string().len() as u64,
                                    cal.with_meta(|m| m.updated().load(Ordering::Acquire)),
                                )
                            })
                    })
                })
            }),
            TASKS_DIR_INO => name.to_str().and_then(|filename| {
                self.tasklists.iter().find_map(|(ino, tl)| {
                    tl.with_meta(|m| {
                        m.tasklist()
                            .title
                            .as_ref()
                            .filter(|title| format!("{}.org", title) == filename)
                            .map(|_| {
                                file_attr(
                                    self.uid,
                                    self.gid,
                                    *ino,
                                    tl.to_org_string().len() as u64,
                                    tl.with_meta(|m| m.updated().load(Ordering::Acquire)),
                                )
                            })
                    })
                })
            }),
            _ => None,
        } {
            reply.entry(&TTL, &fileattr, 0);
        } else {
            reply.error(ENOENT);
        }
    }

    fn getattr(&mut self, req: &Request, ino: Inode, _fh: Option<u64>, reply: ReplyAttr) {
        if let Some(InstanceState {
            write_buffer,
            write_time,
            ..
        }) = self.pending_fh.lock().unwrap().get(&(ino, req.pid()))
        {
            reply.attr(
                &TTL,
                &file_attr(
                    self.uid,
                    self.gid,
                    ino,
                    write_buffer.len() as u64,
                    *write_time,
                ),
            );
        } else if let Some(fileattr) = self.get_inode(ino) {
            reply.attr(&TTL, &fileattr);
        } else {
            reply.error(ENOENT);
        }
    }

    fn setattr(
        &mut self,
        req: &Request,
        ino: u64,
        _mode: Option<u32>,
        _uid: Option<u32>,
        _gid: Option<u32>,
        size: Option<u64>,
        _atime: Option<TimeOrNow>,
        _mtime: Option<TimeOrNow>,
        _ctime: Option<SystemTime>,
        _fh: Option<u64>,
        _crtime: Option<SystemTime>,
        _chgtime: Option<SystemTime>,
        _bkuptime: Option<SystemTime>,
        _flags: Option<u32>,
        reply: ReplyAttr,
    ) {
        if let Some(mut attrs) = self.get_inode(ino) {
            if let Some(size) = size {
                if size == 0 {
                    if let Some(InstanceState { write_buffer, .. }) =
                        self.pending_fh.lock().unwrap().get_mut(&(ino, req.pid()))
                    {
                        attrs.blocks = 0;
                        attrs.size = 0;
                        write_buffer.clear();
                    } else {
                        tracing::warn!(
                            "Zero-truncate requested on a file that is not open, ino: {}",
                            ino
                        );
                    }
                } else {
                    tracing::error!(
                        "Unsupported non-zero truncate requested, ino: {}, size: {}",
                        ino,
                        size
                    );
                }
            }
            tracing::trace!(
                "setattr pending_fh: {:?}",
                self.pending_fh
                    .lock()
                    .unwrap()
                    .iter()
                    .map(|(x, InstanceState { file_handles, .. })| (x, file_handles))
                    .collect::<Vec<_>>()
            );
            reply.attr(&TTL, &attrs);
        } else {
            reply.error(ENOENT);
        };
    }

    fn write(
        &mut self,
        req: &Request<'_>,
        ino: u64,
        fh: u64,
        offset: i64,
        data: &[u8],
        _write_flags: u32,
        _flags: i32,
        _lock_owner: Option<u64>,
        reply: ReplyWrite,
    ) {
        if let Some(InstanceState {
            file_handles,
            write_buffer,
            ..
        }) = self.pending_fh.lock().unwrap().get_mut(&(ino, req.pid()))
        {
            assert!(file_handles.contains(&fh));
            assert_eq!(offset as usize, write_buffer.len());
            write_buffer.extend_from_slice(data);
        } else {
            reply.error(EBADF);
            return;
        }
        tracing::trace!(
            "write pending_fh: {:?}",
            self.pending_fh
                .lock()
                .unwrap()
                .iter()
                .map(|(x, InstanceState { file_handles, .. })| (x, file_handles))
                .collect::<Vec<_>>()
        );
        reply.written(data.len() as u32);
    }

    fn fsync(&mut self, req: &Request<'_>, ino: u64, _fh: u64, _datasync: bool, reply: ReplyEmpty) {
        if let Some(_attrs) = self.get_inode(ino) {
            // sync with online here
            if let Some(InstanceState {
                org,
                write_buffer,
                write_time,
                ..
            }) = self.pending_fh.lock().unwrap().get_mut(&(ino, req.pid()))
            {
                let written = String::from_utf8_lossy(write_buffer);

                // compute diff
                let old = MaybeIdMap::from(&*org);
                tracing::debug!("Old: {:?} ", old);
                let n_old = old.len();
                let new_org = Org::parse(read_conflict_local(&written));
                let new = MaybeIdMap::from(&new_org);
                tracing::debug!("New: {:?} ", new);
                let (removed, added, (changed, moves)) = old.diff(new);
                tracing::debug!(
                    "Computed diff\nRemoved: {:?}\nAdded: {:?}\nChanged: {:?}",
                    removed,
                    added,
                    changed
                );
                assert!(removed.len() < n_old,
                    "Refusing to delete **all** existing entries to prevent data loss\nThis is probably a bug");
                for (id, headline) in added.map() {
                    tracing::warn!(
                        "Found new entry with ID {} we didn't know about: {}",
                        id,
                        headline.title_raw()
                    );
                }
                for headline in removed.fresh() {
                    tracing::warn!("Found removed entry without ID: {}", headline.title_raw());
                }

                match ino {
                    i if self.is_calendar_file(i) => {
                        let orgcal = self
                            .calendars
                            .iter()
                            .find(|(ino, _)| ino == &i)
                            .map(|(_, cal)| cal)
                            .expect("Calendar file not found during fsync");
                        orgcal.clear_pending();
                        orgcal.with_meta(|meta| {
                            let calendar_id = meta.calendar().id.as_ref().unwrap();

                            let mut did_write = false;
                            for id in removed.map().keys() {
                                tracing::info!("Removing event with id {:?}", id);
                                self.tx_wcmd
                                    .send(WriteCommand::CalendarEvent {
                                        calendar_id: calendar_id.clone(),
                                        cmd: CalendarEventWrite::Modify {
                                            event_id: id.to_string(),
                                            modification: CalendarEventModify::Delete,
                                        },
                                    })
                                    .expect("Failed to send event delete command");
                                did_write = true;
                            }
                            for (id, updated) in changed {
                                let event = OrgCalendar::parse_event(&updated).into();
                                tracing::info!("Modifying event with id {:?}: {:?}", id, event);
                                self.tx_wcmd
                                    .send(WriteCommand::CalendarEvent {
                                        calendar_id: calendar_id.clone(),
                                        cmd: CalendarEventWrite::Modify {
                                            event_id: id.to_string(),
                                            modification: CalendarEventModify::Patch { event },
                                        },
                                    })
                                    .expect("Failed to send event modify command");
                                did_write = true;
                            }
                            for headline in added.fresh() {
                                let event = OrgCalendar::parse_event(headline).into();
                                tracing::info!("Adding new event: {:?}", event);
                                self.tx_wcmd
                                    .send(WriteCommand::CalendarEvent {
                                        calendar_id: calendar_id.clone(),
                                        cmd: CalendarEventWrite::Insert(
                                            CalendarEventInsert::Insert { event },
                                        ),
                                    })
                                    .expect("Failed to send event insert command");
                                did_write = true;
                            }

                            if did_write {
                                tracing::debug!("Updating cached Org for ino: {}", ino);
                                *org = new_org;
                                *write_time = SystemTime::now();
                                self.tx_wcmd
                                    .send(WriteCommand::TouchCalendar {
                                        calendar_id: calendar_id.clone(),
                                    })
                                    .expect("Failed to send calendar touch command");
                            } else {
                                tracing::debug!(
                                    "No changes detected during fsync for calendar {}",
                                    calendar_id
                                );
                            }
                        });
                    }
                    i if self.is_tasks_file(i) => {
                        let orgtask = self
                            .tasklists
                            .iter()
                            .find(|(ino, _)| ino == &i)
                            .map(|(_, tl)| tl)
                            .expect("Tasklist file not found during fsync");
                        orgtask.clear_pending();
                        orgtask.with_meta(|meta| {
                            let tasklist_id = meta.tasklist().id.as_ref().unwrap();

                            let mut did_write = false;
                            for id in removed.map().keys() {
                                tracing::info!("Removing task with id {:?}", id);
                                self.tx_wcmd
                                    .send(WriteCommand::Task {
                                        tasklist_id: tasklist_id.clone(),
                                        cmd: TaskWrite::Modify {
                                            task_id: id.to_string(),
                                            modification: TaskModify::Delete,
                                        },
                                    })
                                    .expect("Failed to send task delete command");
                                did_write = true;
                            }
                            for (from, (pred, succ)) in moves {
                                tracing::info!("Moving task {from:?} between ({pred:?}, {succ:?})");
                                self.tx_wcmd
                                    .send(WriteCommand::Task {
                                        tasklist_id: tasklist_id.clone(),
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
                                self.tx_wcmd
                                    .send(WriteCommand::Task {
                                        tasklist_id: tasklist_id.clone(),
                                        cmd: TaskWrite::Modify {
                                            task_id: id.to_string(),
                                            modification: TaskModify::Patch { task },
                                        },
                                    })
                                    .expect("Failed to send task modify command");
                                did_write = true;
                            }
                            for headline in added.fresh() {
                                let task = OrgTaskList::parse_task(headline).into();
                                tracing::info!("Adding new task: {:?}", task);
                                self.tx_wcmd
                                    .send(WriteCommand::Task {
                                        tasklist_id: tasklist_id.clone(),
                                        cmd: TaskWrite::Insert(TaskInsert::Insert { task }),
                                    })
                                    .expect("Failed to send task insert command");
                                did_write = true;
                            }

                            if did_write {
                                tracing::debug!("Updating cached Org for ino: {}", ino);
                                *org = new_org;
                                *write_time = SystemTime::now();
                                self.tx_wcmd
                                    .send(WriteCommand::TouchTasklist {
                                        tasklist_id: tasklist_id.clone(),
                                    })
                                    .expect("Failed to send tasklist touch command");
                            } else {
                                tracing::debug!(
                                    "No changes detected during fsync for tasklist {}",
                                    tasklist_id
                                );
                            }
                        });
                    }
                    _ => {}
                }
            }
            tracing::trace!(
                "fsync pending_fh: {:?}",
                self.pending_fh
                    .lock()
                    .unwrap()
                    .iter()
                    .map(|(x, InstanceState { file_handles, .. })| (x, file_handles))
                    .collect::<Vec<_>>()
            );
            reply.ok();
        } else {
            reply.error(ENOENT);
        };
    }

    fn read(
        &mut self,
        req: &Request,
        ino: Inode,
        _fh: u64,
        offset: i64,
        size: u32,
        _flags: i32,
        _lock: Option<u64>,
        reply: ReplyData,
    ) {
        if offset < 0 {
            reply.error(EINVAL);
            return;
        }
        if let Some(org) = match () {
            () if self.is_calendar_file(ino) => self
                .calendars
                .iter()
                .find(|(i, _)| &ino == i)
                .map(|(_, cal)| cal.to_org_string()),
            () if self.is_tasks_file(ino) => self
                .tasklists
                .iter()
                .find(|(i, _)| &ino == i)
                .map(|(_, tl)| tl.to_org_string()),
            () => None,
        } {
            if offset as usize >= org.len() {
                reply.data(&[]);
                return;
            }
            if let Some(InstanceState { org: cached, .. }) =
                self.pending_fh.lock().unwrap().get_mut(&(ino, req.pid()))
            {
                tracing::debug!(
                    "Fast-forwarding cached Org for ino: {}, pid: {}",
                    ino,
                    req.pid()
                );
                *cached = Org::parse(&org);
            }
            tracing::trace!(
                "read pending_fh: {:?}",
                self.pending_fh
                    .lock()
                    .unwrap()
                    .iter()
                    .map(|(x, InstanceState { file_handles, .. })| (x, file_handles))
                    .collect::<Vec<_>>()
            );
            reply.data(
                &org.as_bytes()
                    [offset as usize..usize::min(org.len(), offset as usize + size as usize)],
            );
        } else {
            reply.error(EBADF);
        }
    }

    fn readdir(
        &mut self,
        _req: &fuser::Request<'_>,
        ino: Inode,
        _fh: u64,
        offset: i64,
        mut reply: fuser::ReplyDirectory,
    ) {
        let entries =
            match ino {
                ROOT_DIR_INO => {
                    vec![
                        (ROOT_DIR_INO, FileType::Directory, ".".to_owned()),
                        (ROOT_DIR_INO, FileType::Directory, "..".to_owned()),
                        (
                            CALENDAR_DIR_INO,
                            FileType::Directory,
                            "calendars".to_owned(),
                        ),
                        (TASKS_DIR_INO, FileType::Directory, "tasks".to_owned()),
                    ]
                }
                CALENDAR_DIR_INO => {
                    let mut entries = vec![
                        (CALENDAR_DIR_INO, FileType::Directory, ".".to_owned()),
                        (ROOT_DIR_INO, FileType::Directory, "..".to_owned()),
                    ];
                    entries.extend(self.calendars.iter().enumerate().filter_map(
                        |(i, (_, cal))| {
                            cal.with_meta(|meta| {
                                meta.calendar().summary.as_ref().map(|summary| {
                                    (
                                        FILE_START_OFFSET + i as Inode,
                                        FileType::RegularFile,
                                        format!("{}.org", summary),
                                    )
                                })
                            })
                        },
                    ));
                    entries
                }
                TASKS_DIR_INO => {
                    let mut entries = vec![
                        (TASKS_DIR_INO, FileType::Directory, ".".to_owned()),
                        (ROOT_DIR_INO, FileType::Directory, "..".to_owned()),
                    ];
                    entries.extend(
                        self.tasklists
                            .iter()
                            .enumerate()
                            .filter_map(|(i, (_, tl))| {
                                tl.with_meta(|meta| {
                                    meta.tasklist().title.as_ref().map(|title| {
                                        (
                                            FILE_START_OFFSET
                                                + self.calendars.len() as Inode
                                                + i as Inode,
                                            FileType::RegularFile,
                                            format!("{}.org", title),
                                        )
                                    })
                                })
                            }),
                    );
                    entries
                }
                _ => {
                    reply.error(ENOTDIR);
                    return;
                }
            };

        for (i, entry) in entries.into_iter().enumerate().skip(offset as usize) {
            // i + 1 means the index of the next entry
            if reply.add(entry.0, (i + 1) as i64, entry.1, entry.2) {
                break;
            }
        }
        reply.ok();
    }

    fn open(&mut self, req: &Request<'_>, ino: u64, _flags: i32, reply: ReplyOpen) {
        tracing::debug!("open ino: {}, pid: {}", ino, req.pid());
        let fh = self.allocate_stateful_file_handle(ino, req.pid());
        reply.opened(fh, 0);
    }

    fn release(
        &mut self,
        req: &Request<'_>,
        ino: u64,
        fh: u64,
        _flags: i32,
        _lock_owner: Option<u64>,
        _flush: bool,
        reply: ReplyEmpty,
    ) {
        match req.pid() {
            0 => {
                // kernel context
                // clear this file handle everywhere
                self.pending_fh
                    .lock()
                    .unwrap()
                    .iter_mut()
                    .filter(|((i, _), _)| *i == ino)
                    .for_each(|(_, InstanceState { file_handles, .. })| {
                        file_handles.retain(|&x| x != fh);
                    });
            }
            pid => {
                self.pending_fh
                    .lock()
                    .unwrap()
                    .entry((ino, pid))
                    .and_modify(|InstanceState { file_handles, .. }| {
                        file_handles.retain(|&x| x != fh);
                    });
            }
        }
        self.pending_fh
            .lock()
            .unwrap()
            .retain(|_, state| !state.file_handles.is_empty());
        tracing::trace!(
            "release pending_fh: {:?}",
            self.pending_fh
                .lock()
                .unwrap()
                .iter()
                .map(|(x, InstanceState { file_handles, .. })| (x, file_handles))
                .collect::<Vec<_>>()
        );
        reply.ok();
    }
}
