//! Optional access to the NVIDIA driver profile for this executable.

mod drs;

use std::{
    fs, io,
    path::{Path, PathBuf},
};

use serde::{Deserialize, Serialize};

use self::drs::{Profile, Session, VRR_OVERRIDE, Value};

const FIXED_REFRESH: u32 = 4;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Action {
    Read,
    Enable,
    Restore,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum State {
    Checking,
    Available,
    External,
    Managed,
    Shared,
    Changed,
    Unavailable,
    Failed,
}

#[derive(Debug, Clone, Copy)]
pub(crate) struct Outcome {
    pub(crate) state: State,
    pub(crate) changed: bool,
}

#[derive(Debug, Serialize, Deserialize)]
struct Backup {
    executable: PathBuf,
    profile: String,
    previous: Option<u32>,
    created: bool,
}

#[derive(Debug)]
struct Current {
    profile: String,
    shared: bool,
    settings_count: u32,
    refresh: Option<Value>,
}

impl Current {
    fn local(&self) -> Option<u32> {
        self.refresh.and_then(|value| value.local)
    }
}

fn state(current: Option<&Current>, backup: Option<&Backup>) -> State {
    let Some(current) = current else {
        return State::Available;
    };
    if current.shared {
        return State::Shared;
    }
    if let Some(backup) = backup {
        if backup.profile != current.profile {
            return State::Changed;
        }
        let local = current.local();
        if local == Some(FIXED_REFRESH) {
            return State::Managed;
        }
        if local != backup.previous {
            return State::Changed;
        }
    }
    if current
        .refresh
        .is_some_and(|value| value.effective == FIXED_REFRESH)
    {
        State::External
    } else {
        State::Available
    }
}

pub(crate) async fn run(config: PathBuf, action: Action) -> Outcome {
    tokio::task::spawn_blocking(move || match run_blocking(&config, action) {
        Ok(outcome) => outcome,
        Err(error) => {
            let state = match error {
                Error::Driver(drs::Error::Unavailable) => State::Unavailable,
                Error::Driver(drs::Error::Driver(operation, code)) => {
                    tracing::debug!(operation, code, "NVIDIA profile operation failed");
                    State::Failed
                }
                Error::Io(error) => {
                    tracing::debug!(kind = ?error.kind(), "NVIDIA profile backup operation failed");
                    State::Failed
                }
                Error::Driver(error) => {
                    tracing::debug!(?error, "NVIDIA profile operation failed");
                    State::Failed
                }
            };
            Outcome {
                state,
                changed: false,
            }
        }
    })
    .await
    .unwrap_or(Outcome {
        state: State::Failed,
        changed: false,
    })
}

#[derive(Debug)]
enum Error {
    Driver(drs::Error),
    Io(io::Error),
}

impl From<drs::Error> for Error {
    fn from(error: drs::Error) -> Self {
        Self::Driver(error)
    }
}

impl From<io::Error> for Error {
    fn from(error: io::Error) -> Self {
        Self::Io(error)
    }
}

fn run_blocking(config: &Path, action: Action) -> Result<Outcome, Error> {
    let executable = std::env::current_exe()?;
    run_for(config, executable, action)
}

fn run_for(config: &Path, executable: PathBuf, action: Action) -> Result<Outcome, Error> {
    let journal = config.with_file_name("nvidia-vrr.json");
    let mut backups = read_backups(&journal)?;
    let backup_index = backups
        .iter()
        .position(|backup| backup.executable == executable);
    let backup = backup_index.and_then(|index| backups.get(index));
    let session = Session::open()?;
    let profile = session.find(executable.as_os_str())?;
    let current = profile
        .map(|profile| read_current(&session, profile))
        .transpose()?;
    let state = state(current.as_ref(), backup);
    let unchanged = Outcome {
        state,
        changed: false,
    };
    match (action, state) {
        (Action::Enable, State::Available) => {
            let (profile, name, previous, created) = match (profile, current) {
                (Some(profile), Some(current)) => {
                    (profile, current.profile.clone(), current.local(), false)
                }
                _ => {
                    let name = profile_name()?;
                    let profile = session.create(&name, executable.as_os_str())?;
                    (profile, name, None, true)
                }
            };
            session.set(profile, VRR_OVERRIDE, Some(FIXED_REFRESH))?;
            if let Some(index) = backup_index {
                backups.remove(index);
            }
            backups.push(Backup {
                executable,
                profile: name,
                previous,
                created,
            });
            // Keep the previous values durable before committing the driver transaction.
            save_backups(&journal, &backups)?;
            session.save()?;
            Ok(Outcome {
                state: State::Managed,
                changed: true,
            })
        }
        (Action::Restore, State::Managed) => {
            let (Some(profile), Some(current), Some(index)) = (profile, current, backup_index)
            else {
                return Ok(unchanged);
            };
            let backup = &backups[index];
            let restored = if backup.created && current.settings_count == 1 {
                session.delete(profile)?;
                State::Available
            } else {
                session.set(profile, VRR_OVERRIDE, backup.previous)?;
                self::state(Some(&read_current(&session, profile)?), None)
            };
            // Keep the journal until the driver confirms the restore. A stale entry
            // whose values match the original is harmless after an interrupted cleanup.
            session.save()?;
            backups.remove(index);
            if let Err(error) = save_backups(&journal, &backups) {
                tracing::debug!(kind = ?error.kind(), "Could not clean up the NVIDIA profile backup");
            }
            Ok(Outcome {
                state: restored,
                changed: true,
            })
        }
        _ => Ok(unchanged),
    }
}

fn profile_name() -> io::Result<String> {
    let mut suffix = [0; 8];
    getrandom::fill(&mut suffix).map_err(|error| io::Error::other(error.to_string()))?;
    Ok(format!(
        "Ruscello (fixed refresh {:016x})",
        u64::from_le_bytes(suffix)
    ))
}

fn read_current(session: &Session, profile: Profile) -> Result<Current, drs::Error> {
    let info = session.info(profile)?;
    Ok(Current {
        profile: info.name,
        shared: info.applications != 1,
        settings_count: info.settings,
        refresh: session.get(profile, VRR_OVERRIDE)?,
    })
}

fn read_backups(path: &Path) -> io::Result<Vec<Backup>> {
    match fs::read(path) {
        Ok(bytes) => Ok(serde_json::from_slice(&bytes)?),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(Vec::new()),
        Err(error) => Err(error),
    }
}

fn save_backups(path: &Path, backups: &[Backup]) -> io::Result<()> {
    let directory = path.parent().ok_or(io::ErrorKind::InvalidInput)?;
    fs::create_dir_all(directory)?;
    let mut file = tempfile::NamedTempFile::new_in(directory)?;
    serde_json::to_writer(&mut file, backups)?;
    file.as_file().sync_all()?;
    file.persist(path)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn current(local: Option<u32>, effective: u32) -> Current {
        Current {
            profile: "Ruscello".into(),
            shared: false,
            settings_count: u32::from(local.is_some()),
            refresh: Some(Value { local, effective }),
        }
    }

    #[test]
    fn recognizes_manual_and_managed_profiles_without_taking_over_external_edits() {
        let backup = Backup {
            executable: "ruscello.exe".into(),
            profile: "Ruscello".into(),
            previous: None,
            created: false,
        };
        let mut fixed = current(Some(FIXED_REFRESH), FIXED_REFRESH);
        assert_eq!(state(Some(&fixed), None), State::External);
        assert_eq!(state(Some(&fixed), Some(&backup)), State::Managed);
        fixed.shared = true;
        assert_eq!(state(Some(&fixed), Some(&backup)), State::Shared);
        assert_eq!(
            state(Some(&current(Some(1), 1)), Some(&backup)),
            State::Changed
        );
        assert_eq!(
            state(Some(&current(None, 0)), Some(&backup)),
            State::Available
        );
        assert_eq!(
            state(Some(&current(None, FIXED_REFRESH)), None),
            State::External
        );
        fixed.shared = false;
        fixed.profile = "Different profile".into();
        assert_eq!(state(Some(&fixed), Some(&backup)), State::Changed);
    }

    #[test]
    fn previous_override_is_preserved_in_the_backup() -> anyhow::Result<()> {
        let temp = tempfile::tempdir()?;
        let path = temp.path().join("nvidia-vrr.json");
        let backup = Backup {
            executable: "ruscello.exe".into(),
            profile: "Ruscello".into(),
            previous: Some(2),
            created: false,
        };
        assert!(read_backups(&path)?.is_empty());
        save_backups(&path, &[backup])?;
        assert_eq!(read_backups(&path)?[0].previous, Some(2));
        Ok(())
    }

    #[test]
    #[ignore = "read-only check of the installed NVIDIA driver's profile for the desktop preview"]
    fn reads_installed_driver() -> anyhow::Result<()> {
        let executable = std::env::current_exe()?
            .parent()
            .and_then(Path::parent)
            .ok_or_else(|| anyhow::anyhow!("Preview executable directory is unavailable"))?
            .join("ruscello.exe");
        let session = Session::open().map_err(|error| anyhow::anyhow!("{error:?}"))?;
        let profile = session
            .find(executable.as_os_str())
            .map_err(|error| anyhow::anyhow!("{error:?}"))?
            .ok_or_else(|| anyhow::anyhow!("No NVIDIA profile was found for the preview"))?;
        let current =
            read_current(&session, profile).map_err(|error| anyhow::anyhow!("{error:?}"))?;
        assert_eq!(state(Some(&current), None), State::External);
        assert_eq!(current.local(), Some(FIXED_REFRESH));
        Ok(())
    }

    #[test]
    #[ignore = "writes only a uniquely named temporary NVIDIA profile and removes it after the check"]
    fn applies_and_restores_with_installed_driver() -> anyhow::Result<()> {
        fn driver<T>(result: Result<T, impl std::fmt::Debug>) -> anyhow::Result<T> {
            result.map_err(|error| anyhow::anyhow!("{error:?}"))
        }

        let temp = tempfile::tempdir()?;
        let executable = temp.path().join(format!(
            "{}.exe",
            temp.path()
                .file_name()
                .ok_or_else(|| anyhow::anyhow!("Temporary directory has no name"))?
                .to_string_lossy()
        ));
        fs::copy(std::env::current_exe()?, &executable)?;
        let config = temp.path().join("gui.json");
        let execute = |action| driver(run_for(&config, executable.clone(), action));
        let session = driver(Session::open())?;
        assert!(driver(session.find(executable.as_os_str()))?.is_none());
        drop(session);
        assert_eq!(execute(Action::Read)?.state, State::Available);
        let result = (|| -> anyhow::Result<()> {
            let applied = execute(Action::Enable)?;
            anyhow::ensure!(
                applied.state == State::Managed && applied.changed,
                "Temporary override was not applied"
            );
            anyhow::ensure!(
                execute(Action::Read)?.state == State::Managed,
                "Temporary override was not persisted"
            );
            let restored = execute(Action::Restore)?;
            anyhow::ensure!(
                restored.state == State::Available && restored.changed,
                "Temporary override was not restored"
            );
            anyhow::ensure!(
                execute(Action::Read)?.state == State::Available,
                "Temporary override remained active"
            );
            let session = driver(Session::open())?;
            anyhow::ensure!(
                driver(session.find(executable.as_os_str()))?.is_none(),
                "The empty temporary profile was not removed"
            );
            let profile = driver(session.create(&profile_name()?, executable.as_os_str()))?;
            driver(session.set(profile, VRR_OVERRIDE, Some(1)))?;
            driver(session.save())?;
            drop(session);

            anyhow::ensure!(
                execute(Action::Enable)?.changed,
                "Existing profile was not updated"
            );
            anyhow::ensure!(
                execute(Action::Restore)?.changed,
                "Existing profile was not restored"
            );
            let session = driver(Session::open())?;
            let profile = driver(session.find(executable.as_os_str()))?
                .ok_or_else(|| anyhow::anyhow!("Existing profile was removed"))?;
            anyhow::ensure!(
                driver(session.get(profile, VRR_OVERRIDE))?.and_then(|value| value.local)
                    == Some(1),
                "The previous NVIDIA override was not restored"
            );
            Ok(())
        })();
        // Clean up even if an assertion or driver operation failed.
        let session = driver(Session::open())?;
        if let Some(profile) = driver(session.find(executable.as_os_str()))? {
            driver(session.delete(profile))?;
            driver(session.save())?;
        }
        result
    }
}
