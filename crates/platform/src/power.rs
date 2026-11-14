//! Operating system sleep inhibition and power transition helpers.

use std::{future::Future, pin::Pin, sync::Arc};

use serde::{Deserialize, Serialize};
use thiserror::Error;

/// Action executed after the download queue becomes empty and idle.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum QueueCompletionAction {
    #[default]
    Nothing,
    Notify,
    Sleep,
    Shutdown,
}

impl QueueCompletionAction {
    /// Whether this action changes OS power state and therefore warrants a cancellable countdown.
    pub const fn requires_countdown(self) -> bool {
        matches!(self, Self::Sleep | Self::Shutdown)
    }
}

/// Errors returned by OS sleep inhibition or power management operations.
#[derive(Debug, Error)]
pub enum PowerError {
    #[error("operating system power interface is unavailable: {0}")]
    Unavailable(String),
    #[error("operating system power command failed: {0}")]
    Failed(String),
}

pub type PowerFuture<'a, T> = Pin<Box<dyn Future<Output = Result<T, PowerError>> + Send + 'a>>;

/// Prevents the host OS from entering idle sleep while downloads are active.
pub trait SleepInhibitor: std::fmt::Debug + Send + Sync {
    /// Updates sleep inhibition state; repeated calls with the same state are idempotent.
    fn set_inhibited<'a>(&'a self, inhibited: bool, reason: &'a str) -> PowerFuture<'a, ()>;
}

/// Executes queue completion power actions (notification, sleep, shutdown).
pub trait PowerController: std::fmt::Debug + Send + Sync {
    fn execute(&self, action: QueueCompletionAction) -> PowerFuture<'_, ()>;
}

/// No-op inhibitor used when tests disable OS power interaction.
#[derive(Debug, Default, Clone, Copy)]
pub struct NoopSleepInhibitor;

impl SleepInhibitor for NoopSleepInhibitor {
    fn set_inhibited<'a>(&'a self, _inhibited: bool, _reason: &'a str) -> PowerFuture<'a, ()> {
        Box::pin(async { Ok(()) })
    }
}

/// Default cross-platform sleep inhibitor using native OS APIs.
#[derive(Debug)]
pub struct SystemSleepInhibitor {
    inner: imp::InhibitorBackend,
}

impl Default for SystemSleepInhibitor {
    fn default() -> Self {
        Self::new()
    }
}

impl SystemSleepInhibitor {
    pub fn new() -> Self {
        Self {
            inner: imp::InhibitorBackend::new(),
        }
    }
}

impl SleepInhibitor for SystemSleepInhibitor {
    fn set_inhibited<'a>(&'a self, inhibited: bool, reason: &'a str) -> PowerFuture<'a, ()> {
        Box::pin(self.inner.set_inhibited(inhibited, reason))
    }
}

/// Default cross-platform power controller using native OS endpoints.
#[derive(Debug, Default, Clone)]
pub struct SystemPowerController {
    _private: (),
}

impl SystemPowerController {
    pub fn new() -> Self {
        Self { _private: () }
    }
}

impl PowerController for SystemPowerController {
    fn execute(&self, action: QueueCompletionAction) -> PowerFuture<'_, ()> {
        Box::pin(async move {
            match action {
                QueueCompletionAction::Nothing | QueueCompletionAction::Notify => Ok(()),
                QueueCompletionAction::Sleep => imp::sleep_system().await,
                QueueCompletionAction::Shutdown => imp::shutdown_system().await,
            }
        })
    }
}

/// Convenience alias for shared sleep inhibitor trait objects.
pub type SharedSleepInhibitor = Arc<dyn SleepInhibitor>;
/// Convenience alias for shared power controller trait objects.
pub type SharedPowerController = Arc<dyn PowerController>;

#[cfg(target_os = "linux")]
mod imp {
    use tokio::sync::Mutex;
    use zbus::zvariant::OwnedFd;

    use super::PowerError;

    #[derive(Debug)]
    pub(super) struct InhibitorBackend {
        guard: Mutex<Option<OwnedFd>>,
    }

