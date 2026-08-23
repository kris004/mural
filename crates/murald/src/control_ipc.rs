use std::io::{ErrorKind, Read, Write};
use std::os::unix::net::UnixStream;

use calloop::generic::Generic;
use calloop::{Interest, LoopHandle, Mode, PostAction};
use mural_ipc::{Response, parse_request};

use crate::MuralApp;

const MAX_FRAME_BYTES: usize = 16 * 1024 * 1024;

pub(crate) fn write_frame(stream: &UnixStream, payload: &str) -> Result<(), String> {
    let mut stream = stream;
    let bytes = payload.as_bytes();
    let len = u32::try_from(bytes.len()).map_err(|_| "control frame is too large".to_owned())?;
    stream
        .write_all(&len.to_be_bytes())
        .map_err(|error| format!("failed to write control frame header: {error}"))?;
    stream
        .write_all(bytes)
        .map_err(|error| format!("failed to write control frame body: {error}"))?;
    Ok(())
}

pub(crate) fn read_frame(stream: &UnixStream) -> Result<Option<String>, String> {
    let mut stream = stream;
    let mut header = [0_u8; 4];
    match stream.read_exact(&mut header) {
        Ok(()) => {}
        Err(error) if error.kind() == ErrorKind::UnexpectedEof => return Ok(None),
        Err(error) => return Err(format!("failed to read control frame header: {error}")),
    }
    let len = u32::from_be_bytes(header) as usize;
    if len > MAX_FRAME_BYTES {
        return Err(format!("control frame too large: {len} bytes"));
    }

    let mut payload = vec![0_u8; len];
    stream
        .read_exact(&mut payload)
        .map_err(|error| format!("failed to read control frame body: {error}"))?;
    String::from_utf8(payload).map(Some).map_err(|error| {
        format!(
            "control frame payload is not UTF-8 at byte {}",
            error.utf8_error().valid_up_to()
        )
    })
}

pub(crate) fn insert_renderer_control_source(
    loop_handle: &LoopHandle<'_, MuralApp>,
    stream: UnixStream,
) -> Result<(), String> {
    loop_handle
        .insert_source(
            Generic::new(stream, Interest::READ, Mode::Level),
            |readiness, stream, app| {
                trace_log!(
                    app.trace,
                    "renderer control readiness readable={} writable={} error={}",
                    readiness.readable,
                    readiness.writable,
                    readiness.error
                );
                match read_frame(stream) {
                    Ok(Some(request_json)) => {
                        let response = match parse_request(&request_json) {
                            Ok(request) => app.handle_request(request).0,
                            Err(error) => Response::Error {
                                message: error.to_string(),
                            },
                        };
                        if let Err(error) = write_frame(stream, &response.to_json()) {
                            eprintln!("murald: renderer control response failed: {error}");
                            app.flags.request_exit();
                        }
                    }
                    Ok(None) => {
                        eprintln!("murald: supervisor control channel closed; renderer exiting");
                        app.flags.request_exit();
                    }
                    Err(error) => {
                        eprintln!("murald: renderer control channel error: {error}");
                        app.flags.request_exit();
                    }
                }
                Ok(PostAction::Continue)
            },
        )
        .map_err(|error| format!("failed to insert renderer control event source: {error}"))?;
    Ok(())
}
