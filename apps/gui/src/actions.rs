use std::path::PathBuf;

use iced::{
    Event, event,
    keyboard::{self, Key, key::Named},
    window,
};
use ruscello_core::{DownloadId, Priority, Status};
use ruscello_engine::EngineHandle;

use crate::app::Message;

#[derive(Debug, Clone, Copy)]
pub(crate) enum Action {
    TogglePause,
    Cancel,
    OpenFile,
    OpenFolder,
    CopyLink,
    Priority(Priority),
    MoveUp,
    MoveDown,
}

#[derive(Debug, Clone, Copy)]
pub(crate) enum Shortcut {
    Add,
    Paste,
    Search,
    Settings,
    Escape,
    All,
    Action(Action),
}

pub(crate) fn event(event: Event, status: event::Status, _: window::Id) -> Option<Message> {
    match event {
        Event::Window(window::Event::FileDropped(path)) => Some(Message::Files(vec![path])),
        Event::Window(window::Event::Unfocused) => {
            Some(Message::Modifiers(keyboard::Modifiers::default()))
        }
        Event::Keyboard(keyboard::Event::ModifiersChanged(modifiers)) => {
            Some(Message::Modifiers(modifiers))
        }
        Event::Keyboard(keyboard::Event::KeyPressed { key, modifiers, .. }) => {
            let shortcut = match key.as_ref() {
                Key::Character(key) if modifiers.command() || modifiers.control() => {
                    match key.to_ascii_lowercase().as_str() {
                        "n" => Shortcut::Add,
                        "v" if status == event::Status::Ignored => Shortcut::Paste,
                        "f" => Shortcut::Search,
                        "," => Shortcut::Settings,
                        "a" if status == event::Status::Ignored => Shortcut::All,
                        _ => return None,
                    }
                }
                Key::Named(Named::Escape) => Shortcut::Escape,
                Key::Named(Named::Enter) if status == event::Status::Ignored => {
                    Shortcut::Action(if modifiers.command() || modifiers.control() {
                        Action::OpenFolder
                    } else {
                        Action::OpenFile
                    })
                }
                Key::Named(Named::Space) if status == event::Status::Ignored => {
                    Shortcut::Action(Action::TogglePause)
                }
                Key::Named(Named::Delete) if status == event::Status::Ignored => {
                    Shortcut::Action(Action::Cancel)
                }
                _ => return None,
            };
            Some(Message::Shortcut(shortcut))
        }
        _ => None,
    }
}

pub(crate) fn can_pause(status: Status) -> bool {
    status.is_active() || matches!(status, Status::Queued | Status::WaitingUser)
}

pub(crate) fn can_resume(status: Status) -> bool {
    matches!(status, Status::Paused | Status::Failed { .. })
}

#[derive(Debug)]
pub(crate) enum Command {
    Pause(DownloadId),
    Resume(DownloadId),
    Remove(DownloadId, bool),
    Priority(DownloadId, Priority),
    Move(DownloadId, Option<DownloadId>),
}

pub(crate) async fn run(engine: EngineHandle, commands: Vec<Command>) -> bool {
    let mut success = true;
    for command in commands {
        let result = match command {
            Command::Pause(id) => engine.pause(id).await,
            Command::Resume(id) => engine.resume(id).await,
            Command::Remove(id, delete) => engine.cancel(id, delete).await,
            Command::Priority(id, priority) => engine.set_priority(id, priority).await,
            Command::Move(id, before) => engine.move_before(id, before).await,
        };
        if result.is_err() {
            tracing::debug!("Queue action failed");
            success = false;
        }
    }
    success
}

pub(crate) async fn open(paths: Vec<PathBuf>) -> bool {
    tokio::task::spawn_blocking(move || {
        let mut success = true;
        for path in paths {
            if !path.exists() || open::that_detached(path).is_err() {
                success = false;
            }
        }
        success
    })
    .await
    .unwrap_or(false)
}
