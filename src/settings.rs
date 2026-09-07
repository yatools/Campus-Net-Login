use std::env;
use std::fs::{self, OpenOptions};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::Mutex;

use serde::{Deserialize, Serialize};
use windows::Win32::Storage::FileSystem::{
    MOVEFILE_REPLACE_EXISTING, MOVEFILE_WRITE_THROUGH, MoveFileExW, REPLACEFILE_WRITE_THROUGH,
    ReplaceFileW,
};

use crate::AppResult;
use crate::wide::{pcwstr, to_wide};

pub const PROJECT_URL: &str = "https://github.com/yatools/CampusNetAutoLogin";

pub const APP_NAME: &str = "CampusNetAutoLogin";
pub const DEFAULT_FALLBACK_PROBE_URL: &str = "https://www.baidu.com/favicon.ico";

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub enum Provider {
    #[default]
    Campus,
    ChinaMobile,
    ChinaUnicom,
    ChinaTelecom,
}

impl Provider {
    pub const ALL: [Provider; 4] = [
        Provider::Campus,
        Provider::ChinaMobile,
        Provider::ChinaUnicom,
        Provider::ChinaTelecom,
    ];

    pub fn display_name(self) -> &'static str {
        match self {
            Provider::Campus => "校园网",
            Provider::ChinaMobile => "中国移动",
            Provider::ChinaUnicom => "中国联通",
            Provider::ChinaTelecom => "中国电信",
        }
    }

    pub fn suffix(self) -> &'static str {
        match self {
            Provider::Campus => "",
            Provider::ChinaMobile => "@cmcc",
            Provider::ChinaUnicom => "@unicom",
            Provider::ChinaTelecom => "@telecom",
        }
    }

    pub fn submitted_account(self, account: &str) -> String {
        format!("{}{}", account.trim(), self.suffix())
    }

    pub fn index(self) -> usize {
        Provider::ALL
            .iter()
            .position(|item| *item == self)
            .unwrap_or(0)
    }

    pub fn from_index(index: usize) -> Provider {
        Provider::ALL.get(index).copied().unwrap_or_default()
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct AppSettings {
    pub schema_version: i32,
    pub account: String,
    pub provider: Provider,
    pub encrypted_password: String,
    pub check_interval_seconds: i32,
    pub failure_cooldown_seconds: i32,
    pub fallback_probe_url: String,
    pub auto_start: bool,
    #[serde(alias = "successNotification")]
    pub notifications_enabled: bool,
    pub autostart_login_once: bool,
    pub pause_periods: String,
    pub last_successful_login_utc: Option<String>,
}

impl Default for AppSettings {
    fn default() -> Self {
        Self {
            schema_version: 2,
            account: String::new(),
            provider: Provider::Campus,
            encrypted_password: String::new(),
            check_interval_seconds: 1800,
            failure_cooldown_seconds: 20,
            fallback_probe_url: DEFAULT_FALLBACK_PROBE_URL.to_string(),
            auto_start: false,
            notifications_enabled: true,
            autostart_login_once: false,
            pause_periods: String::new(),
            last_successful_login_utc: None,
        }
    }
}

impl AppSettings {
    pub fn normalize(mut self) -> Self {
        if self.schema_version < 2 && self.failure_cooldown_seconds == 0 {
            self.failure_cooldown_seconds = -1;
        }
        self.schema_version = 2;
        self.account = self.account.trim().to_string();
        self.check_interval_seconds = self.check_interval_seconds.clamp(10, 3600);
        self.failure_cooldown_seconds = self.failure_cooldown_seconds.clamp(-1, 3600);
        self.fallback_probe_url = self.fallback_probe_url.trim().to_string();
        if !is_valid_fallback_url(&self.fallback_probe_url) {
            self.fallback_probe_url = DEFAULT_FALLBACK_PROBE_URL.to_string();
        }
        self
    }

    pub fn has_credentials(&self) -> bool {
        !self.account.trim().is_empty() && !self.encrypted_password.trim().is_empty()
    }

    pub fn validate(&self, require_password: bool) -> Vec<String> {
        let mut errors = Vec::new();
        if self.account.trim().is_empty() {
            errors.push("请输入学号或账号。".to_string());
        } else if self.account.contains('@') {
            errors.push("账号中不要填写运营商后缀，请通过服务商下拉框选择。".to_string());
        }
        if require_password && self.encrypted_password.trim().is_empty() {
            errors.push("首次保存时必须输入密码。".to_string());
        }
        if !(10..=3600).contains(&self.check_interval_seconds) {
            errors.push("检测间隔必须在 10 到 3600 秒之间。".to_string());
        }
        if !(-1..=3600).contains(&self.failure_cooldown_seconds) {
            errors.push("失败冷却时间必须在 -1 到 3600 秒之间。".to_string());
        }
        if parse_pause_periods(&self.pause_periods).is_err() {
            errors.push(
                "暂停时段格式应为 HH:MM-HH:MM，多个时段用分号分隔，起止时间不能相同。".to_string(),
            );
        }
        if !is_valid_fallback_url(&self.fallback_probe_url) {
            errors.push("国内备用探测地址必须是有效的 HTTPS 地址。".to_string());
        }
        errors
    }
}

/// Daily local-time intervals, start inclusive and end exclusive.
pub fn parse_pause_periods(value: &str) -> Result<Vec<(u16, u16)>, ()> {
    fn minute(value: &str) -> Result<u16, ()> {
        let value = value.trim();
        let b = value.as_bytes();
        if b.len() != 5 || b[2] != b':' || ![b[0], b[1], b[3], b[4]].iter().all(u8::is_ascii_digit)
        {
            return Err(());
        }
        let h = value[..2].parse::<u16>().map_err(|_| ())?;
        let m = value[3..].parse::<u16>().map_err(|_| ())?;
        if h > 23 || m > 59 {
            return Err(());
        }
        Ok(h * 60 + m)
    }
    if value.trim().is_empty() {
        return Ok(Vec::new());
    }
    value
        .split([';', '；'])
        .map(|part| {
            let (start, end) = part.trim().split_once('-').ok_or(())?;
            let (start, end) = (minute(start)?, minute(end)?);
            if start == end {
                return Err(());
            }
            Ok((start, end))
        })
        .collect()
}

pub fn pause_remaining_seconds(periods: &str, second: u32) -> Option<u64> {
    parse_pause_periods(periods)
        .ok()?
        .into_iter()
        .filter_map(|(start, end)| {
            let (start, end) = (u32::from(start) * 60, u32::from(end) * 60);
            let active = if start < end {
                second >= start && second < end
            } else {
                second >= start || second < end
            };
            active.then(|| u64::from((end + 86400 - second) % 86400))
        })
        .max()
}

pub fn is_valid_fallback_url(value: &str) -> bool {
    let value = value.trim();
    let bytes = value.as_bytes();
    if bytes.len() < 8 || !bytes[..8].eq_ignore_ascii_case(b"https://") {
        return false;
    }
    let rest = &value[8..];
    if rest.is_empty() || rest.starts_with('/') || rest.contains('@') || rest.contains('#') {
        return false;
    }
    let authority_end = rest.find(['/', '?']).unwrap_or(rest.len());
    let authority = &rest[..authority_end];
    valid_authority(authority)
}

fn valid_authority(authority: &str) -> bool {
    if authority.is_empty() || authority.chars().any(char::is_whitespace) {
        return false;
    }
    let (host, port) = match authority.rsplit_once(':') {
        Some((host, port)) => (host, Some(port)),
        None => (authority, None),
    };
    if host.is_empty()
        || !host
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'-'))
    {
        return false;
    }
    match port {
        Some(value) => value.parse::<u16>().map(|port| port != 0).unwrap_or(false),
        None => true,
    }
}

