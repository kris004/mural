use std::env;
use std::ffi::CString;
use std::ffi::OsString;
use std::mem;
use std::os::fd::{AsRawFd, FromRawFd, OwnedFd};
use std::os::unix::ffi::{OsStrExt, OsStringExt};
use std::path::{Path, PathBuf};
use std::time::Duration;

#[derive(Clone, Debug)]
pub(crate) struct SystemdNotify {
    socket: Option<NotifySocket>,
}

impl SystemdNotify {
    pub(crate) const fn disabled() -> Self {
        Self { socket: None }
    }

    pub(crate) fn from_env() -> Self {
        let socket = env::var_os("NOTIFY_SOCKET").and_then(|value| {
            if value.is_empty() {
                None
            } else {
                Some(NotifySocket::from_raw(value.as_bytes()))
            }
        });
        Self { socket }
    }

    pub(crate) fn ready(&self, status: &str) {
        self.notify(&format!("READY=1\nSTATUS={status}"));
    }

    pub(crate) fn stopping(&self) {
        self.notify("STOPPING=1\nSTATUS=stopping");
    }

    pub(crate) fn watchdog(&self) {
        self.notify("WATCHDOG=1");
    }

    pub(crate) fn watchdog_interval() -> Option<Duration> {
        env::var_os("WATCHDOG_USEC")
            .and_then(|value| value.into_string().ok())
            .and_then(|value| value.parse::<u64>().ok())
            .and_then(watchdog_interval_from_usec)
    }

    fn notify(&self, message: &str) {
        let Some(socket) = &self.socket else {
            return;
        };
        if let Err(error) = socket.send(message.as_bytes()) {
            eprintln!("murald: sd_notify failed: {error}");
        }
    }
}

fn watchdog_interval_from_usec(usec: u64) -> Option<Duration> {
    if usec == 0 {
        None
    } else {
        Some(Duration::from_micros(usec / 2).max(Duration::from_secs(1)))
    }
}

#[derive(Clone, Debug)]
enum NotifySocket {
    Path(PathBuf),
    Abstract(Vec<u8>),
}

impl NotifySocket {
    fn from_raw(raw: &[u8]) -> Self {
        if raw.first() == Some(&b'@') {
            Self::Abstract(raw[1..].to_vec())
        } else {
            Self::Path(PathBuf::from(OsString::from_vec(raw.to_vec())))
        }
    }

    fn send(&self, message: &[u8]) -> Result<(), String> {
        match self {
            Self::Path(path) => send_to_path(path, message),
            Self::Abstract(name) => send_to_abstract(name, message),
        }
    }
}

fn send_to_path(path: &Path, message: &[u8]) -> Result<(), String> {
    let path = CString::new(path.as_os_str().as_bytes())
        .map_err(|_| format!("NOTIFY_SOCKET path contains NUL: {}", path.display()))?;
    let mut addr = zero_sockaddr_un();
    addr.sun_family = libc::AF_UNIX
        .try_into()
        .map_err(|_| "AF_UNIX does not fit sockaddr family".to_owned())?;
    write_sun_path(&mut addr, path.as_bytes_with_nul())?;
    let len = sockaddr_un_len(path.as_bytes_with_nul().len())?;
    send_to_addr(&addr, len, message)
}

fn send_to_abstract(name: &[u8], message: &[u8]) -> Result<(), String> {
    let mut addr = zero_sockaddr_un();
    addr.sun_family = libc::AF_UNIX
        .try_into()
        .map_err(|_| "AF_UNIX does not fit sockaddr family".to_owned())?;
    let mut path = Vec::with_capacity(name.len() + 1);
    path.push(0);
    path.extend_from_slice(name);
    write_sun_path(&mut addr, &path)?;
    let len = sockaddr_un_len(path.len())?;
    send_to_addr(&addr, len, message)
}

