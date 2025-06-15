use std::{
    ffi::OsStr,
    time::{Duration, UNIX_EPOCH},
};

use fuser::{FileAttr, FileType, Filesystem, ReplyAttr, ReplyData, ReplyEntry, Request};
use libc::ENOENT;

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

const TTL: Duration = Duration::from_secs(1);

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
    let blocks = (size + BLKSIZE as u64 - 1) / BLKSIZE as u64;
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
        match parent {
            p if p == ROOT_DIR_INO => match name.to_str() {
                Some("calendars") => {
                    reply.entry(&TTL, &calendar_dir_attr(self.uid, self.gid), 0);
                }
                Some("tasks") => {
                    reply.entry(&TTL, &tasks_dir_attr(self.uid, self.gid), 0);
                }
                _ => {
                    reply.error(ENOENT);
                }
            },
            p if p == CALENDAR_DIR_INO => match name.to_str() {
                Some(file) => {
                    if let Some(i) = self.calendars.iter().find(|x| {
                        x.1 .0.summary.as_ref().map(|f| format!("{}.org", f))
                            == Some(file.to_owned())
                    }) {
                        reply.entry(
                            &TTL,
                            &file_attr(self.uid, self.gid, i.0, i.1.to_org().len() as u64),
                            0,
                        );
                    } else {
                        reply.error(ENOENT);
                    }
                }
                _ => reply.error(ENOENT),
            },
            p if p == TASKS_DIR_INO => match name.to_str() {
                Some(file) => {
                    if let Some(i) = self.tasklists.iter().find(|x| {
                        x.1 .0.title.as_ref().map(|f| format!("{}.org", f)) == Some(file.to_owned())
                    }) {
                        reply.entry(
                            &TTL,
                            &file_attr(self.uid, self.gid, i.0, i.1.to_org().len() as u64),
                            0,
                        );
                    } else {
                        reply.error(ENOENT);
                    }
                }
                _ => {
                    reply.error(ENOENT);
                }
            },
            _ => reply.error(ENOENT),
        }
    }

    fn getattr(&mut self, _req: &Request, ino: Inode, _fh: Option<u64>, reply: ReplyAttr) {
        match ino {
            i if i == ROOT_DIR_INO => reply.attr(&TTL, &root_dir_attr(self.uid, self.gid)),
            i if i == CALENDAR_DIR_INO => reply.attr(&TTL, &calendar_dir_attr(self.uid, self.gid)),
            i if i == TASKS_DIR_INO => reply.attr(&TTL, &tasks_dir_attr(self.uid, self.gid)),
            i if self.is_calendar_file(i) => {
                if let Some((_, cal)) = self.calendars.iter().find(|(ino, _)| ino == &i) {
                    reply.attr(
                        &TTL,
                        &file_attr(self.uid, self.gid, i, cal.to_org().len() as u64),
                    )
                } else {
                    reply.error(ENOENT);
                }
            }
            i if self.is_tasks_file(i) => {
                if let Some((_, tl)) = self.tasklists.iter().find(|(ino, _)| ino == &i) {
                    reply.attr(
                        &TTL,
                        &file_attr(self.uid, self.gid, i, tl.to_org().len() as u64),
                    )
                } else {
                    reply.error(ENOENT);
                }
            }
            _ => reply.error(ENOENT),
        }
    }

    fn read(
        &mut self,
        _req: &Request,
        ino: Inode,
        _fh: u64,
        offset: i64,
        _size: u32,
        _flags: i32,
        _lock: Option<u64>,
        reply: ReplyData,
    ) {
        tracing::debug!("read: ino: {}, offset: {}", ino, offset);
        match () {
            () if self.is_calendar_file(ino) => {
                if let Some((_, cal)) = self.calendars.iter().find(|(i, _)| &ino == i) {
                    let org = cal.to_org();
                    let len = org.len() as i64;
                    if offset >= len {
                        reply.error(ENOENT);
                        return;
                    }
                    reply.data(&org.as_bytes()[offset as usize..]);
                } else {
                    reply.error(ENOENT);
                }
            }
            () if self.is_tasks_file(ino) => {
                if let Some((_, tl)) = self.tasklists.iter().find(|(i, _)| &ino == i) {
                    let org = tl.to_org();
                    let len = org.len() as i64;
                    if offset >= len {
                        reply.error(ENOENT);
                        return;
                    }
                    reply.data(&org.as_bytes()[offset as usize..]);
                } else {
                    reply.error(ENOENT);
                }
            }
            () => {
                reply.error(ENOENT);
            }
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
        match ino {
            i if i == ROOT_DIR_INO => {
                let entries = [
                    (ROOT_DIR_INO, "."),
                    (ROOT_DIR_INO, ".."),
                    (CALENDAR_DIR_INO, "calendars"),
                    (TASKS_DIR_INO, "tasks"),
                ];
                for (i, entry) in entries.into_iter().enumerate().skip(offset as usize) {
                    // i + 1 means the index of the next entry
                    if reply.add(entry.0, (i + 1) as i64, FileType::Directory, entry.1) {
                        break;
                    }
                }
                reply.ok();
            }
            i if i == CALENDAR_DIR_INO => {
                let mut entries = vec![
                    (CALENDAR_DIR_INO, FileType::Directory, ".".to_owned()),
                    (ROOT_DIR_INO, FileType::Directory, "..".to_owned()),
                ];
                for (i, calendar) in self.calendars.iter().enumerate() {
                    if let Some(summary) = calendar.1 .0.summary.as_ref() {
                        entries.push((
                            FILE_START_OFFSET + i as Inode,
                            FileType::RegularFile,
                            format!("{}.org", summary),
                        ));
                    }
                }
                for (i, entry) in entries.into_iter().enumerate().skip(offset as usize) {
                    // i + 1 means the index of the next entry
                    if reply.add(entry.0, (i + 1) as i64, entry.1, entry.2) {
                        break;
                    }
                }
                reply.ok();
            }
            i if i == TASKS_DIR_INO => {
                let mut entries = vec![
                    (TASKS_DIR_INO, FileType::Directory, ".".to_owned()),
                    (ROOT_DIR_INO, FileType::Directory, "..".to_owned()),
                ];
                for (i, tasklist) in self.tasklists.iter().enumerate() {
                    if let Some(title) = tasklist.1 .0.title.as_ref() {
                        entries.push((
                            FILE_START_OFFSET + self.calendars.len() as Inode + i as Inode,
                            FileType::RegularFile,
                            format!("{}.org", title),
                        ));
                    }
                }
                for (i, entry) in entries.into_iter().enumerate().skip(offset as usize) {
                    // i + 1 means the index of the next entry
                    if reply.add(entry.0, (i + 1) as i64, entry.1, entry.2) {
                        break;
                    }
                }
                reply.ok();
            }
            _ => reply.error(ENOENT),
        }
    }
}
