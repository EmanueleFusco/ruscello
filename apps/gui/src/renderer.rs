//! Configure the process-local Windows graphics environment before Iced starts.

use std::io;

use windows_sys::Win32::System::Environment::SetEnvironmentVariableW;

use crate::preferences::Renderer;

pub(crate) fn configure(renderer: Renderer) -> io::Result<()> {
    set("ICED_BACKEND", renderer.backend())?;
    if renderer == Renderer::Hardware {
        set("ICED_PRESENT_MODE", "fifo")?;
        if std::env::var_os("WGPU_BACKEND").is_none() {
            set("WGPU_BACKEND", "dx12")?;
        }
    }
    Ok(())
}

#[expect(
    unsafe_code,
    reason = "Win32 exposes the process-local environment through OS FFI"
)]
fn set(name: &str, value: &str) -> io::Result<()> {
    let name: Vec<u16> = name.encode_utf16().chain(std::iter::once(0)).collect();
    let value: Vec<u16> = value.encode_utf16().chain(std::iter::once(0)).collect();
    // SAFETY: Both UTF-16 buffers are live and NUL-terminated for the Win32 call.
    // The function updates only the environment of the current process.
    if unsafe { SetEnvironmentVariableW(name.as_ptr(), value.as_ptr()) } == 0 {
        return Err(io::Error::last_os_error());
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn configures_the_saved_renderer_and_keeps_an_explicit_adapter_backend() -> io::Result<()> {
        for (renderer, backend) in [
            ("hardware", "dx12"),
            ("hardware", "vulkan"),
            ("software", "vulkan"),
        ] {
            let mut command = std::process::Command::new(std::env::current_exe()?);
            command
                .args([
                    "--ignored",
                    "--exact",
                    "renderer::tests::process_environment_fixture",
                ])
                .env("RUSCELLO_TEST_RENDERER", renderer)
                .env("RUSCELLO_TEST_EXPECTED_WGPU_BACKEND", backend)
                .env("ICED_BACKEND", "old-backend")
                .env("ICED_PRESENT_MODE", "old-present-mode")
                .env_remove("WGPU_BACKEND");
            if backend == "vulkan" {
                command.env("WGPU_BACKEND", backend);
            }
            let output = command.output()?;
            assert!(
                output.status.success(),
                "Renderer configuration failed for {renderer} with {backend}: {}",
                String::from_utf8_lossy(&output.stdout)
            );
        }
        Ok(())
    }

    #[test]
    #[ignore = "subprocess fixture for the Windows renderer environment test"]
    fn process_environment_fixture() -> io::Result<()> {
        let gpu = std::env::var("RUSCELLO_TEST_RENDERER").as_deref() == Ok("hardware");
        let renderer = if gpu {
            Renderer::Hardware
        } else {
            Renderer::Software
        };
        configure(renderer)?;
        assert_eq!(
            std::env::var("ICED_BACKEND").as_deref(),
            Ok(renderer.backend())
        );
        assert_eq!(
            std::env::var("ICED_PRESENT_MODE").as_deref(),
            Ok(if gpu { "fifo" } else { "old-present-mode" })
        );
        assert_eq!(
            std::env::var_os("WGPU_BACKEND"),
            std::env::var_os("RUSCELLO_TEST_EXPECTED_WGPU_BACKEND")
        );
        Ok(())
    }
}
