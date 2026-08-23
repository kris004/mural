use std::fs::{self, DirBuilder, Permissions};
use std::io::{Read, Write};
use std::net::Shutdown;
use std::os::unix::fs::{DirBuilderExt, FileTypeExt, MetadataExt, PermissionsExt};
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::Path;
use std::time::Duration;

use calloop::generic::Generic;
use calloop::{Interest, LoopHandle, Mode, PostAction};
use mural_ipc::{
    CapabilitiesResponse, DaemonMode, PROTOCOL_VERSION, QueryResponse, Request, Response,
    SetRequest, parse_public_request,
};

use crate::MuralApp;
use crate::transitions::canvas::CanvasPreviewPlan;
use crate::wallpaper_actions::wallpaper_action_trace_name;

const MAX_PUBLIC_REQUEST_BYTES: usize = 1024 * 1024;

pub(crate) fn insert_ipc_source(
    loop_handle: &LoopHandle<'_, MuralApp>,
    listener: UnixListener,
    socket_path: &Path,
) -> Result<(), String> {
    let ipc_socket_path = socket_path.to_owned();
    loop_handle
        .insert_source(
            Generic::new(listener, Interest::READ, Mode::Level),
            move |readiness, listener, app| {
                trace_log!(
                    app.trace,
                    "ipc readiness readable={} writable={} error={}",
                    readiness.readable,
                    readiness.writable,
                    readiness.error
                );
                let accepted = app.accept_ipc_connections(listener, &ipc_socket_path);
                trace_log!(
                    app.trace,
                    "ipc readiness handler accepted {accepted} connection(s); exit={}",
                    app.flags.should_exit()
                );
                Ok(PostAction::Continue)
            },
        )
        .map_err(|error| format!("failed to insert IPC event source: {error}"))?;
    Ok(())
}

pub(crate) fn prepare_socket_path(socket_path: &Path) -> Result<(), String> {
    let fallback_parent = current_fallback_socket_parent(socket_path);
    if let Some(parent) = socket_path
        .parent()
        .filter(|parent| !parent.as_os_str().is_empty())
    {
        let mut builder = DirBuilder::new();
        builder.recursive(true).mode(0o700);
        builder
            .create(parent)
            .map_err(|error| format!("failed to create {}: {error}", parent.display()))?;
    }
    if let Some((parent, uid)) = fallback_parent {
        validate_fallback_socket_parent(&parent, uid)?;
    }

    let metadata = match fs::symlink_metadata(socket_path) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(()),
        Err(error) => {
            return Err(format!(
                "failed to inspect existing socket path {}: {error}",
                socket_path.display()
            ));
        }
    };

    if !metadata.file_type().is_socket() {
        return Err(format!(
            "refusing to replace non-socket path {}",
            socket_path.display()
        ));
    }

    if UnixStream::connect(socket_path).is_ok() {
        Err(format!(
            "socket {} is already accepting connections",
            socket_path.display()
        ))
    } else {
        fs::remove_file(socket_path).map_err(|error| {
            format!(
                "failed to remove stale socket {}: {error}",
                socket_path.display()
            )
        })?;
        Ok(())
    }
}

fn current_fallback_socket_parent(socket_path: &Path) -> Option<(std::path::PathBuf, u32)> {
    let uid = fs::metadata("/proc/self").ok()?.uid();
    let parent = Path::new("/tmp").join(format!("mural-{uid}"));
    (socket_path == parent.join("mural.sock")).then_some((parent, uid))
}

fn validate_fallback_socket_parent(parent: &Path, expected_uid: u32) -> Result<(), String> {
    let metadata = fs::symlink_metadata(parent)
        .map_err(|error| format!("failed to inspect {}: {error}", parent.display()))?;
    if !metadata.file_type().is_dir() {
        return Err(format!(
            "fallback socket parent {} is not a directory",
            parent.display()
        ));
    }
    if metadata.uid() != expected_uid {
        return Err(format!(
            "fallback socket parent {} is owned by uid {}, expected uid {expected_uid}",
            parent.display(),
            metadata.uid()
        ));
    }

    let mode = metadata.permissions().mode() & 0o777;
    if mode & 0o077 != 0 {
        return Err(format!(
            "fallback socket parent {} has insecure mode {mode:04o}; remove group and other permissions",
            parent.display()
        ));
    }
    Ok(())
}

