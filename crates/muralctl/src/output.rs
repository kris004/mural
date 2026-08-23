use std::fmt::Write as _;

use mural_ipc::{
    CapabilitiesResponse, WallpaperResponse, parse_capabilities_response, parse_wallpaper_response,
    response_error_message, response_is_error,
};

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum PrintMode {
    RawJson,
    CapabilitiesText,
    WallpaperText,
    WallpaperJson,
}

pub(crate) fn print_response(response: &str, print_mode: PrintMode) -> Result<(), String> {
    if matches!(print_mode, PrintMode::RawJson | PrintMode::WallpaperJson) {
        println!("{response}");
        return Ok(());
    }

    if response_is_error(response) {
        let message = response_error_message(response)
            .unwrap_or_else(|| human_error_fallback(print_mode).to_owned());
        eprintln!("{message}");
        return Ok(());
    }

    match print_mode {
        PrintMode::CapabilitiesText => {
            let capabilities =
                parse_capabilities_response(response).map_err(|error| error.to_string())?;
            print!("{}", format_capabilities_text(&capabilities));
        }
        PrintMode::WallpaperText => {
            let wallpaper =
                parse_wallpaper_response(response).map_err(|error| error.to_string())?;
            print_wallpaper_text(&wallpaper);
        }
        PrintMode::RawJson | PrintMode::WallpaperJson => unreachable!("handled above"),
    }
    Ok(())
}

fn human_error_fallback(print_mode: PrintMode) -> &'static str {
    match print_mode {
        PrintMode::CapabilitiesText => "capabilities request failed",
        PrintMode::WallpaperText => "wallpaper request failed",
        PrintMode::RawJson | PrintMode::WallpaperJson => "daemon request failed",
    }
}

fn format_capabilities_text(response: &CapabilitiesResponse) -> String {
    let mut output = format!(
        "Capabilities schema version: {}\nProtocol version: {}\nDaemon mode: {}\nTransitions:\n",
        response.schema_version,
        response.protocol_version,
        response.daemon_mode.as_str()
    );
    for transition in &response.transitions {
        let available = transition.scopes.explicit_set || transition.scopes.wallpaper_actions;
        let status = if !available && transition.experimental {
            "experimental, unavailable in this daemon mode"
        } else if !available {
            "unavailable in this daemon mode"
        } else if transition.experimental {
            "experimental"
        } else {
            "supported"
        };
        let _ = writeln!(
            output,
            "  {} ({}, {status})",
            transition.name,
            transition.class.as_str()
        );

        let mut scopes = Vec::with_capacity(2);
        if transition.scopes.explicit_set {
            scopes.push("explicit set");
        }
        if transition.scopes.wallpaper_actions {
            scopes.push("wallpaper actions");
        }
        if scopes.is_empty() {
            output.push_str("    Scopes: none (unavailable in this daemon mode)\n");
        } else {
            let _ = writeln!(output, "    Scopes: {}", scopes.join(", "));
        }
        if transition.requirements.is_empty() {
            output.push_str("    Requirements: none\n");
        } else {
            let _ = writeln!(
                output,
                "    Requirements: {}",
                transition.requirements.join(", ")
            );
        }

        if transition.parameters.is_empty() {
            output.push_str("    Parameters: none\n");
            continue;
        }
        output.push_str("    Parameters:\n");
        for parameter in &transition.parameters {
            let _ = write!(
                output,
                "      {} ({}, {})",
                parameter.name,
                parameter.value_type.as_str(),
                if parameter.required {
                    "required"
                } else {
                    "optional"
                }
            );
            if !parameter.allowed_values.is_empty() {
                let _ = write!(output, ": {}", parameter.allowed_values.join(", "));
            }
            if let Some(default_value) = &parameter.default_value {
                let _ = write!(output, "; default={default_value}");
            }
            if let Some(constraint) = &parameter.constraint {
                let _ = write!(output, "; {constraint}");
            }
            output.push('\n');
        }
    }
    output
}

fn print_wallpaper_text(response: &WallpaperResponse) {
    match response.action.as_str() {
        "favorites" => {
            for favorite in &response.favorites {
                println!("{favorite}");
            }
        }
        "current" => {
            for entry in &response.entries {
                let marker = if entry.favorite { "*" } else { "-" };
                println!(
                    "{}\t{}\t{}\t{}",
                    entry.index, entry.output, marker, entry.path
                );
            }
        }
        "favorite" | "unfavorite" | "rescan" => {
            if !response.message.is_empty() {
                println!("{}", response.message);
            }
        }
        _ => {
            for entry in &response.entries {
                println!("{}\t{}", entry.output, entry.path);
            }
            if response.entries.is_empty() && !response.message.is_empty() {
                println!("{}", response.message);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use mural_ipc::DaemonMode;

    #[test]
    fn human_capabilities_output_includes_scopes_and_parameter_schema() {
        let output =
            format_capabilities_text(&CapabilitiesResponse::current(DaemonMode::Supervisor));

        assert!(output.contains("Capabilities schema version: 1"));
        assert!(output.contains("Protocol version: 1"));
        assert!(output.contains("Daemon mode: supervisor"));
        assert!(output.contains("fade (pairwise, supported)"));
        assert!(output.contains("Scopes: explicit set, wallpaper actions"));
        assert!(output.contains("direction (enum, required): up, down, left, right"));
        assert!(output.contains("duration_ms (integer, optional); default=900"));
        assert!(output.contains("canvas (scene, supported)"));
        assert!(output.contains("Scopes: wallpaper actions"));
        assert!(output.contains("Requirements: wallpaper action history"));
        assert!(output.contains("world (scene, experimental)"));
        assert!(output.contains(
            "Requirements: supervisor planning, wallpaper library, ready cache coverage"
        ));
    }

    #[test]
    fn human_capabilities_output_labels_mode_unavailable_transitions() {
        let output =
            format_capabilities_text(&CapabilitiesResponse::current(DaemonMode::Standalone));

        assert!(output.contains("Daemon mode: standalone"));
        assert!(output.contains("world (scene, experimental, unavailable in this daemon mode)"));
        assert!(output.contains("Scopes: none (unavailable in this daemon mode)"));
    }

    #[test]
    fn capability_errors_have_a_capability_specific_fallback() {
        assert_eq!(
            human_error_fallback(PrintMode::CapabilitiesText),
            "capabilities request failed"
        );
        assert_eq!(
            human_error_fallback(PrintMode::WallpaperText),
            "wallpaper request failed"
        );
    }
}