    impl InhibitorBackend {
        pub(super) fn new() -> Self {
            Self {
                guard: Mutex::new(None),
            }
        }

        pub(super) async fn set_inhibited(
            &self,
            inhibited: bool,
            reason: &str,
        ) -> Result<(), PowerError> {
            let mut guard = self.guard.lock().await;
            match (inhibited, guard.is_some()) {
                (true, true) | (false, false) => Ok(()),
                (false, true) => {
                    *guard = None;
                    tracing::debug!("Released Linux login1 sleep inhibitor");
                    Ok(())
                }
                (true, false) => {
                    let fd = acquire_login1_inhibit(reason).await?;
                    *guard = Some(fd);
                    tracing::debug!("Acquired Linux login1 sleep inhibitor");
                    Ok(())
                }
            }
        }
    }

    async fn acquire_login1_inhibit(reason: &str) -> Result<OwnedFd, PowerError> {
        let connection = zbus::Connection::system()
            .await
            .map_err(|error| PowerError::Unavailable(error.to_string()))?;
        let reply = connection
            .call_method(
                Some("org.freedesktop.login1"),
                "/org/freedesktop/login1",
                Some("org.freedesktop.login1.Manager"),
                "Inhibit",
                &("sleep", "Ruscello", reason, "block"),
            )
            .await
            .map_err(|error| PowerError::Unavailable(error.to_string()))?;
        reply
            .body()
            .deserialize::<OwnedFd>()
            .map_err(|error| PowerError::Failed(error.to_string()))
    }

    async fn call_login1_power(method: &str) -> Result<(), PowerError> {
        let connection = zbus::Connection::system()
            .await
            .map_err(|error| PowerError::Unavailable(error.to_string()))?;
        connection
            .call_method(
                Some("org.freedesktop.login1"),
                "/org/freedesktop/login1",
                Some("org.freedesktop.login1.Manager"),
                method,
                &(false,),
            )
            .await
            .map(|_| ())
            .map_err(|error| PowerError::Failed(error.to_string()))
    }

    async fn run_systemctl(verb: &str) -> Result<(), PowerError> {
        let status = tokio::process::Command::new("systemctl")
            .arg(verb)
            .status()
            .await
            .map_err(|error| PowerError::Unavailable(error.to_string()))?;
        if status.success() {
            Ok(())
        } else {
            Err(PowerError::Failed(format!(
                "systemctl {verb} exited with {status}"
            )))
        }
    }

    pub(super) async fn sleep_system() -> Result<(), PowerError> {
        match call_login1_power("Suspend").await {
            Ok(()) => Ok(()),
            Err(dbus_err) => {
                tracing::debug!(error = %dbus_err, "login1 Suspend failed, falling back to systemctl");
                run_systemctl("suspend").await
            }
        }
    }

    pub(super) async fn shutdown_system() -> Result<(), PowerError> {
        match call_login1_power("PowerOff").await {
            Ok(()) => Ok(()),
            Err(dbus_err) => {
                tracing::debug!(error = %dbus_err, "login1 PowerOff failed, falling back to systemctl");
                run_systemctl("poweroff").await
            }
        }
    }
}

#[cfg(windows)]
mod imp {
    #![allow(unsafe_code)]

    use std::{
        sync::{Mutex, mpsc},
        thread::JoinHandle,
    };
    use windows_sys::Win32::{
        Foundation::{CloseHandle, LUID},
        Security::{
            AdjustTokenPrivileges, LUID_AND_ATTRIBUTES, LookupPrivilegeValueW,
            SE_PRIVILEGE_ENABLED, SE_SHUTDOWN_NAME, TOKEN_ADJUST_PRIVILEGES, TOKEN_PRIVILEGES,
        },
        System::{
            Power::{ES_CONTINUOUS, ES_SYSTEM_REQUIRED, SetSuspendState, SetThreadExecutionState},
            Shutdown::{EWX_POWEROFF, EWX_SHUTDOWN, ExitWindowsEx, SHTDN_REASON_MAJOR_APPLICATION},
            Threading::{GetCurrentProcess, OpenProcessToken},
        },
    };