pub(crate) fn bind_public_listener(socket_path: &Path) -> Result<UnixListener, String> {
    prepare_socket_path(socket_path)?;

    // SAFETY: both callers bind once during single-threaded startup, before
    // worker threads or the renderer child exist. Restore the process umask
    // immediately after bind so the socket node is owner-only atomically.
    let previous_umask = unsafe { libc::umask(0o077) };
    let listener = UnixListener::bind(socket_path);
    unsafe {
        libc::umask(previous_umask);
    }

    let listener =
        listener.map_err(|error| format!("failed to bind {}: {error}", socket_path.display()))?;
    if let Err(error) = fs::set_permissions(socket_path, Permissions::from_mode(0o600)) {
        drop(listener);
        let _ = fs::remove_file(socket_path);
        return Err(format!(
            "failed to restrict socket {} to owner access: {error}",
            socket_path.display()
        ));
    }
    Ok(listener)
}

pub(crate) fn read_public_request_json<R: Read>(reader: &mut R) -> Result<(String, usize), String> {
    let mut request_json = String::new();
    let bytes_read = reader
        .take((MAX_PUBLIC_REQUEST_BYTES + 1) as u64)
        .read_to_string(&mut request_json)
        .map_err(|error| error.to_string())?;
    if bytes_read > MAX_PUBLIC_REQUEST_BYTES {
        return Err(format!(
            "request exceeds the {MAX_PUBLIC_REQUEST_BYTES}-byte public IPC limit"
        ));
    }
    Ok((request_json, bytes_read))
}

impl MuralApp {
    pub(crate) fn accept_ipc_connections(
        &mut self,
        listener: &UnixListener,
        socket_path: &Path,
    ) -> usize {
        let mut accepted = 0_usize;
        loop {
            match listener.accept() {
                Ok((mut stream, _addr)) => {
                    accepted += 1;
                    let connection_id = self.next_ipc_id();
                    trace_log!(self.trace, "ipc #{connection_id}: accepted");
                    if let Err(error) = stream.set_read_timeout(Some(Duration::from_secs(2))) {
                        eprintln!("murald: failed to set IPC read timeout: {error}");
                    }
                    let should_stop = self.handle_ipc_connection(&mut stream, connection_id);
                    if should_stop {
                        self.flags.request_exit();
                        return accepted;
                    }
                }
                Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                    trace_log!(
                        self.trace,
                        "ipc accept drained after {accepted} connection(s)"
                    );
                    return accepted;
                }
                Err(error) => {
                    eprintln!(
                        "murald: failed to accept IPC connection on {}: {error}",
                        socket_path.display()
                    );
                    return accepted;
                }
            }
        }
    }

    fn next_ipc_id(&mut self) -> u64 {
        let id = self.next_ipc_id;
        self.next_ipc_id = self.next_ipc_id.wrapping_add(1).max(1);
        id
    }

    fn handle_ipc_connection(&mut self, stream: &mut UnixStream, connection_id: u64) -> bool {
        let (response, should_stop) = match read_public_request_json(stream) {
            Ok((request_json, bytes_read)) => {
                trace_log!(
                    self.trace,
                    "ipc #{connection_id}: read {bytes_read} byte(s)"
                );
                match parse_public_request(&request_json) {
                    Ok(request) => {
                        trace_log!(
                            self.trace,
                            "ipc #{connection_id}: request {}",
                            request_trace_name(&request)
                        );
                        self.handle_request(request)
                    }
                    Err(error) => {
                        trace_log!(self.trace, "ipc #{connection_id}: parse error: {error}");
                        (
                            Response::Error {
                                message: error.to_string(),
                            },
                            false,
                        )
                    }
                }
            }
            Err(error) => {
                trace_log!(self.trace, "ipc #{connection_id}: read error: {error}");
                (
                    Response::Error {
                        message: format!("failed to read request: {error}"),
                    },
                    false,
                )
            }
        };

        let response_json = response.to_json();
        trace_log!(
            self.trace,
            "ipc #{connection_id}: response {}; stop={should_stop}",
            response_trace_name(&response)
        );
        if let Err(error) = stream.write_all(response_json.as_bytes()) {
            eprintln!("murald: failed to write IPC response: {error}");
        } else {
            trace_log!(
                self.trace,
                "ipc #{connection_id}: wrote {} byte(s)",
                response_json.len()
            );
        }
        let _ = stream.shutdown(Shutdown::Both);

        should_stop
    }

    pub(crate) fn handle_request(&mut self, request: Request) -> (Response, bool) {
        match request {
            Request::Ping => (
                Response::Pong {
                    version: PROTOCOL_VERSION,
                },
                false,
            ),
            Request::Capabilities => (
                Response::Capabilities(CapabilitiesResponse::current(DaemonMode::Standalone)),
                false,
            ),
            Request::Health => (
                Response::Health(Box::new(self.renderer_health_response())),
                false,
            ),
            Request::Query => (
                Response::Query(QueryResponse {
                    outputs: self.query_outputs(),
                }),
                false,
            ),
            Request::Set(request) => (self.set_wallpapers(&request), false),
            Request::Preload(request) => (self.handle_preload_request(&request), false),
            Request::Clear(request) => (self.clear(request), false),
            Request::Wallpaper(request) => (self.handle_wallpaper_request(&request), false),
            Request::Cache(request) => (self.handle_cache_request(&request), false),
            Request::RenderCanvasSet(request) => {
                let set = SetRequest {
                    outputs: request.outputs,
                    transition: request.transition,
                    scale_mode: request.scale_mode,
                    allow_partial: request.allow_partial,
                };
                let preview = CanvasPreviewPlan {
                    paths: request.preview_paths,
                    start_index: request.preview_start,
                };
                (
                    self.set_canvas_wallpapers_from_preview(&set, &preview),
                    false,
                )
            }
            Request::RenderWorldSet(request) => (self.set_world_wallpapers(&request), false),
            Request::Stop => (
                Response::Ack {
                    message: "stopping".to_owned(),
                },
                true,
            ),
        }
    }
}

