/*
 * Copyright (C) 2021-2022 The Aero Project Developers.
 *
 * This file is part of The Aero Project.
 *
 * Aero is free software: you can redistribute it and/or modify
 * it under the terms of the GNU General Public License as published by
 * the Free Software Foundation, either version 3 of the License, or
 * (at your option) any later version.
 *
 * Aero is distributed in the hope that it will be useful,
 * but WITHOUT ANY WARRANTY; without even the implied warranty of
 * MERCHANTABILITY or FITNESS FOR A PARTICULAR PURPOSE. See the
 * GNU General Public License for more details.
 *
 * You should have received a copy of the GNU General Public License
 * along with Aero. If not, see <https://www.gnu.org/licenses/>.
 */

use aero_syscall::SocketAddrUnix;

use aero_syscall::prelude::EPollEventFlags;
use alloc::string::String;
use alloc::sync::{Arc, Weak};
use alloc::vec::Vec;
use spin::RwLock;

use crate::fs;
use crate::fs::inode::{DirEntry, FileType, INodeInterface, Metadata, PollTable};
use crate::fs::{FileSystemError, Path, Result};
use crate::utils::sync::BlockQueue;

use super::SocketAddr;

fn path_from_unix_sock<'sock>(address: &'sock SocketAddrUnix) -> Result<&'sock Path> {
    // The abstract namespace socket allows the creation of a socket
    // connection which does not require a path to be created.
    let abstrat_namespaced = address.path[0] == 0;
    assert!(!abstrat_namespaced);

    let path_len = address
        .path
        .iter()
        .position(|&c| c == 0)
        .unwrap_or(address.path.len());

    let path_str = core::str::from_utf8(&address.path[..path_len])
        .ok()
        .ok_or(FileSystemError::InvalidPath)?;

    Ok(Path::new(path_str))
}

#[derive(Default)]
struct UnixSocketBacklog {
    backlog: Option<Vec<Arc<UnixSocket>>>,
}

impl UnixSocketBacklog {
    pub fn push(&mut self, socket: Arc<UnixSocket>) {
        if let Some(ref mut backlog) = self.backlog {
            assert!(backlog.len() != backlog.capacity());
            backlog.push(socket);
        }
    }

    pub fn len(&self) -> usize {
        self.backlog.as_ref().map(|e| e.len()).unwrap_or(0)
    }

    pub fn update_capacity(&mut self, capacity: usize) {
        assert!(
            self.backlog.is_none(),
            "UnixSocket::listen() has already been called"
        );

        self.backlog = Some(Vec::with_capacity(capacity));
    }
}

#[derive(Default)]
struct UnixSocketInner {
    backlog: UnixSocketBacklog,
    listening: bool,
}

pub struct UnixSocket {
    inner: RwLock<UnixSocketInner>,
    wq: BlockQueue,
    weak: Weak<UnixSocket>,
}

impl UnixSocket {
    pub fn new() -> Arc<UnixSocket> {
        Arc::new_cyclic(|weak| UnixSocket {
            inner: RwLock::new(UnixSocketInner::default()),
            wq: BlockQueue::new(),
            weak: weak.clone(),
        })
    }

    pub fn sref(&self) -> Arc<UnixSocket> {
        self.weak.upgrade().unwrap()
    }
}

impl INodeInterface for UnixSocket {
    fn metadata(&self) -> Result<Metadata> {
        Ok(Metadata {
            id: 0,
            file_type: FileType::Socket,
            size: 0,
            children_len: 0,
        })
    }

    fn bind(&self, address: SocketAddr, _length: usize) -> Result<()> {
        let address = address.as_unix().ok_or(FileSystemError::NotSupported)?;
        let path = path_from_unix_sock(address)?;

        // ensure that the provided path is not already in use.
        if fs::lookup_path(path).is_ok() {
            return Err(FileSystemError::EntryExists);
        }

        let (parent, name) = path.parent_and_basename();

        // create the socket inode.
        DirEntry::from_socket_inode(fs::lookup_path(parent)?, String::from(name), self.sref())?;

        Ok(())
    }

    fn connect(&self, address: SocketAddr, _length: usize) -> Result<()> {
        let address = address.as_unix().ok_or(FileSystemError::NotSupported)?;
        let path = path_from_unix_sock(address)?;
        let socket = fs::lookup_path(path)?;

        let target = socket
            .inode()
            .as_unix_socket()?
            .downcast_arc::<UnixSocket>()
            .ok_or(FileSystemError::NotSocket)?; // NOTE: the provided socket was not a unix socket.

        let mut target = target.inner.write();

        // ensure that the target socket is listening for new connections.
        if !target.listening {
            return Err(FileSystemError::ConnectionRefused);
        }

        target.backlog.push(self.sref());
        self.wq.notify_complete();

        Ok(())
    }

    fn listen(&self, backlog: usize) -> Result<()> {
        let mut this = self.inner.write();

        this.backlog.update_capacity(backlog);
        this.listening = true;

        Ok(())
    }

    fn poll(&self, table: Option<&mut PollTable>) -> Result<EPollEventFlags> {
        log::warn!("UnixSocket::poll() is a stub");

        table.map(|e| e.insert(&self.wq));

        let mut events = EPollEventFlags::default();

        if self.inner.read().backlog.len() > 0 {
            events.insert(EPollEventFlags::OUT);
        }

        Ok(events)
    }
}