    use super::PowerError;

    #[derive(Debug)]
    struct Worker {
        tx: mpsc::Sender<bool>,
        handle: JoinHandle<()>,
    }

    /// Calls `SetThreadExecutionState` on a single dedicated OS thread because execution
    /// state flags are tracked per-thread in Win32.
    #[derive(Debug)]
    pub(super) struct InhibitorBackend {
        state: Mutex<Option<Worker>>,
        active: Mutex<bool>,
    }

    impl InhibitorBackend {
        pub(super) fn new() -> Self {
            Self {
                state: Mutex::new(None),
                active: Mutex::new(false),
            }
        }

        pub(super) async fn set_inhibited(
            &self,
            inhibited: bool,
            _reason: &str,
        ) -> Result<(), PowerError> {
            let mut active = self
                .active
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            if *active == inhibited {
                return Ok(());
            }
            let mut worker_guard = self
                .state
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            if worker_guard.is_none() {
                if !inhibited {
                    *active = false;
                    return Ok(());
                }
                let (tx, rx) = mpsc::channel::<bool>();
                let handle = std::thread::Builder::new()
                    .name("ruscello-sleep-inhibit".into())
                    .spawn(move || {
                        while let Ok(enable) = rx.recv() {
                            let flags = if enable {
                                ES_CONTINUOUS | ES_SYSTEM_REQUIRED
                            } else {
                                ES_CONTINUOUS
                            };
                            // SAFETY: `SetThreadExecutionState` takes bitflags by value and has no
                            // pointer or lifetime preconditions.
                            unsafe {
                                let _ = SetThreadExecutionState(flags);
                            }
                        }
                        // SAFETY: Clears this thread's continuous sleep-inhibition flag on exit.
                        unsafe {
                            let _ = SetThreadExecutionState(ES_CONTINUOUS);
                        }
                    })
                    .map_err(|error| PowerError::Failed(error.to_string()))?;
                *worker_guard = Some(Worker { tx, handle });
            }
            if let Some(worker) = worker_guard.as_ref() {
                worker
                    .tx
                    .send(inhibited)
                    .map_err(|error| PowerError::Failed(error.to_string()))?;
                *active = inhibited;
            }
            Ok(())
        }
    }

    impl Drop for InhibitorBackend {
        fn drop(&mut self) {
            let worker = self
                .state
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .take();
            if let Some(worker) = worker {
                let _ = worker.tx.send(false);
                drop(worker.tx);
                let _ = worker.handle.join();
            }
        }
    }

    pub(super) async fn sleep_system() -> Result<(), PowerError> {
        tokio::task::spawn_blocking(|| {
            // SAFETY: `SetSuspendState` takes plain booleans and suspends the system cleanly.
            let ok = unsafe { SetSuspendState(false, false, false) };
            if ok {
                Ok(())
            } else {
                Err(PowerError::Failed("SetSuspendState failed".into()))
            }
        })
        .await
        .map_err(|error| PowerError::Failed(error.to_string()))?
    }

