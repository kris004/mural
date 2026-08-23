pub(crate) fn print_help() {
    println!(
        "muralctl\n\nUSAGE:\n    muralctl [--socket PATH] [--timeout-ms MS] COMMAND [ARGS]\n\nGLOBAL OPTIONS:\n    --socket PATH      Connect to PATH instead of the default mural socket\n    --timeout-ms MS    Give up if the daemon does not respond within MS milliseconds (default: 30000)\n    -V, --version      Print the Mural version and exit\n\nCOMMANDS:\n    next         Show the next wallpaper set\n    back         Show the previous wallpaper set\n    shift        Slide wallpapers forward/backward by one output\n    replace      Replace one current output wallpaper\n    quarantine   Move one current wallpaper to quarantine and replace it\n    favorite     Mark one current wallpaper as favorite\n    unfavorite   Remove one current wallpaper from favorites\n    favorites    List favorite wallpapers\n    current      Show current wallpaper rows\n    rescan       Rescan the top-level wallpaper directory\n    cache        Inspect or warm the canvas thumbnail cache\n    world        Inspect virtual world cache state\n    capabilities Show protocol and compiled-in transition capabilities\n    ping         Check daemon health\n    health       Print supervisor and renderer health as JSON\n    query        Print daemon state as JSON\n    set          Set one or more output image paths\n    preload      Validate paths only; cache warming is handled by `cache warm`\n    clear        Clear known outputs to a color\n    stop         Ask the daemon to exit\n\nRun `muralctl COMMAND --help` for command-specific help."
    );
}

pub(crate) fn print_capabilities_help() {
    println!(
        "USAGE:\n    muralctl capabilities [--json]\n\nShow the daemon protocol version and compiled-in transition schemas."
    );
}

pub(crate) fn print_wallpaper_help(command: &str) {
    let canvas_options = "[--canvas-zoom-out-ms MS] [--canvas-pan-ms MS] [--canvas-zoom-in-ms MS] [--canvas-walk paged|strip] [--canvas-pan-axis auto|horizontal|vertical] [--canvas-overview-scale SCALE] [--canvas-tile-count auto|N] [--canvas-max-tile-count N]";
    let defaults_note = "Defaults are read by murald from its config file unless overridden here.\n\n--transition accepts cut, fade, world, push:DIR, or canvas. --mode selects portal|screen|pan for push transitions, or clipped|morph|overlap|collage|span for canvas transitions. --canvas-walk selects paged row/column canvas order or the older centered strip walk. Canvas modes collage and span require --canvas-walk strip.";
    match command {
        "next" => println!(
            "USAGE:\n    muralctl next [--json] [--transition NAME] [--duration-ms MS] [--easing NAME] [--mode NAME] {canvas_options} [--scale-mode NAME]\n\n{defaults_note}"
        ),
        "back" => println!(
            "USAGE:\n    muralctl back [--json] [--transition NAME] [--duration-ms MS] [--easing NAME] [--mode NAME] {canvas_options} [--scale-mode NAME]\n\n{defaults_note}"
        ),
        "shift" | "shift-forward" | "shift-back" => println!(
            "USAGE:\n    muralctl shift [forward|back] [--json] [--transition NAME] [--duration-ms MS] [--easing NAME] [--mode NAME] {canvas_options} [--scale-mode NAME]\n\n{defaults_note}"
        ),
        "replace" => println!(
            "USAGE:\n    muralctl replace INDEX [--json] [--transition NAME] [--duration-ms MS] [--easing NAME] [--mode NAME] {canvas_options} [--scale-mode NAME]\n\n{defaults_note}"
        ),
        "quarantine" | "quarentine" => println!(
            "USAGE:\n    muralctl quarantine INDEX [--json] [--transition NAME] [--duration-ms MS] [--easing NAME] [--mode NAME] {canvas_options} [--scale-mode NAME]\n\n{defaults_note}"
        ),
        "favorite" => println!("USAGE:\n    muralctl favorite INDEX [--json]"),
        "unfavorite" => println!("USAGE:\n    muralctl unfavorite INDEX [--json]"),
        "favorites" => println!("USAGE:\n    muralctl favorites [--json]"),
        "current" => println!("USAGE:\n    muralctl current [--json]"),
        "rescan" => println!("USAGE:\n    muralctl rescan [--json]"),
        _ => print_help(),
    }
}

pub(crate) fn print_set_help() {
    println!(
        "USAGE:\n    muralctl set --output NAME PATH [OPTIONS]\n    muralctl set --output NAME=PATH [--output NAME=PATH ...] [OPTIONS]\n\nOPTIONS:\n    -o, --output NAME PATH     Output/image pair\n    -o, --output NAME=PATH     Output/image pair\n    -t, --transition NAME      cut, fade, world, push:up, push:down, push:left, push:right\n        --duration-ms MS       Transition duration in milliseconds\n        --easing NAME          linear, ease-out-cubic, ease-in-out-cubic\n        --mode NAME            portal, screen, pan\n        --scale-mode NAME      fill, fit, center, stretch\n        --allow-partial        Request partial success for future renderer support\n        --socket PATH          Override the daemon socket path\n        --timeout-ms MS        Override the daemon response timeout\n\nCanvas still requires wallpaper action history; use next/back/shift/replace/quarantine for canvas."
    );
}

pub(crate) fn print_cache_help() {
    println!(
        "USAGE:\n    muralctl cache status [--json]\n    muralctl cache warm [--scope current|all] [--workers N] [--backend auto|vips|internal] [--json]\n    muralctl cache clear [--json]\n\nCanvas cache warming is nonblocking; the daemon schedules thumbnail work and returns immediately."
    );
}
