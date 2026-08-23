use std::mem::size_of;
use std::os::fd::{AsFd, AsRawFd, BorrowedFd, FromRawFd, OwnedFd};
use std::os::unix::ffi::OsStrExt;
use std::path::{Path, PathBuf};

use calloop::generic::Generic;
use calloop::{Interest, LoopHandle, Mode, PostAction};

use crate::MuralApp;

pub(crate) fn insert_library_watcher(
    loop_handle: &LoopHandle<'_, MuralApp>,
    wall_dir: &Path,
) -> Result<(), String> {
    match LibraryWatcher::new(wall_dir) {
        Ok(watcher) => {
            loop_handle
                .insert_source(
                    Generic::new(watcher, Interest::READ, Mode::Level),
                    |readiness, watcher, app| {
                        trace_log!(
                            app.trace,
                            "library watcher readiness readable={} writable={} error={}",
                            readiness.readable,
                            readiness.writable,
                            readiness.error
                        );
                        match watcher.drain_events() {
                            Ok(paths) => {
                                trace_log!(
                                    app.trace,
                                    "library watcher drained {} path event(s)",
                                    paths.len()
                                );
                                for path in paths {
                                    match app.wallpaper.add_top_level_file(&path) {
                                        Ok(true) => {
                                            eprintln!("murald: added wallpaper {}", path.display());
                                        }
                                        Ok(false) => {}
                                        Err(error) => {
                                            eprintln!(
                                                "murald: failed to add wallpaper {}: {error}",
                                                path.display()
                                            );
                                        }
                                    }
                                }
                            }
                            Err(error) => {
                                eprintln!(
                                    "murald: failed to read wallpaper directory events: {error}"
                                );
                            }
                        }
                        Ok(PostAction::Continue)
                    },
                )
                .map_err(|error| {
                    format!("failed to insert wallpaper directory watcher event source: {error}")
                })?;
        }
        Err(error) => {
            eprintln!("murald: wallpaper directory watcher disabled: {error}");
        }
    }
    Ok(())
}

#[derive(Debug)]
pub(crate) struct LibraryWatcher {
    fd: OwnedFd,
    wall_dir: PathBuf,
}

impl LibraryWatcher {
    pub(crate) fn new(wall_dir: &Path) -> Result<Self, String> {
        if !wall_dir.is_dir() {
            return Err(format!("{} is not a directory", wall_dir.display()));
        }
        let fd = unsafe { libc::inotify_init1(libc::IN_NONBLOCK | libc::IN_CLOEXEC) };
        if fd < 0 {
            return Err(format!(
                "inotify_init1 failed: {}",
                std::io::Error::last_os_error()
            ));
        }
        let fd = unsafe { OwnedFd::from_raw_fd(fd) };
        let c_path = std::ffi::CString::new(wall_dir.as_os_str().as_bytes()).map_err(|_| {
            format!(
                "wallpaper directory contains NUL byte: {}",
                wall_dir.display()
            )
        })?;
        let mask = libc::IN_CLOSE_WRITE | libc::IN_MOVED_TO;
        let watch = unsafe { libc::inotify_add_watch(fd.as_raw_fd(), c_path.as_ptr(), mask) };
        if watch < 0 {
            return Err(format!(
                "inotify_add_watch {} failed: {}",
                wall_dir.display(),
                std::io::Error::last_os_error()
            ));
        }
        Ok(Self {
            fd,
            wall_dir: wall_dir.to_owned(),
        })
    }

    pub(crate) fn drain_events(&self) -> std::io::Result<Vec<PathBuf>> {
        let mut paths = Vec::new();
        let mut buffer = [0_u8; 8192];
        loop {
            let bytes_read = unsafe {
                libc::read(
                    self.fd.as_raw_fd(),
                    buffer.as_mut_ptr().cast::<libc::c_void>(),
                    buffer.len(),
                )
            };
            if bytes_read < 0 {
                let error = std::io::Error::last_os_error();
                if matches!(
                    error.kind(),
                    std::io::ErrorKind::WouldBlock | std::io::ErrorKind::Interrupted
                ) {
                    return Ok(paths);
                }
                return Err(error);
            }
            if bytes_read == 0 {
                return Ok(paths);
            }
            let mut offset = 0_usize;
            let bytes_read = usize::try_from(bytes_read).unwrap_or(0);
            while offset + size_of::<libc::inotify_event>() <= bytes_read {
                let event = unsafe {
                    buffer
                        .as_ptr()
                        .add(offset)
                        .cast::<libc::inotify_event>()
                        .read_unaligned()
                };
                offset += size_of::<libc::inotify_event>();
                let name_len = usize::try_from(event.len).unwrap_or(0);
                if offset + name_len > bytes_read {
                    break;
                }
                let name_bytes = &buffer[offset..offset + name_len];
                offset += name_len;
                if event.mask & (libc::IN_CLOSE_WRITE | libc::IN_MOVED_TO) == 0 {
                    continue;
                }
                let end = name_bytes
                    .iter()
                    .position(|byte| *byte == 0)
                    .unwrap_or(name_bytes.len());
                if end == 0 {
                    continue;
                }
                let name = std::ffi::OsStr::from_bytes(&name_bytes[..end]);
                paths.push(self.wall_dir.join(name));
            }
        }
    }
}

impl AsFd for LibraryWatcher {
    fn as_fd(&self) -> BorrowedFd<'_> {
        self.fd.as_fd()
    }
}
