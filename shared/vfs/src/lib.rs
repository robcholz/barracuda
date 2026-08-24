#![no_std]

//! Asynchronous virtual filesystem contracts and mount namespaces.

extern crate alloc;

mod backend;
mod error;
mod global;
mod path;
mod types;
mod vfs;

pub use backend::{Backend, BackendFile, BackendFuture, File, VfsBackend};
pub use embedded_io::SeekFrom;
pub use error::FsError;
pub use global::{
    create, create_dir_all, metadata, mount, mount_scoped, open, open_with, read, read_dir,
    remove_dir, remove_file, rename, unmount, write,
};
pub use types::{DirEntry, FileType, Metadata, MountOptions, OpenOptions, ReadDir};
pub use vfs::Vfs;
