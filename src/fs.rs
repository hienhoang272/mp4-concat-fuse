//! Read-only FUSE filesystem exposing one or more virtual concatenated MP4s.

use crate::concat::{Layout, Piece};
use fuser::{
    Errno, FileAttr, FileHandle, FileType, Filesystem, FopenFlags, Generation, INodeNo, LockOwner,
    OpenFlags, ReplyAttr, ReplyData, ReplyDirectory, ReplyEntry, ReplyOpen, Request,
};
use std::ffi::OsStr;
use std::fs::File;
use std::io;
use std::os::unix::fs::FileExt;
use std::time::{Duration, SystemTime};

pub struct VirtualFile {
    pub name: String,
    pub layout: Layout,
    pub files: Vec<File>,
    pub mtime: SystemTime,
}

impl VirtualFile {
    pub fn new(name: String, layout: Layout) -> io::Result<Self> {
        let files = layout
            .sources
            .iter()
            .map(|p| {
                File::open(p).map_err(|e| {
                    let hint = if e.raw_os_error() == Some(libc::EMFILE) {
                        " (too many open files: raise `ulimit -n`)"
                    } else {
                        ""
                    };
                    io::Error::new(e.kind(), format!("{}: {e}{hint}", p.display()))
                })
            })
            .collect::<io::Result<Vec<_>>>()?;
        let mtime = files[0].metadata()?.modified().unwrap_or(SystemTime::UNIX_EPOCH);
        Ok(VirtualFile { name, layout, files, mtime })
    }

    /// pread() on the virtual file.
    pub fn read_at(&self, buf: &mut [u8], mut off: u64) -> io::Result<usize> {
        let l = &self.layout;
        let mut done = 0;
        let Some(mut idx) = l.locate(off) else { return Ok(0) };
        while done < buf.len() && idx < l.extents.len() {
            let e = &l.extents[idx];
            let within = off - e.vstart;
            let n = ((e.len - within) as usize).min(buf.len() - done);
            let dst = &mut buf[done..done + n];
            match e.piece {
                Piece::Header { start } => {
                    let s = (start + within) as usize;
                    dst.copy_from_slice(&l.header[s..s + n]);
                }
                Piece::Source { src, offset } => {
                    self.files[src].read_exact_at(dst, offset + within)?;
                }
            }
            done += n;
            off += n as u64;
            idx += 1;
        }
        Ok(done)
    }
}

pub struct ConcatFs {
    pub files: Vec<VirtualFile>,
    uid: u32,
    gid: u32,
    started: SystemTime,
}

const TTL: Duration = Duration::from_secs(3600);

impl ConcatFs {
    pub fn new(files: Vec<VirtualFile>) -> Self {
        ConcatFs {
            files,
            uid: unsafe { libc::getuid() },
            gid: unsafe { libc::getgid() },
            started: SystemTime::now(),
        }
    }

    fn attr(&self, ino: u64) -> Option<FileAttr> {
        let (kind, size, perm, nlink, mtime) = if ino == 1 {
            (FileType::Directory, 0, 0o555, 2, self.started)
        } else {
            let f = self.files.get((ino - 2) as usize)?;
            (FileType::RegularFile, f.layout.size, 0o444, 1, f.mtime)
        };
        Some(FileAttr {
            ino: INodeNo(ino),
            size,
            blocks: size.div_ceil(512),
            atime: mtime,
            mtime,
            ctime: mtime,
            crtime: mtime,
            kind,
            perm,
            nlink,
            uid: self.uid,
            gid: self.gid,
            rdev: 0,
            blksize: 1 << 20,
            flags: 0,
        })
    }
}

impl Filesystem for ConcatFs {
    fn lookup(&self, _req: &Request, parent: INodeNo, name: &OsStr, reply: ReplyEntry) {
        if parent.0 != 1 {
            return reply.error(Errno::ENOENT);
        }
        match self.files.iter().position(|f| OsStr::new(&f.name) == name) {
            Some(i) => reply.entry(&TTL, &self.attr(i as u64 + 2).unwrap(), Generation(0)),
            None => reply.error(Errno::ENOENT),
        }
    }

    fn getattr(&self, _req: &Request, ino: INodeNo, _fh: Option<FileHandle>, reply: ReplyAttr) {
        match self.attr(ino.0) {
            Some(a) => reply.attr(&TTL, &a),
            None => reply.error(Errno::ENOENT),
        }
    }

    fn open(&self, _req: &Request, ino: INodeNo, flags: OpenFlags, reply: ReplyOpen) {
        if ino.0 < 2 || self.files.get((ino.0 - 2) as usize).is_none() {
            return reply.error(Errno::ENOENT);
        }
        if flags.0 & libc::O_ACCMODE != libc::O_RDONLY {
            return reply.error(Errno::EROFS);
        }
        reply.opened(FileHandle(0), FopenFlags::FOPEN_KEEP_CACHE);
    }

    fn read(
        &self,
        _req: &Request,
        ino: INodeNo,
        _fh: FileHandle,
        offset: u64,
        size: u32,
        _flags: OpenFlags,
        _lock: Option<LockOwner>,
        reply: ReplyData,
    ) {
        let Some(f) = ino.0.checked_sub(2).and_then(|i| self.files.get(i as usize)) else {
            return reply.error(Errno::ENOENT);
        };
        let len = (size as u64).min(f.layout.size.saturating_sub(offset)) as usize;
        let mut buf = vec![0u8; len];
        match f.read_at(&mut buf, offset) {
            Ok(n) => reply.data(&buf[..n]),
            Err(e) => {
                eprintln!("read error at {offset}: {e}");
                reply.error(Errno::EIO)
            }
        }
    }

    fn readdir(&self, _req: &Request, ino: INodeNo, _fh: FileHandle, offset: u64, mut reply: ReplyDirectory) {
        if ino.0 != 1 {
            return reply.error(Errno::ENOTDIR);
        }
        let mut entries: Vec<(u64, FileType, &str)> =
            vec![(1, FileType::Directory, "."), (1, FileType::Directory, "..")];
        for (i, f) in self.files.iter().enumerate() {
            entries.push((i as u64 + 2, FileType::RegularFile, &f.name));
        }
        for (i, (ino, kind, name)) in entries.into_iter().enumerate().skip(offset as usize) {
            if reply.add(INodeNo(ino), i as u64 + 1, kind, name) {
                break;
            }
        }
        reply.ok();
    }
}