fn request_trace_name(request: &Request) -> &'static str {
    match request {
        Request::Ping => "ping",
        Request::Capabilities => "capabilities",
        Request::Health => "health",
        Request::Query => "query",
        Request::Set(_) => "set",
        Request::Preload(_) => "preload",
        Request::Clear(_) => "clear",
        Request::Wallpaper(request) => wallpaper_action_trace_name(&request.action),
        Request::Cache(_) => "cache",
        Request::RenderCanvasSet(_) => "renderer_canvas_set",
        Request::RenderWorldSet(_) => "renderer_world_set",
        Request::Stop => "stop",
    }
}

fn response_trace_name(response: &Response) -> &'static str {
    match response {
        Response::Pong { .. } => "pong",
        Response::Capabilities(_) => "capabilities",
        Response::Ack { .. } => "ack",
        Response::Health(_) => "health",
        Response::Query(_) => "query",
        Response::Wallpaper(_) => "wallpaper",
        Response::Cache(_) => "cache",
        Response::Error { .. } => "error",
    }
}

#[cfg(test)]
mod tests {
    use std::env;
    use std::fs;
    use std::io::Cursor;
    use std::os::unix::fs::{MetadataExt, PermissionsExt};
    use std::path::PathBuf;
    use std::process;

    use super::{
        MAX_PUBLIC_REQUEST_BYTES, read_public_request_json, validate_fallback_socket_parent,
    };

    fn temp_parent(name: &str) -> PathBuf {
        env::temp_dir().join(format!("mural-ipc-test-{}-{name}", process::id()))
    }

    #[test]
    fn public_request_reader_rejects_oversized_input() {
        let mut reader = Cursor::new(vec![b' '; MAX_PUBLIC_REQUEST_BYTES + 1]);
        let error = read_public_request_json(&mut reader).unwrap_err();

        assert_eq!(
            error,
            format!("request exceeds the {MAX_PUBLIC_REQUEST_BYTES}-byte public IPC limit")
        );
    }

    #[test]
    fn fallback_socket_parent_must_be_owner_only() {
        let parent = temp_parent("parent-mode");
        let _ = fs::remove_dir(&parent);
        fs::create_dir(&parent).unwrap();
        let uid = fs::symlink_metadata(&parent).unwrap().uid();
        fs::set_permissions(&parent, fs::Permissions::from_mode(0o750)).unwrap();

        let error = validate_fallback_socket_parent(&parent, uid).unwrap_err();
        assert!(error.contains("insecure mode 0750"));

        fs::set_permissions(&parent, fs::Permissions::from_mode(0o700)).unwrap();
        assert!(validate_fallback_socket_parent(&parent, uid).is_ok());
        fs::remove_dir(parent).unwrap();
    }

    #[test]
    fn fallback_socket_parent_must_have_expected_owner() {
        let parent = temp_parent("parent-owner");
        let _ = fs::remove_dir(&parent);
        fs::create_dir(&parent).unwrap();
        fs::set_permissions(&parent, fs::Permissions::from_mode(0o700)).unwrap();
        let uid = fs::symlink_metadata(&parent).unwrap().uid();

        let error = validate_fallback_socket_parent(&parent, uid.wrapping_add(1)).unwrap_err();
        assert!(error.contains("is owned by uid"));

        fs::remove_dir(parent).unwrap();
    }
}
