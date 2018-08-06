extern crate winapi;

use std::cmp;
use std::fs;
use std::io;
use std::mem;
use std::os::windows::fs::MetadataExt;
use std::os::windows::io::AsRawHandle;
use std::path::Path;
use std::ptr;

use winapi::um::winioctl::FSCTL_SET_SPARSE;
use winapi::um::winnt::FILE_ATTRIBUTE_SPARSE_FILE;

pub fn reflink<P: AsRef<Path>, Q: AsRef<Path>>(from: P, to: Q) -> io::Result<()> {
    // Inspired by https://github.com/0xbadfca11/reflink/blob/master/reflink.cpp
    let src = fs::File::open(&from)?;
    let src_metadata = src.metadata()?;

    let src_file_size = src_metadata.file_size();
    let src_is_sparse = src_metadata.file_attributes() & FILE_ATTRIBUTE_SPARSE_FILE > 0;
    let src_integrity_info = src.get_integrity_information()?;

    let dest = fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&to)?;

    if src_is_sparse {
        try_cleanup!(dest.set_sparse(), to);
    }

    // Copy over integrity information. Not sure if this is required.
    let dest_integrity_info = ffi::FSCTL_SET_INTEGRITY_INFORMATION {
        ChecksumAlgorithm: src_integrity_info.ChecksumAlgorithm,
        Reserved: src_integrity_info.Reserved,
        Flags: src_integrity_info.Flags,
    };
    try_cleanup!(dest.set_integrity_information(&dest_integrity_info), to);

    // file_size must be sufficient to hold the data.
    try_cleanup!(dest.set_len(src_file_size), to);

    // Preparation done, now reflink
    let split_threshold: u32 = 0u32.wrapping_sub(src_integrity_info.ClusterSizeInBytes);
    let mut dup_extent: ffi::DUPLICATE_EXTENTS_DATA = unsafe { mem::uninitialized() };
    dup_extents_data.FileHandle = src.as_raw_handle();
    let mut remain = round_up(src_file_size, src_integrity_info.ClusterSizeInBytes);
    let mut offset = 0;
    while remain > 0 {
        *dup_extent.SourceFileOffset.QuadPart_mut() = offset;
        *dup_extent.TargetFileOffset.QuadPart_mut() = offset;
        *dup_extent.ByteCount.QuadPart_mut() = cmp::min(split_threshold as i64, remain);
        let mut bytes_returned = 0;

        let res = unsafe {
            DeviceIoControl(
                dest.as_raw_handle(),
                ffi::FSCTL_DUPLICATE_EXTENTS_TO_FILE,
                &mut dup_extent as *mut _,
                mem::size_of::<ffi::DUPLICATE_EXTENTS_DATA>(),
                ptr::null_mut(),
                0,
                &mut bytes_returned as *mut _,
                ptr::null_mut(),
            )
        };
        if res != 0 {
            let _ = fs::remove_file(to);
            return Err(io::Error::last_os_error());
        }
        remain -= split_threshold;
        offset += split_threshold;
    }
    Ok(())
}

/// Additional functionality for windows files, needed for reflink
trait FileExt {
    fn set_sparse(&self) -> io::Result<()>;
    fn get_integrity_information(&self) -> io::Result<ffi::FSCTL_GET_INTEGRITY_INFORMATION_BUFFER>;
    fn set_integrity_information(
        &self,
        integrity_info: &ffi::FSCTL_SET_INTEGRITY_INFORMATION_BUFFER,
    ) -> io::Result<()>;
}

impl FileExt for fs::File {
    fn set_sparse(&self) -> io::Result<()> {
        let mut bytes_returned = 0;
        let res = unsafe {
            DeviceIoControl(
                self.as_raw_handle(),
                FSCTL_SET_SPARSE,
                ptr::null_mut(),
                0,
                ptr::null_mut(),
                0,
                &mut bytes_returned as *mut _,
                ptr::null_mut(),
            )
        };
        if res != 0 {
            Err(io::Error::last_os_error())
        } else {
            Ok(())
        }
    }

    fn get_integrity_information(&self) -> io::Result<ffi::FSCTL_GET_INTEGRITY_INFORMATION_BUFFER> {
        let mut bytes_returned = 0;
        unsafe {
            let mut integrity_info: ffi::FSCTL_GET_INTEGRITY_INFORMATION_BUFFER = mem::zeroed();
            let res = DeviceIoControl(
                self.as_raw_handle(),
                FSCTL_GET_INTEGRITY_INFORMATION,
                ptr::null_mut(),
                0,
                &mut integrity_info as *mut _,
                mem::size_of::<ffi::FSCTL_GET_INTEGRITY_INFORMATION_BUFFER>(),
                &mut bytes_returned as *mut _,
                ptr::null_mut(),
            );
            if res != 0 {
                Err(io::Error::last_os_error())
            } else {
                Ok(integrity_info)
            }
        }
    }

    fn set_integrity_information(
        &self,
        integrity_info: &ffi::FSCTL_SET_INTEGRITY_INFORMATION_BUFFER,
    ) -> io::Result<()> {
        let res = unsafe {
            DeviceIoControl(
                self.as_raw_handle(),
                FSCTL_SET_INTEGRITY_INFORMATION,
                integrity_info as *const _,
                mem::size_of::<ffi::FSCTL_SET_INTEGRITY_INFORMATION_BUFFER>(),
                ptr::null_mut(),
                0,
                ptr::null_mut(),
                ptr::null_mut(),
            )
        };
        if res != 0 {
            Err(io::Error::last_os_error())
        } else {
            Ok(())
        }
    }
}

macro_rules! try_cleanup {
    ($expr:expr, $dest:ident) => {
        match $expr {
            Ok(()) => {}
            Err(err) => {
                let _ = fs::remove_file($dest);
                return Err(err);
            }
        }
    };
}

fn round_up(number: i64, num_digits: u32) -> i64 {
    let num_digits = num_digits as i64;
    (number + num_digits - 1) / num_digits * num_digits
}

/// Contains definitions not included in winapi
mod ffi {
    use std::os::windows::raw::HANDLE;
    use winapi::shared::minwindef::{DWORD, WORD};
    use winapi::um::winnt::LARGE_INTEGER;

    pub const FSCTL_DUPLICATE_EXTENTS_TO_FILE: u32 = 0x98344;

    #[repr(C)]
    pub struct FSCTL_GET_INTEGRITY_INFORMATION_BUFFER {
        pub ChecksumAlgorithm: WORD,
        pub Reserved: WORD,
        pub Flags: DWORD,
        pub ChecksumChunkSizeInBytes: DWORD,
        pub ClusterSizeInBytes: DWORD,
    }

    #[repr(C)]
    pub struct FSCTL_SET_INTEGRITY_INFORMATION_BUFFER {
        pub ChecksumAlgorithm: WORD,
        pub Reserved: WORD,
        pub Flags: DWORD,
    }

    #[repr(C)]
    pub struct DUPLICATE_EXTENTS_DATA {
        pub FileHandle: HANDLE,
        pub SourceFileOffset: LARGE_INTEGER,
        pub TargetFileOffset: LARGE_INTEGER,
        pub ByteCount: LARGE_INTEGER,
    }
}
