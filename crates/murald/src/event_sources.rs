use std::time::Duration;

use calloop::LoopHandle;
use calloop::channel::{self as calloop_channel, Event as ChannelEvent};
use calloop::timer::{TimeoutAction, Timer};

use crate::MuralApp;
use crate::decode::DecodeResult;
use crate::transitions::canvas::cache::CanvasCacheResult;

pub(crate) fn insert_decode_result_source(
    loop_handle: &LoopHandle<'_, MuralApp>,
    decoded_rx: calloop_channel::Channel<DecodeResult>,
) -> Result<(), String> {
    loop_handle
        .insert_source(decoded_rx, |event, _metadata, app| match event {
            ChannelEvent::Msg(result) => {
                trace_log!(
                    app.trace,
                    "decode result: id={} ok={}",
                    result.id,
                    result.result.is_ok()
                );
                app.handle_decode_result(result);
                app.pump_one_queue_upload();
            }
            ChannelEvent::Closed => {
                eprintln!("murald: decode worker result channel closed");
            }
        })
        .map_err(|error| format!("failed to insert decode result event source: {error}"))?;
    Ok(())
}

pub(crate) fn insert_canvas_cache_result_source(
    loop_handle: &LoopHandle<'_, MuralApp>,
    cache_rx: calloop_channel::Channel<CanvasCacheResult>,
) -> Result<(), String> {
    loop_handle
        .insert_source(cache_rx, |event, _metadata, app| match event {
            ChannelEvent::Msg(result) => {
                trace_log!(
                    app.trace,
                    "canvas cache result: {} ok={}",
                    result.source_path,
                    result.result.is_ok()
                );
                app.handle_canvas_cache_result(&result);
            }
            ChannelEvent::Closed => {
                eprintln!("murald: canvas cache result channel closed");
            }
        })
        .map_err(|error| format!("failed to insert canvas cache result event source: {error}"))?;
    Ok(())
}

pub(crate) fn insert_watchdog_source(
    loop_handle: &LoopHandle<'_, MuralApp>,
    interval: Option<Duration>,
) -> Result<(), String> {
    let Some(interval) = interval else {
        return Ok(());
    };
    loop_handle
        .insert_source(
            Timer::from_duration(interval),
            move |_deadline, _metadata, app| {
                app.notifier.watchdog();
                trace_log!(app.trace, "sd_notify WATCHDOG sent");
                TimeoutAction::ToDuration(interval)
            },
        )
        .map_err(|error| format!("failed to insert systemd watchdog source: {error}"))?;
    Ok(())
}