#[derive(Clone, Debug)]
pub struct SettingsStore {
    path: PathBuf,
}

impl SettingsStore {
    pub fn system_default() -> AppResult<Self> {
        let local_app_data = env::var_os("LOCALAPPDATA")
            .ok_or_else(|| "无法确定当前用户的 LocalAppData 目录。".to_string())?;
        Ok(Self::new(
            PathBuf::from(local_app_data)
                .join(APP_NAME)
                .join("settings.json"),
        ))
    }

    pub fn new(path: PathBuf) -> Self {
        Self { path }
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    pub fn data_directory(&self) -> &Path {
        self.path.parent().unwrap_or_else(|| Path::new("."))
    }

    pub fn load(&self) -> AppSettings {
        let Ok(bytes) = fs::read(&self.path) else {
            return AppSettings::default();
        };
        serde_json::from_slice::<AppSettings>(&bytes)
            .unwrap_or_default()
            .normalize()
    }

    pub fn save(&self, settings: &AppSettings) -> AppResult<()> {
        let directory = self
            .path
            .parent()
            .ok_or_else(|| "配置文件路径无效。".to_string())?;
        fs::create_dir_all(directory).map_err(|error| format!("无法创建配置目录：{error}"))?;

        let temporary = self.path.with_extension("json.tmp");
        let payload = serde_json::to_vec_pretty(&settings.clone().normalize())
            .map_err(|error| format!("无法序列化配置：{error}"))?;
        let mut file = OpenOptions::new()
            .create(true)
            .truncate(true)
            .write(true)
            .open(&temporary)
            .map_err(|error| format!("无法创建临时配置：{error}"))?;
        file.write_all(&payload)
            .and_then(|_| file.write_all(b"\n"))
            .and_then(|_| file.sync_all())
            .map_err(|error| format!("无法写入配置：{error}"))?;
        drop(file);

        let destination_wide = to_wide(&self.path);
        let temporary_wide = to_wide(&temporary);
        let result = unsafe {
            if self.path.exists() {
                ReplaceFileW(
                    pcwstr(&destination_wide),
                    pcwstr(&temporary_wide),
                    None,
                    REPLACEFILE_WRITE_THROUGH,
                    None,
                    None,
                )
            } else {
                MoveFileExW(
                    pcwstr(&temporary_wide),
                    pcwstr(&destination_wide),
                    MOVEFILE_REPLACE_EXISTING | MOVEFILE_WRITE_THROUGH,
                )
            }
        };
        if let Err(error) = result {
            let _ = fs::remove_file(&temporary);
            return Err(format!("无法原子替换配置：{error}"));
        }
        Ok(())
    }

    pub fn clear(&self) -> AppResult<()> {
        if self.path.exists() {
            fs::remove_file(&self.path).map_err(|error| format!("无法删除配置：{error}"))?;
        }
        let temporary = self.path.with_extension("json.tmp");
        let _ = fs::remove_file(temporary);
        Ok(())
    }
}

pub struct SettingsManager {
    store: SettingsStore,
    current: Mutex<AppSettings>,
}

impl SettingsManager {
    pub fn new(store: SettingsStore) -> Self {
        let current = store.load();
        Self {
            store,
            current: Mutex::new(current),
        }
    }