fn send_to_addr(
    addr: &libc::sockaddr_un,
    len: libc::socklen_t,
    message: &[u8],
) -> Result<(), String> {
    let fd = unsafe { libc::socket(libc::AF_UNIX, libc::SOCK_DGRAM | libc::SOCK_CLOEXEC, 0) };
    if fd < 0 {
        return Err(format!(
            "socket(AF_UNIX, SOCK_DGRAM) failed: {}",
            std::io::Error::last_os_error()
        ));
    }
    let fd = unsafe { OwnedFd::from_raw_fd(fd) };
    let written = unsafe {
        libc::sendto(
            fd.as_raw_fd(),
            message.as_ptr().cast::<libc::c_void>(),
            message.len(),
            libc::MSG_NOSIGNAL,
            std::ptr::from_ref(addr).cast::<libc::sockaddr>(),
            len,
        )
    };
    if written < 0 {
        return Err(format!(
            "sendto(NOTIFY_SOCKET) failed: {}",
            std::io::Error::last_os_error()
        ));
    }
    Ok(())
}

fn zero_sockaddr_un() -> libc::sockaddr_un {
    unsafe { mem::zeroed() }
}

fn write_sun_path(addr: &mut libc::sockaddr_un, path: &[u8]) -> Result<(), String> {
    if path.len() > addr.sun_path.len() {
        return Err("NOTIFY_SOCKET path is too long".to_owned());
    }
    let target = unsafe {
        std::slice::from_raw_parts_mut(addr.sun_path.as_mut_ptr().cast::<u8>(), addr.sun_path.len())
    };
    target[..path.len()].copy_from_slice(path);
    Ok(())
}

fn sockaddr_un_len(path_len: usize) -> Result<libc::socklen_t, String> {
    let len = mem::size_of::<libc::sa_family_t>() + path_len;
    len.try_into()
        .map_err(|_| "sockaddr_un length does not fit socklen_t".to_owned())
}

#[cfg(test)]
mod tests {
    use std::fs;
    use std::os::unix::net::UnixDatagram;
    use std::time::{Duration, SystemTime, UNIX_EPOCH};

    use super::*;

    #[test]
    fn sends_notify_message_to_path_socket() {
        // Pathname-backed Unix sockets have a short platform limit. Keep this
        // fixture independent of a potentially long inherited TMPDIR.
        let socket_path = std::path::Path::new("/tmp").join(format!(
            "mural-notify-test-{}-{}.sock",
            std::process::id(),
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .expect("system clock is after Unix epoch")
                .as_nanos()
        ));
        let listener = match UnixDatagram::bind(&socket_path) {
            Ok(listener) => listener,
            Err(error) if error.kind() == std::io::ErrorKind::PermissionDenied => return,
            Err(error) => panic!("{error}"),
        };
        listener
            .set_read_timeout(Some(Duration::from_secs(2)))
            .unwrap();

        if let Err(error) = NotifySocket::Path(socket_path.clone()).send(b"READY=1\nSTATUS=ready") {
            // Some sandboxes deny AF_UNIX datagram sends; the live service
            // path covers the integration behavior.
            if error.contains("Operation not permitted") {
                fs::remove_file(&socket_path).expect("remove temporary socket");
                return;
            }
            panic!("{error}");
        }

        let mut buffer = [0_u8; 128];
        let size = listener.recv(&mut buffer).unwrap();
        assert_eq!(&buffer[..size], b"READY=1\nSTATUS=ready");
        fs::remove_file(&socket_path).expect("remove temporary socket");
    }

    #[test]
    fn watchdog_interval_uses_half_systemd_deadline() {
        assert_eq!(watchdog_interval_from_usec(0), None);
        assert_eq!(
            watchdog_interval_from_usec(30_000_000),
            Some(Duration::from_secs(15))
        );
        assert_eq!(
            watchdog_interval_from_usec(500_000),
            Some(Duration::from_secs(1))
        );
    }
}
