use std::{
    io::{self, Write},
    time::Instant,
};

use ruscello_engine::DownloadProgress;
use ruscello_platform::language::Language;
use tokio::sync::watch;

use crate::messages;

pub(crate) async fn render(
    mut receiver: watch::Receiver<DownloadProgress>,
    language: Language,
    terminal: bool,
    visible: bool,
) {
    let messages = messages::get(language);
    let mut previous = (Instant::now(), 0);
    let mut warned = false;
    let mut displayed = false;
    while receiver.changed().await.is_ok() {
        let progress = *receiver.borrow_and_update();
        if progress.restarted && !warned {
            if terminal && displayed {
                let _ = writeln!(io::stderr().lock());
            }
            let _ = writeln!(io::stderr().lock(), "{}", messages.restarted);
            warned = true;
        }
        if !visible {
            continue;
        }
        let now = Instant::now();
        let elapsed = (now - previous.0).as_secs_f64().max(f64::EPSILON);
        let speed = progress.completed.saturating_sub(previous.1) as f64 / elapsed;
        previous = (now, progress.completed);
        let line = if progress.waiting_for_network {
            messages.waiting.to_owned()
        } else {
            format_progress(progress, speed, messages)
        };
        if terminal {
            let _ = write!(io::stderr().lock(), "\r{line:<76}");
        } else {
            let _ = writeln!(io::stderr().lock(), "{line}");
        }
        displayed = true;
    }
    if terminal && displayed {
        let _ = writeln!(io::stderr().lock());
    }
}

fn format_progress(
    progress: DownloadProgress,
    speed: f64,
    messages: &messages::Messages,
) -> String {
    let completed = readable_size(progress.completed as f64);
    let speed = readable_size(speed);
    let state = if progress.resumable {
        ""
    } else {
        messages.non_resumable
    };
    if let Some(total) = progress.total {
        let percent = if total == 0 {
            100.0
        } else {
            100.0 * progress.completed as f64 / total as f64
        };
        format!(
            "{completed} / {} ({percent:.0}%)  {speed}/s  {state}",
            readable_size(total as f64)
        )
    } else {
        format!("{completed}  {speed}/s  {state}")
    }
}

fn readable_size(bytes: f64) -> String {
    let mut value = bytes;
    for unit in ["B", "KiB", "MiB", "GiB", "TiB"] {
        if value < 1024.0 || unit == "TiB" {
            return format!("{value:.1} {unit}");
        }
        value /= 1024.0;
    }
    String::new()
}