    pub(super) async fn shutdown_system() -> Result<(), PowerError> {
        tokio::task::spawn_blocking(|| {
            // SAFETY: Calls standard Win32 token privilege adjustment followed by `ExitWindowsEx`.
            // All pointers point to stack-local initialized structs and valid static wide strings,
            // and the process token handle is always closed via `CloseHandle`.
            unsafe {
                let mut token = std::ptr::null_mut();
                if OpenProcessToken(GetCurrentProcess(), TOKEN_ADJUST_PRIVILEGES, &raw mut token)
                    == 0
                {
                    return Err(PowerError::Failed("OpenProcessToken failed".into()));
                }
                let mut luid = LUID {
                    LowPart: 0,
                    HighPart: 0,
                };
                let lookup_ok =
                    LookupPrivilegeValueW(std::ptr::null(), SE_SHUTDOWN_NAME, &raw mut luid);
                if lookup_ok == 0 {
                    let _ = CloseHandle(token);
                    return Err(PowerError::Failed("LookupPrivilegeValueW failed".into()));
                }
                let tp = TOKEN_PRIVILEGES {
                    PrivilegeCount: 1,
                    Privileges: [LUID_AND_ATTRIBUTES {
                        Luid: luid,
                        Attributes: SE_PRIVILEGE_ENABLED,
                    }],
                };
                let adjusted = AdjustTokenPrivileges(
                    token,
                    0,
                    &raw const tp,
                    0,
                    std::ptr::null_mut(),
                    std::ptr::null_mut(),
                );
                let _ = CloseHandle(token);
                if adjusted == 0 {
                    return Err(PowerError::Failed("AdjustTokenPrivileges failed".into()));
                }
                if ExitWindowsEx(EWX_SHUTDOWN | EWX_POWEROFF, SHTDN_REASON_MAJOR_APPLICATION) == 0 {
                    return Err(PowerError::Failed("ExitWindowsEx failed".into()));
                }
                Ok(())
            }
        })
        .await
        .map_err(|error| PowerError::Failed(error.to_string()))?
    }
}

#[cfg(target_os = "macos")]
mod imp {
    #![allow(unsafe_code)]

    use std::{ffi::c_void, ptr, sync::Mutex};

    use super::PowerError;

    type CFStringRef = *const c_void;
    type CFAllocatorRef = *const c_void;

    const K_CF_STRING_ENCODING_UTF8: u32 = 0x0800_0100;
    const K_IOPM_ASSERTION_LEVEL_ON: u32 = 255;
    const K_IO_RETURN_SUCCESS: i32 = 0;

    #[link(name = "CoreFoundation", kind = "framework")]
    unsafe extern "C" {
        fn CFStringCreateWithBytes(
            alloc: CFAllocatorRef,
            bytes: *const u8,
            num_bytes: isize,
            encoding: u32,
            is_external_representation: u8,
        ) -> CFStringRef;
        fn CFRelease(cf: *const c_void);
    }

    #[link(name = "IOKit", kind = "framework")]
    unsafe extern "C" {
        fn IOPMAssertionCreateWithName(
            assertion_type: CFStringRef,
            assertion_level: u32,
            assertion_name: CFStringRef,
            assertion_id: *mut u32,
        ) -> i32;
        fn IOPMAssertionRelease(assertion_id: u32) -> i32;
    }

    #[derive(Debug)]
    pub(super) struct InhibitorBackend {
        assertion_id: Mutex<Option<u32>>,
    }

    impl InhibitorBackend {
        pub(super) fn new() -> Self {
            Self {
                assertion_id: Mutex::new(None),
            }
        }

        pub(super) async fn set_inhibited(
            &self,
            inhibited: bool,
            reason: &str,
        ) -> Result<(), PowerError> {
            let mut guard = self
                .assertion_id
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            match (inhibited, *guard) {
                (true, Some(_)) | (false, None) => Ok(()),
                (false, Some(id)) => {
                    *guard = None;
                    // SAFETY: `id` was returned by `IOPMAssertionCreateWithName` and is released once.
                    let status = unsafe { IOPMAssertionRelease(id) };
                    if status == K_IO_RETURN_SUCCESS {
                        Ok(())
                    } else {
                        Err(PowerError::Failed(format!(
                            "IOPMAssertionRelease failed ({status})"
                        )))
                    }
                }
                (true, None) => {
                    let kind = "PreventUserIdleSystemSleep";
                    // SAFETY: Both slices have valid UTF-8 pointers/lengths, non-null CFStrings are
                    // released before returning, and `assertion_id` points to initialized stack memory.
                    unsafe {
                        let cf_type = CFStringCreateWithBytes(
                            ptr::null(),
                            kind.as_ptr(),
                            kind.len() as isize,
                            K_CF_STRING_ENCODING_UTF8,
                            0,
                        );
                        if cf_type.is_null() {
                            return Err(PowerError::Unavailable(
                                "CFStringCreateWithBytes returned null".into(),
                            ));
                        }
                        let cf_name = CFStringCreateWithBytes(
                            ptr::null(),
                            reason.as_ptr(),
                            reason.len() as isize,
                            K_CF_STRING_ENCODING_UTF8,
                            0,
                        );
                        if cf_name.is_null() {
                            CFRelease(cf_type);
                            return Err(PowerError::Unavailable(
                                "CFStringCreateWithBytes returned null".into(),
                            ));
                        }
                        let mut id: u32 = 0;
                        let status = IOPMAssertionCreateWithName(
                            cf_type,
                            K_IOPM_ASSERTION_LEVEL_ON,
                            cf_name,
                            &raw mut id,
                        );
                        CFRelease(cf_name);
                        CFRelease(cf_type);
                        if status == K_IO_RETURN_SUCCESS {
                            *guard = Some(id);
                            Ok(())
                        } else {
                            Err(PowerError::Failed(format!(
                                "IOPMAssertionCreateWithName failed ({status})"
                            )))
                        }
                    }
                }
            }
        }
    }

