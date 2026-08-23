use mural_ipc::{PreloadRequest, Response};

use crate::{MuralApp, validate_image_paths};

impl MuralApp {
    pub(crate) fn handle_preload_request(&mut self, request: &PreloadRequest) -> Response {
        trace_log!(
            self.trace,
            "preload request: validate outputs={}",
            request.outputs.len()
        );
        match validate_image_paths(&request.outputs) {
            Ok(()) => Response::Ack {
                message: format!(
                    "validated {} preload path(s); decode/upload is not implemented yet",
                    request.outputs.len()
                ),
            },
            Err(message) => Response::Error { message },
        }
    }
}
