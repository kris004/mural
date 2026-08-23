use mural_ipc::{Response, SetRequest};

use crate::image_loader;
use crate::transitions::TargetPlan;
use crate::{MuralApp, transition_name};

impl MuralApp {
    pub(crate) fn set_cut_wallpapers(
        &mut self,
        request: &SetRequest,
        plan: TargetPlan,
    ) -> Response {
        let mut decoded = Vec::with_capacity(plan.starts.len());
        for target in plan.starts {
            trace_log!(self.trace, "set_wallpapers cut: decode {}", target.name);
            match image_loader::load(&target.image_path) {
                Ok(image) => {
                    trace_log!(self.trace, "set_wallpapers cut: decoded {}", target.name);
                    decoded.push((target, image));
                }
                Err(message) => return Response::Error { message },
            }
        }

        let mut uploads = Vec::with_capacity(decoded.len());
        for (target, image) in decoded {
            trace_log!(self.trace, "set_wallpapers cut: upload {}", target.name);
            match self.surfaces[target.surface_index].upload_wallpaper_texture(&self.egl, &image) {
                Ok(texture) => {
                    trace_log!(self.trace, "set_wallpapers cut: uploaded {}", target.name);
                    uploads.push((target.surface_index, target.image_path, texture));
                }
                Err(message) => {
                    for (_, _, texture) in uploads {
                        self.egl.delete_texture(texture);
                    }
                    return Response::Error { message };
                }
            }
        }

        let accepted = uploads.len();
        for (surface_index, image_path, texture) in uploads {
            let surface = &mut self.surfaces[surface_index];
            trace_log!(self.trace, "set_wallpapers cut: render {}", surface.name);
            surface.set_cut_wallpaper(&self.egl, image_path, texture, request.scale_mode);
            if let Err(message) = surface.render_current(&self.egl, self.trace) {
                surface.mark_recreate_needed(self.trace, "cut render", &message);
                return Response::Error { message };
            }
            trace_log!(self.trace, "set_wallpapers cut: rendered {}", surface.name);
        }

        trace_log!(self.trace, "set_wallpapers: complete accepted={accepted}");
        Response::Ack {
            message: format!(
                "rendered {accepted} output(s) with {} transition",
                transition_name(request.transition)
            ),
        }
    }
}
