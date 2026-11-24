use base64::Engine;
use base64::engine::general_purpose::STANDARD;
use windows::Win32::Foundation::{HLOCAL, LocalFree};
use windows::Win32::Security::Cryptography::{
    CRYPT_INTEGER_BLOB, CRYPTPROTECT_UI_FORBIDDEN, CryptProtectData, CryptUnprotectData,
};
use windows::core::PCWSTR;
use zeroize::{Zeroize, Zeroizing};

use crate::AppResult;

const ENTROPY: &[u8] = b"CampusNetAutoLogin/v1/CurrentUser";

pub fn encrypt_password(value: &str) -> AppResult<String> {
    if value.trim().is_empty() {
        return Err("密码不能为空。".to_string());
    }
    let mut plaintext = Zeroizing::new(value.as_bytes().to_vec());
    let plaintext_len = u32::try_from(plaintext.len())
        .map_err(|_| "密码过长，超出 Windows 数据加密支持的范围。".to_string())?;
    let mut entropy = Zeroizing::new(ENTROPY.to_vec());
    let entropy_len =
        u32::try_from(entropy.len()).map_err(|_| "加密附加参数长度超出限制。".to_string())?;
    let input = CRYPT_INTEGER_BLOB {
        cbData: plaintext_len,
        pbData: plaintext.as_mut_ptr(),
    };
    let optional_entropy = CRYPT_INTEGER_BLOB {
        cbData: entropy_len,
        pbData: entropy.as_mut_ptr(),
    };
    let mut output = CRYPT_INTEGER_BLOB::default();

    let result = unsafe {
        CryptProtectData(
            &input,
            PCWSTR::null(),
            Some(&optional_entropy),
            None,
            None,
            CRYPTPROTECT_UI_FORBIDDEN,
            &mut output,
        )
    };
    drop(plaintext);
    drop(entropy);
    result.map_err(|error| format!("Windows 无法加密密码：{error}"))?;

    let encoded = unsafe {
        let bytes = std::slice::from_raw_parts(output.pbData, output.cbData as usize);
        STANDARD.encode(bytes)
    };
    unsafe {
        LocalFree(Some(HLOCAL(output.pbData.cast())));
    }
    Ok(encoded)
}

pub fn decrypt_password(value: &str) -> AppResult<Zeroizing<String>> {
    let mut encrypted = Zeroizing::new(
        STANDARD
            .decode(value.trim())
            .map_err(|_| "已保存的密码格式无效，请重新输入。".to_string())?,
    );
    let encrypted_len = u32::try_from(encrypted.len())
        .map_err(|_| "已保存的加密密码过长，无法解密。".to_string())?;
    let mut entropy = Zeroizing::new(ENTROPY.to_vec());
    let entropy_len =
        u32::try_from(entropy.len()).map_err(|_| "加密附加参数长度超出限制。".to_string())?;
    let input = CRYPT_INTEGER_BLOB {
        cbData: encrypted_len,
        pbData: encrypted.as_mut_ptr(),
    };
    let optional_entropy = CRYPT_INTEGER_BLOB {
        cbData: entropy_len,
        pbData: entropy.as_mut_ptr(),
    };
    let mut output = CRYPT_INTEGER_BLOB::default();
    let result = unsafe {
        CryptUnprotectData(
            &input,
            None,
            Some(&optional_entropy),
            None,
            None,
            CRYPTPROTECT_UI_FORBIDDEN,
            &mut output,
        )
    };
    drop(encrypted);
    drop(entropy);
    result.map_err(|_| "已保存的密码无法由当前 Windows 用户解密，请重新输入。".to_string())?;

    let plaintext =
        unsafe { std::slice::from_raw_parts(output.pbData, output.cbData as usize).to_vec() };
    unsafe {
        std::ptr::write_bytes(output.pbData, 0, output.cbData as usize);
        LocalFree(Some(HLOCAL(output.pbData.cast())));
    }
    let text = match String::from_utf8(plaintext) {
        Ok(text) => text,
        Err(error) => {
            let mut bytes = error.into_bytes();
            bytes.zeroize();
            return Err("已保存的密码不是有效的 UTF-8 文本。".to_string());
        }
    };
    Ok(Zeroizing::new(text))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn dpapi_round_trip_uses_current_user() {
        let encrypted = encrypt_password("复杂 密码!@#").unwrap();
        assert_ne!(encrypted, "复杂 密码!@#");
        assert_eq!(
            decrypt_password(&encrypted).unwrap().as_str(),
            "复杂 密码!@#"
        );
        assert!(decrypt_password("not-base64").is_err());
    }
}