    impl Drop for InhibitorBackend {
        fn drop(&mut self) {
            if let Some(id) = self
                .assertion_id
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .take()
            {
                // SAFETY: Releases the active IOPMAssertion owned by this struct.
                unsafe {
                    let _ = IOPMAssertionRelease(id);
                }
            }
        }
    }

    pub(super) async fn sleep_system() -> Result<(), PowerError> {
        let status = tokio::process::Command::new("pmset")
            .arg("sleepnow")
            .status()
            .await
            .map_err(|error| PowerError::Unavailable(error.to_string()))?;
        if status.success() {
            Ok(())
        } else {
            Err(PowerError::Failed(format!(
                "pmset sleepnow exited with {status}"
            )))
        }
    }

    pub(super) async fn shutdown_system() -> Result<(), PowerError> {
        let status = tokio::process::Command::new("osascript")
            .args(["-e", "tell application \"System Events\" to shut down"])
            .status()
            .await
            .map_err(|error| PowerError::Unavailable(error.to_string()))?;
        if status.success() {
            Ok(())
        } else {
            Err(PowerError::Failed(format!(
                "osascript shutdown exited with {status}"
            )))
        }
    }
}

#[cfg(not(any(target_os = "linux", windows, target_os = "macos")))]
mod imp {
    use super::PowerError;

    #[derive(Debug)]
    pub(super) struct InhibitorBackend;

    impl InhibitorBackend {
        pub(super) fn new() -> Self {
            Self
        }

        pub(super) async fn set_inhibited(
            &self,
            _inhibited: bool,
            _reason: &str,
        ) -> Result<(), PowerError> {
            Ok(())
        }
    }

    pub(super) async fn sleep_system() -> Result<(), PowerError> {
        Err(PowerError::Unavailable(
            "sleep is not implemented on this platform".into(),
        ))
    }

    pub(super) async fn shutdown_system() -> Result<(), PowerError> {
        Err(PowerError::Unavailable(
            "shutdown is not implemented on this platform".into(),
        ))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn noop_inhibitor_is_idempotent() {
        let inhibitor = NoopSleepInhibitor;
        assert!(inhibitor.set_inhibited(true, "test").await.is_ok());
        assert!(inhibitor.set_inhibited(false, "test").await.is_ok());
    }

    #[tokio::test]
    async fn power_controller_nothing_and_notify_succeed_without_os_calls() {
        let controller = SystemPowerController::new();
        assert!(
            controller
                .execute(QueueCompletionAction::Nothing)
                .await
                .is_ok()
        );
        assert!(
            controller
                .execute(QueueCompletionAction::Notify)
                .await
                .is_ok()
        );
    }

    #[test]
    fn countdown_only_applies_to_power_transitions() {
        assert!(!QueueCompletionAction::Nothing.requires_countdown());
        assert!(!QueueCompletionAction::Notify.requires_countdown());
        assert!(QueueCompletionAction::Sleep.requires_countdown());
        assert!(QueueCompletionAction::Shutdown.requires_countdown());
    }
}
