use std::ffi::OsStr;
use std::os::windows::ffi::OsStrExt;

use windows::core::{PCWSTR, PWSTR};

pub fn to_wide(value: impl AsRef<OsStr>) -> Vec<u16> {
    value
        .as_ref()
        .encode_wide()
        .chain(std::iter::once(0))
        .collect()
}

/// Converts a null-terminated UTF-16 pointer into an owned string.
///
/// # Safety
///
/// `value` must be null or point to a readable, null-terminated UTF-16 buffer
/// for the duration of this call.
pub unsafe fn pwstr_to_string(value: PWSTR) -> String {
    if value.is_null() {
        return String::new();
    }

    let mut length = 0usize;
    unsafe {
        while *value.0.add(length) != 0 {
            length += 1;
        }
        String::from_utf16_lossy(std::slice::from_raw_parts(value.0, length))
    }
}

pub fn pcwstr(value: &[u16]) -> PCWSTR {
    PCWSTR(value.as_ptr())
}
