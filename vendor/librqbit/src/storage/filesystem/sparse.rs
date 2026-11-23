#[cfg(windows)]
pub fn mark_file_sparse(f: &std::fs::File) -> bool {
    use std::os::windows::io::AsRawHandle;
    use windows::{
        Win32::Foundation::HANDLE, Win32::System::IO::DeviceIoControl,
        Win32::System::Ioctl::FSCTL_SET_SPARSE,
    };

    let handle = HANDLE(f.as_raw_handle());

    unsafe { DeviceIoControl(handle, FSCTL_SET_SPARSE, None, 0, None, 0, None, None).is_ok() }
}

#[cfg(windows)]
pub(super) fn query_allocated_ranges(
    f: &std::fs::File,
) -> Option<crate::storage::SparseFileLayout> {
    use std::{
        mem::{size_of, size_of_val},
        os::windows::{fs::MetadataExt, io::AsRawHandle},
    };
    use windows::Win32::{
        Foundation::HANDLE,
        System::{
            IO::DeviceIoControl,
            Ioctl::{FILE_ALLOCATED_RANGE_BUFFER, FSCTL_QUERY_ALLOCATED_RANGES},
        },
    };

    const FILE_ATTRIBUTE_SPARSE_FILE: u32 = 0x200;
    let metadata = f.metadata().ok()?;
    if metadata.file_attributes() & FILE_ATTRIBUTE_SPARSE_FILE == 0 || metadata.len() == 0 {
        return None;
    }

    let input = FILE_ALLOCATED_RANGE_BUFFER {
        FileOffset: 0,
        Length: metadata.len().try_into().ok()?,
    };
    let mut ranges = [FILE_ALLOCATED_RANGE_BUFFER::default(); 1024];
    let mut written = 0u32;
    // SAFETY: The file handle and aligned input and output buffers stay live until the
    // synchronous IOCTL returns. Windows receives the exact sizes of those buffers.
    unsafe {
        DeviceIoControl(
            HANDLE(f.as_raw_handle()),
            FSCTL_QUERY_ALLOCATED_RANGES,
            Some((&raw const input).cast()),
            size_of_val(&input) as u32,
            Some(ranges.as_mut_ptr().cast()),
            size_of_val(&ranges) as u32,
            Some(&mut written),
            None,
        )
    }
    .ok()?; // A truncated or unsupported allocation map cannot identify holes safely.

    let entry_size = size_of::<FILE_ALLOCATED_RANGE_BUFFER>();
    if !(written as usize).is_multiple_of(entry_size) {
        return None;
    }
    let ranges = ranges.get(..written as usize / entry_size)?;
    let mut allocated = Vec::with_capacity(ranges.len());
    let mut previous_end = 0;
    for range in ranges {
        let start = u64::try_from(range.FileOffset).ok()?;
        let end = start
            .checked_add(u64::try_from(range.Length).ok()?)?
            .min(metadata.len());
        if start < previous_end || start >= end {
            return None;
        }
        allocated.push(start..end);
        previous_end = end;
    }
    Some(crate::storage::SparseFileLayout {
        len: metadata.len(),
        allocated,
    })
}
