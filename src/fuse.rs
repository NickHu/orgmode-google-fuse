use std::{
    ffi::OsStr,
    time::{Duration, UNIX_EPOCH},
};

use fuser::{FileAttr, FileType, Filesystem, ReplyAttr, ReplyData, ReplyEntry, Request};
use libc::{EBADF, EINVAL, ENOENT, ENOTDIR};

use crate::org::ToOrg;
use crate::org::{calendar::OrgCalendar, tasklist::OrgTaskList};

const BLKSIZE: u32 = 4096; // 4 KiB
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
pub(crate) struct OrgFS {
    pub(crate) uid: u32,
    pub(crate) gid: u32,
    pub(crate) calendars: Vec<(Inode, OrgCalendar)>,
    pub(crate) tasklists: Vec<(Inode, OrgTaskList)>,
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
    pub(crate) fn new(calendars: Vec<OrgCalendar>, tasklists: Vec<OrgTaskList>) -> Self {
        let csl = calendars.len();
        Self {
            uid: nix::unistd::getuid().as_raw(),
            gid: nix::unistd::getgid().as_raw(),
            calendars: calendars
                .into_iter()
                .enumerate()
                .map(|(i, cal)| (FILE_START_OFFSET + i as u64, cal))
                .collect(),
            tasklists: tasklists
                .into_iter()
                .enumerate()
                .map(|(i, tl)| (FILE_START_OFFSET + csl as u64 + i as u64, tl))
                .collect(),
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
                        .map(|_| file_attr(self.uid, self.gid, *ino, cal.to_org().len() as u64))
                })
            }),
            TASKS_DIR_INO => name.to_str().and_then(|filename| {
                self.tasklists.iter().find_map(|(ino, tl)| {
                    tl.tasklist()
                        .as_ref()
                        .title
                        .as_ref()
                        .filter(|title| format!("{}.org", title) == filename)
                        .map(|_| file_attr(self.uid, self.gid, *ino, tl.to_org().len() as u64))
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
                .map(|(_, cal)| file_attr(self.uid, self.gid, i, cal.to_org().len() as u64)),
            i if self.is_tasks_file(i) => self
                .tasklists
                .iter()
                .find(|(ino, _)| ino == &i)
                .map(|(_, tl)| file_attr(self.uid, self.gid, i, tl.to_org().len() as u64)),

            _ => None,
        } {
            reply.attr(&TTL, &fileattr);
        } else {
            reply.error(ENOENT);
        }
    }

    fn read(
        &mut self,
        _req: &Request,
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
                .map(|(_, cal)| cal.to_org()),
            () if self.is_tasks_file(ino) => self
                .tasklists
                .iter()
                .find(|(i, _)| &ino == i)
                .map(|(_, tl)| tl.to_org()),
            () => None,
        } {
            if offset as usize >= org.len() {
                reply.data(&[]);
                return;
            }
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
}
