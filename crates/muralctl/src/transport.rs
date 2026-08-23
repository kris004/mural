use std::io::{ErrorKind, Read, Write};
use std::net::Shutdown;
use std::os::unix::net::UnixStream;
use std::path::Path;
use std::time::Duration;

use mural_ipc::Request;

pub(crate) const DEFAULT_TIMEOUT: Duration = Duration::from_secs(30);

pub(crate) fn send_request(
    socket_path: &Path,
    request: &Request,
    timeout: Duration,
) -> Result<String, String> {
    let mut stream = UnixStream::connect(socket_path)
        .map_err(|error| format!("failed to connect to {}: {error}", socket_path.display()))?;
    stream
        .set_read_timeout(Some(timeout))
        .map_err(|error| format!("failed to set response timeout: {error}"))?;
    stream
        .set_write_timeout(Some(timeout))
        .map_err(|error| format!("failed to set request timeout: {error}"))?;

    let request_json = request.to_json();
    stream.write_all(request_json.as_bytes()).map_err(|error| {
        if is_timeout(&error) {
            format!(
                "timed out writing request to daemon after {}",
                format_duration(timeout)
            )
        } else {
            format!("failed to write request: {error}")
        }
    })?;
    stream
        .shutdown(Shutdown::Write)
        .map_err(|error| format!("failed to finish request: {error}"))?;

    let mut response = String::new();
    stream.read_to_string(&mut response).map_err(|error| {
        if is_timeout(&error) {
            format!(
                "timed out waiting for daemon response after {}",
                format_duration(timeout)
            )
        } else {
            format!("failed to read response: {error}")
        }
    })?;

    if response.is_empty() {
        return Err("daemon closed connection without a response".to_owned());
    }

    Ok(response)
}

fn is_timeout(error: &std::io::Error) -> bool {
    matches!(error.kind(), ErrorKind::TimedOut | ErrorKind::WouldBlock)
}

fn format_duration(duration: Duration) -> String {
    if duration.as_millis() < 1_000 {
        format!("{}ms", duration.as_millis())
    } else if duration.as_millis().is_multiple_of(1_000) {
        format!("{}s", duration.as_secs())
    } else {
        format!("{}ms", duration.as_millis())
    }
}

#[cfg(test)]
mod tests {
    use std::fs;
    use std::io::Read as _;
    use std::os::unix::net::UnixListener;
    use std::sync::mpsc;
    use std::thread;
    use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

    use mural_ipc::Request;

    use super::send_request;

    #[test]
    fn send_request_times_out_when_daemon_never_replies() {
        let dir = std::env::temp_dir().join(format!(
            "muralctl-timeout-test-{}-{}",
            std::process::id(),
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .expect("system clock is after Unix epoch")
                .as_nanos()
        ));
        fs::create_dir_all(&dir).expect("create temporary socket directory");
        let socket_path = dir.join("mural.sock");
        let listener = UnixListener::bind(&socket_path).expect("bind fake daemon socket");
        let (release_tx, release_rx) = mpsc::channel();

        let server = thread::spawn(move || {
            let (mut stream, _) = listener.accept().expect("accept client connection");
            let mut request = String::new();
            stream
                .read_to_string(&mut request)
                .expect("read client request");
            assert!(!request.is_empty());
            let _ = release_rx.recv_timeout(Duration::from_secs(2));
        });

        let start = Instant::now();
        let error = send_request(&socket_path, &Request::Ping, Duration::from_millis(50))
            .expect_err("request should time out");

        assert!(
            start.elapsed() < Duration::from_secs(1),
            "request should honor the configured timeout"
        );
        assert!(
            error.contains("timed out waiting for daemon response after 50ms"),
            "unexpected error: {error}"
        );

        let _ = release_tx.send(());
        server.join().expect("fake daemon thread exits");
        fs::remove_dir_all(&dir).expect("remove temporary socket directory");
    }
}
