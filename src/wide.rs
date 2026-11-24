use std::ffi::OsStr;
use std::os::windows::ffi::OsStrExt;

use windows::core::PCWSTR;
use zeroize::Zeroizing;

pub fn to_wide(value: impl AsRef<OsStr>) -> Vec<u16> {
    value
        .as_ref()
        .encode_wide()
        .chain(std::iter::once(0))
        .collect()
}

pub fn to_wide_zeroizing(value: impl AsRef<OsStr>) -> Zeroizing<Vec<u16>> {
    Zeroizing::new(to_wide(value))
}

pub fn pcwstr(value: &[u16]) -> PCWSTR {
    PCWSTR(value.as_ptr())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn converts_strings_to_null_terminated_utf16_and_zeroizing() {
        assert_eq!(to_wide(""), vec![0]);
        assert_eq!(to_wide("abc"), vec![b'a' as u16, b'b' as u16, b'c' as u16, 0]);

        let chinese: Vec<u16> = "校园网".encode_utf16().chain(std::iter::once(0)).collect();
        assert_eq!(to_wide("校园网"), chinese);

        let secret = to_wide_zeroizing("secret123");
        assert_eq!(secret.as_slice(), to_wide("secret123").as_slice());
        assert_eq!(secret.last(), Some(&0));
        assert_eq!(pcwstr(&secret).0, secret.as_ptr());
    }
}
