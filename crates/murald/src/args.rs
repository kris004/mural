use std::env;
use std::path::PathBuf;
use std::process;

use mural_ipc::default_socket_path;

use crate::TraceMode;

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct DaemonOptions {
    pub(crate) socket_path: PathBuf,
    pub(crate) trace: TraceMode,
    pub(crate) mode: RunMode,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) enum RunMode {
    Supervisor,
    RendererChild { fd: i32 },
    Standalone,
}

pub(crate) fn parse_args() -> Result<DaemonOptions, String> {
    let mut socket_path = None;
    let mut trace = TraceMode::Disabled;
    let mut mode = RunMode::Supervisor;
    let mut args = env::args().skip(1);

    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--socket" => {
                let value = args
                    .next()
                    .ok_or_else(|| "--socket requires a path".to_owned())?;
                socket_path = Some(PathBuf::from(value));
            }
            "--trace" | "--debug" => {
                trace = if env_flag("MURAL_TRACE_FRAMES") {
                    TraceMode::Frames
                } else {
                    TraceMode::Enabled
                };
            }
            "--renderer-child" => {
                mode = RunMode::RendererChild { fd: -1 };
            }
            "--renderer-fd" => {
                let value = args
                    .next()
                    .ok_or_else(|| "--renderer-fd requires a file descriptor".to_owned())?;
                let fd = value
                    .parse::<i32>()
                    .map_err(|_| format!("--renderer-fd must be an integer: {value}"))?;
                mode = RunMode::RendererChild { fd };
            }
            "--standalone" => {
                mode = RunMode::Standalone;
            }
            "-h" | "--help" => {
                print_help();
                process::exit(0);
            }
            "-V" | "--version" => {
                println!("murald {}", env!("CARGO_PKG_VERSION"));
                process::exit(0);
            }
            _ => return Err(format!("unknown argument: {arg}")),
        }
    }

    if matches!(mode, RunMode::RendererChild { fd } if fd < 0) {
        return Err("--renderer-child requires --renderer-fd FD".to_owned());
    }

    let socket_path = match socket_path {
        Some(socket_path) => socket_path,
        None if matches!(mode, RunMode::RendererChild { .. }) => PathBuf::new(),
        None => default_socket_path()?,
    };

    Ok(DaemonOptions {
        socket_path,
        trace,
        mode,
    })
}

fn env_flag(name: &str) -> bool {
    env::var(name).is_ok_and(|value| {
        let value = value.trim();
        !value.is_empty()
            && !matches!(
                value.to_ascii_lowercase().as_str(),
                "0" | "false" | "no" | "off"
            )
    })
}

fn print_help() {
    println!(
        "murald\n\nUSAGE:\n    murald [--socket PATH] [--trace] [--standalone]\n\nDESCRIPTION:\n    Wayland layer-shell wallpaper daemon. The default process owns public IPC\n    and supervises an isolated Wayland/EGL renderer child.\n\nOPTIONS:\n    --socket PATH    Override the public Unix socket path\n    --trace, --debug Enable verbose diagnostic logging\n    --standalone     Run the legacy single-process daemon for debugging only\n    -V, --version    Print the Mural version and exit\n\nENVIRONMENT:\n    MURAL_TRACE_FRAMES=1 Include per-frame render diagnostics with --trace"
    );
}
