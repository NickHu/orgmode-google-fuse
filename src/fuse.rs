use std::{
    collections::HashMap,
    ffi::OsStr,
    sync::{Arc, Mutex},
    time::{Duration, UNIX_EPOCH},
};

use fuser::{
    FileAttr, FileType, Filesystem, ReplyAttr, ReplyData, ReplyEmpty, ReplyEntry, ReplyOpen,
    Request,
};
use itertools::Itertools;
use libc::{EBADF, EINVAL, ENOENT, ENOTDIR};
use orgize::Org;

use crate::org::{calendar::OrgCalendar, tasklist::OrgTaskList};
use crate::{org::ToOrg, Pid};

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
pub(crate) struct OrgFS {
    pub(crate) uid: u32,
    pub(crate) gid: u32,
    pub(crate) calendars: Vec<(Inode, OrgCalendar)>,
    pub(crate) tasklists: Vec<(Inode, OrgTaskList)>,
    tx: tokio::sync::mpsc::UnboundedSender<Pid>,
    #[allow(clippy::type_complexity)]
    pending_fh: Arc<Mutex<HashMap<(Inode, Pid), (Vec<FileHandle>, Org<'static>)>>>,
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

const fn file_attr(uid: u32, gid: u32, ino: Inode, size: u64) -> FileAttr {
    let blocks = size.div_ceil(BLKSIZE as u64);
    FileAttr {
        ino,
        size,
        blocks,
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
        tx: tokio::sync::mpsc::UnboundedSender<Pid>,
        pending_fh: Arc<Mutex<HashMap<(Inode, Pid), (Vec<FileHandle>, Org<'static>)>>>,
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
            tx,
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
        // opens a file. Subsequent reads will return the same data from the snapshot, and writes
        // are reconciled against this snapshot.
        if let Some(org) = match ino {
            i if self.is_calendar_file(i) => self
                .calendars
                .iter()
                .find(|(ino, _)| ino == &i)
                .map(|(_, cal)| cal.clone().to_org()),
            i if self.is_tasks_file(i) => self
                .tasklists
                .iter()
                .find(|(ino, _)| ino == &i)
                .map(|(_, tl)| tl.to_org()),
            _ => None,
        } {
            let mut guard = self.pending_fh.lock().unwrap();
            if guard.keys().all(|(_, p)| *p != pid) {
                // newly opened file, watch the pid
                self.tx.send(pid).unwrap();
            }
            let fh = guard
                .values()
                .flat_map(|(fhs, _)| fhs)
                .sorted()
                .copied()
                .enumerate()
                .find(|(i, fh)| i + 1 != *fh as usize)
                .map(|(_, fh)| fh)
                .unwrap_or(1);
            guard
                .entry((ino, pid))
                .or_insert((Vec::default(), org))
                .0
                .push(fh);
            fh
        } else {
            0
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
                    cal.calendar()
                        .as_ref()
                        .summary
                        .as_ref()
                        .filter(|summary| format!("{}.org", summary) == filename)
                        .map(|_| {
                            file_attr(self.uid, self.gid, *ino, cal.to_org_string().len() as u64)
                        })
                })
            }),
            TASKS_DIR_INO => name.to_str().and_then(|filename| {
                self.tasklists.iter().find_map(|(ino, tl)| {
                    tl.tasklist()
                        .as_ref()
                        .title
                        .as_ref()
                        .filter(|title| format!("{}.org", title) == filename)
                        .map(|_| {
                            file_attr(self.uid, self.gid, *ino, tl.to_org_string().len() as u64)
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

    fn getattr(&mut self, _req: &Request, ino: Inode, _fh: Option<u64>, reply: ReplyAttr) {
        if let Some(fileattr) = match ino {
            ROOT_DIR_INO => Some(root_dir_attr(self.uid, self.gid)),
            CALENDAR_DIR_INO => Some(calendar_dir_attr(self.uid, self.gid)),
            TASKS_DIR_INO => Some(tasks_dir_attr(self.uid, self.gid)),
            i if self.is_calendar_file(i) => self
                .calendars
                .iter()
                .find(|(ino, _)| ino == &i)
                .map(|(_, cal)| file_attr(self.uid, self.gid, i, cal.to_org_string().len() as u64)),
            i if self.is_tasks_file(i) => self
                .tasklists
                .iter()
                .find(|(ino, _)| ino == &i)
                .map(|(_, tl)| file_attr(self.uid, self.gid, i, tl.to_org_string().len() as u64)),
            _ => None,
        } {
            reply.attr(&TTL, &fileattr);
        } else {
            reply.error(ENOENT);
        }
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
        if let Some((_fhs, org)) = self.pending_fh.lock().unwrap().get(&(ino, req.pid())) {
            let org_str = org.to_org_string();
            if offset as usize >= org_str.len() {
                reply.data(&[]);
                return;
            }
            reply.data(
                &org_str.as_bytes()
                    [offset as usize..usize::min(org_str.len(), offset as usize + size as usize)],
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
        let entries = match ino {
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
                entries.extend(
                    self.calendars
                        .iter()
                        .enumerate()
                        .filter_map(|(i, (_, cal))| {
                            cal.calendar().as_ref().summary.as_ref().map(|summary| {
                                (
                                    FILE_START_OFFSET + i as Inode,
                                    FileType::RegularFile,
                                    format!("{}.org", summary),
                                )
                            })
                        }),
                );
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
                            tl.tasklist().as_ref().title.as_ref().map(|title| {
                                (
                                    FILE_START_OFFSET + self.calendars.len() as Inode + i as Inode,
                                    FileType::RegularFile,
                                    format!("{}.org", title),
                                )
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
        self.pending_fh
            .lock()
            .unwrap()
            .entry((ino, req.pid()))
            .and_modify(|(fhs, _)| {
                fhs.retain(|&x| x != fh);
            });
        reply.ok();
    }
}