    pub fn snapshot(&self) -> AppSettings {
        self.current
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .clone()
    }

    pub fn replace(&self, settings: AppSettings) -> AppResult<AppSettings> {
        let normalized = settings.normalize();
        let errors = normalized.validate(false);
        if !errors.is_empty() {
            return Err(errors.join("\n"));
        }
        let mut guard = self
            .current
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        self.store.save(&normalized)?;
        *guard = normalized.clone();
        Ok(normalized)
    }

    pub fn update(&self, update: impl FnOnce(&mut AppSettings)) -> AppResult<AppSettings> {
        let mut guard = self
            .current
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let mut candidate = guard.clone();
        update(&mut candidate);
        candidate = candidate.normalize();
        let errors = candidate.validate(false);
        if !errors.is_empty() {
            return Err(errors.join("\n"));
        }
        self.store.save(&candidate)?;
        *guard = candidate.clone();
        Ok(candidate)
    }

    pub fn clear(&self) -> AppResult<()> {
        let mut guard = self
            .current
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        self.store.clear()?;
        *guard = AppSettings::default();
        Ok(())
    }

    pub fn store(&self) -> &SettingsStore {
        &self.store
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::{SystemTime, UNIX_EPOCH};

    #[test]
    fn daily_pause_boundaries_and_invalid_inputs() {
        let periods = "23:00-07:00;12:00-13:00";
        assert_eq!(pause_remaining_seconds(periods, 23 * 3600), Some(8 * 3600));
        assert_eq!(pause_remaining_seconds(periods, 0), Some(7 * 3600));
        assert_eq!(pause_remaining_seconds(periods, 7 * 3600), None);
        assert_eq!(pause_remaining_seconds(periods, 12 * 3600), Some(3600));
        assert_eq!(pause_remaining_seconds(periods, 13 * 3600), None);
        for invalid in [
            "24:00-07:00",
            "12:60-13:00",
            "00:00-00:00",
            "学校-时间",
            "1:00-02:00",
            "12:00-13:00;",
        ] {
            assert!(parse_pause_periods(invalid).is_err());
        }
        assert!(parse_pause_periods("").unwrap().is_empty());
    }

    #[test]
    fn new_options_round_trip_and_legacy_notification_migrates() {
        let old: AppSettings = serde_json::from_str(r#"{"successNotification":false}"#).unwrap();
        assert!(!old.notifications_enabled);
        let value = AppSettings {
            autostart_login_once: true,
            pause_periods: "23:00-07:00".into(),
            ..old
        };
        let json = serde_json::to_string(&value).unwrap();
        let restored: AppSettings = serde_json::from_str(&json).unwrap();
        assert!(restored.autostart_login_once);
        assert_eq!(restored.pause_periods, "23:00-07:00");
        assert!(!restored.notifications_enabled);
        assert_eq!(restored.check_interval_seconds, 1800);
        assert_eq!(restored.failure_cooldown_seconds, 20);
    }

    #[test]
    fn provider_suffixes_match_legacy_client() {
        assert_eq!(Provider::Campus.submitted_account("123"), "123");
        assert_eq!(Provider::ChinaMobile.submitted_account("123"), "123@cmcc");
        assert_eq!(Provider::ChinaUnicom.submitted_account("123"), "123@unicom");
        assert_eq!(
            Provider::ChinaTelecom.submitted_account("123"),
            "123@telecom"
        );
    }

    #[test]
    fn schema_one_zero_cooldown_migrates_to_pause() {
        let settings: AppSettings = serde_json::from_str(
            r#"{
                "schemaVersion": 1,
                "account": " 11230909 ",
                "provider": "ChinaUnicom",
                "encryptedPassword": "legacy-dpapi",
                "checkIntervalSeconds": 30,
                "failureCooldownSeconds": 0,
                "fallbackProbeUrl": "https://example.com/ping",
                "autoStart": true,
                "successNotification": false
            }"#,
        )
        .unwrap();
        let settings = settings.normalize();
        assert_eq!(settings.schema_version, 2);
        assert_eq!(settings.failure_cooldown_seconds, -1);
        assert_eq!(settings.account, "11230909");
        assert_eq!(settings.provider, Provider::ChinaUnicom);
        assert!(settings.auto_start);
        assert!(!settings.notifications_enabled);
    }

    #[test]
    fn fallback_must_be_https_without_credentials() {
        assert!(is_valid_fallback_url("https://example.com/ping"));
        assert!(is_valid_fallback_url("HTTPS://example.com?probe=1"));
        assert!(!is_valid_fallback_url("http://example.com/ping"));
        assert!(!is_valid_fallback_url("https://user@example.com/ping"));
        assert!(!is_valid_fallback_url("https://example.com:bad/ping"));
        assert!(!is_valid_fallback_url("https://example.com/#fragment"));
    }

    #[test]
    fn store_round_trip_is_atomic_and_compatible() {
        let id = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let directory = env::temp_dir().join(format!("CampusNetAutoLogin-Rust-Test-{id}"));
        let store = SettingsStore::new(directory.join("settings.json"));
        let settings = AppSettings {
            account: "11230909".to_string(),
            provider: Provider::ChinaMobile,
            encrypted_password: "cipher".to_string(),
            ..Default::default()
        };
        store.save(&settings).unwrap();
        let loaded = store.load();
        assert_eq!(loaded.account, "11230909");
        assert_eq!(loaded.provider, Provider::ChinaMobile);
        assert!(!store.path().with_extension("json.tmp").exists());
        let json = fs::read_to_string(store.path()).unwrap();
        assert!(json.contains("\"schemaVersion\""));
        assert!(json.contains("\"ChinaMobile\""));
        let _ = fs::remove_dir_all(directory);
    }
}
