use std::mem::size_of;
use std::path::PathBuf;

use windows::Win32::Foundation::{ERROR_FILE_NOT_FOUND, ERROR_PATH_NOT_FOUND, WIN32_ERROR};
use windows::Win32::System::Registry::{
    HKEY_CURRENT_USER, REG_SZ, RRF_RT_REG_SZ, RegDeleteKeyValueW, RegGetValueW, RegSetKeyValueW,
};

use crate::AppResult;
use crate::wide::{pcwstr, to_wide};

const RUN_SUBKEY: &str = r"Software\Microsoft\Windows\CurrentVersion\Run";
const RUN_VALUE_NAME: &str = "CampusNetAutoLogin";
const MAX_RUN_VALUE_CHARS: usize = 32_768;

pub struct StartupManager {
    executable_path: PathBuf,
    value_name: String,
}

impl StartupManager {
    pub fn new(executable_path: PathBuf) -> AppResult<Self> {
        if executable_path.as_os_str().is_empty() {
            return Err("无法确定程序路径。".to_owned());
        }
        Ok(Self {
            executable_path,
            value_name: RUN_VALUE_NAME.to_owned(),
        })
    }

    pub fn is_enabled(&self) -> bool {
        self.is_enabled_inner().unwrap_or(false)
    }

    pub fn enable(&self) -> AppResult<()> {
        let subkey = to_wide(RUN_SUBKEY);
        let value_name = to_wide(&self.value_name);
        let command = to_wide(self.command_line());
        let byte_count = command
            .len()
            .checked_mul(size_of::<u16>())
            .and_then(|value| u32::try_from(value).ok())
            .ok_or_else(|| "开机自启命令过长。".to_owned())?;

        let result = unsafe {
            RegSetKeyValueW(
                HKEY_CURRENT_USER,
                pcwstr(&subkey),
                pcwstr(&value_name),
                REG_SZ.0,
                Some(command.as_ptr().cast()),
                byte_count,
            )
        };
        registry_result(result, "无法写入当前用户开机启动项")
    }

    pub fn disable(&self) -> AppResult<()> {
        let subkey = to_wide(RUN_SUBKEY);
        let value_name = to_wide(&self.value_name);
        let result =
            unsafe { RegDeleteKeyValueW(HKEY_CURRENT_USER, pcwstr(&subkey), pcwstr(&value_name)) };
        if is_not_found(result) {
            return Ok(());
        }
        registry_result(result, "无法删除当前用户开机启动项")
    }

    fn is_enabled_inner(&self) -> AppResult<bool> {
        let Some(command) = self.read_command()? else {
            return Ok(false);
        };
        Ok(command.eq_ignore_ascii_case(&self.command_line()))
    }

    fn read_command(&self) -> AppResult<Option<String>> {
        let subkey = to_wide(RUN_SUBKEY);
        let value_name = to_wide(&self.value_name);
        let mut buffer = vec![0u16; MAX_RUN_VALUE_CHARS];
        let mut byte_count = u32::try_from(buffer.len() * size_of::<u16>())
            .map_err(|_| "无法分配开机启动项读取缓冲区。".to_owned())?;
        let result = unsafe {
            RegGetValueW(
                HKEY_CURRENT_USER,
                pcwstr(&subkey),
                pcwstr(&value_name),
                RRF_RT_REG_SZ,
                None,
                Some(buffer.as_mut_ptr().cast()),
                Some(&mut byte_count),
            )
        };
        if is_not_found(result) {
            return Ok(None);
        }
        registry_result(result, "无法读取当前用户开机启动项")?;

        let char_count = (byte_count as usize / size_of::<u16>()).min(buffer.len());
        let text_length = buffer[..char_count]
            .iter()
            .position(|value| *value == 0)
            .unwrap_or(char_count);
        Ok(Some(String::from_utf16_lossy(&buffer[..text_length])))
    }

    fn command_line(&self) -> String {
        format!("\"{}\" --autostart", self.executable_path.to_string_lossy())
    }
}

fn is_not_found(result: WIN32_ERROR) -> bool {
    matches!(result, ERROR_FILE_NOT_FOUND | ERROR_PATH_NOT_FOUND)
}

fn registry_result(result: WIN32_ERROR, context: &str) -> AppResult<()> {
    result.ok().map_err(|error| format!("{context}：{error}"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::env;
    use std::time::{SystemTime, UNIX_EPOCH};

    struct RegistryCleanup<'a>(&'a StartupManager);

    impl Drop for RegistryCleanup<'_> {
        fn drop(&mut self) {
            let _ = self.0.disable();
        }
    }

    #[test]
    fn run_command_quotes_executable_and_keeps_autostart_argument() {
        let manager = StartupManager {
            executable_path: PathBuf::from(r"C:\Program Files\校园网\CampusNetAutoLogin.exe"),
            value_name: "Test".to_owned(),
        };
        assert_eq!(
            manager.command_line(),
            r#""C:\Program Files\校园网\CampusNetAutoLogin.exe" --autostart"#
        );
    }

    #[test]
    #[ignore = "temporarily writes a uniquely named HKCU Run value and removes it"]
    fn registry_run_round_trip() {
        let id = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let manager = StartupManager {
            executable_path: env::current_exe().unwrap(),
            value_name: format!("CampusNetAutoLogin-IntegrationTest-{id}"),
        };
        let _cleanup = RegistryCleanup(&manager);

        manager.enable().unwrap();
        assert_eq!(
            manager.read_command().unwrap().as_deref(),
            Some(manager.command_line().as_str())
        );
        assert!(manager.is_enabled());
        manager.disable().unwrap();
        assert!(!manager.is_enabled());
    }
}
