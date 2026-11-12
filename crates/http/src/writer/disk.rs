use std::{fs::File, io, path::Path};

#[cfg(unix)]
use std::os::unix::fs::FileExt;
#[cfg(windows)]
use std::os::windows::fs::FileExt;

pub(super) fn prepare(file: &File, size: u64) -> io::Result<()> {
    #[cfg(windows)]
    sparse(file)?;
    file.set_len(size)
}

pub(super) fn write_at(file: &File, mut bytes: &[u8], mut offset: u64) -> io::Result<()> {
    while !bytes.is_empty() {
        #[cfg(unix)]
        let written = file.write_at(bytes, offset);
        #[cfg(windows)]
        let written = file.seek_write(bytes, offset);
        match written {
            Ok(0) => return Err(io::ErrorKind::WriteZero.into()),
            Ok(count) => {
                bytes = &bytes[count..];
                offset += count as u64;
            }
            Err(error) if error.kind() == io::ErrorKind::Interrupted => {}
            Err(error) => return Err(error),
        }
    }
    Ok(())
}

#[cfg(unix)]
pub(super) fn sync_directory(directory: &Path) -> io::Result<()> {
    File::open(directory)?.sync_all()
}

#[cfg(windows)]
pub(super) fn sync_directory(_directory: &Path) -> io::Result<()> {
    // Windows does not expose a portable directory fsync.
    Ok(())
}

#[cfg(windows)]
#[allow(unsafe_code)] // The Windows standard library has no sparse-file operation.
fn sparse(file: &File) -> io::Result<()> {
    use std::{os::windows::io::AsRawHandle, ptr};
    use windows_sys::Win32::{
        Foundation::{ERROR_INVALID_FUNCTION, ERROR_NOT_SUPPORTED},
        System::{IO::DeviceIoControl, Ioctl::FSCTL_SET_SPARSE},
    };

    let mut returned = 0;
    // SAFETY: The handle is valid for the synchronous call. FSCTL_SET_SPARSE
    // accepts null input and output buffers when marking the entire file sparse.
    let result = unsafe {
        DeviceIoControl(
            file.as_raw_handle(),
            FSCTL_SET_SPARSE,
            ptr::null(),
            0,
            ptr::null_mut(),
            0,
            &mut returned,
            ptr::null_mut(),
        )
    };
    if result != 0 {
        return Ok(());
    }
    let error = io::Error::last_os_error();
    if error.raw_os_error().is_some_and(|code| {
        code == ERROR_INVALID_FUNCTION as i32 || code == ERROR_NOT_SUPPORTED as i32
    }) {
        return Ok(());
    }
    Err(error)
}
