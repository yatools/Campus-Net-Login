use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{self, Receiver, RecvTimeoutError, Sender};
use std::sync::{Arc, Mutex};
use std::thread::{self, JoinHandle};
use std::time::Duration;

use crate::AppResult;
use crate::credentials::decrypt_password;
use crate::network::{NetworkBackend, WinHttpClient};
use crate::settings::{AppSettings, SettingsManager};
use crate::time_utils::now_utc_rfc3339;

fn failure_delay(settings: &AppSettings, failures: u32) -> Duration {
    let cap = settings.check_interval_seconds as u64;
    let base = settings.failure_cooldown_seconds.max(0) as u64;
    let seconds = if base == 0 {
        cap
    } else {
        base.saturating_mul(1u64 << failures.saturating_sub(2).min(32))
            .min(cap)
    };
    Duration::from_secs(seconds)
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum NetworkState {
    NotConfigured,
    Checking,
    Online,
    Offline,
    Authenticating,
    LoggingOut,
    AuthServerUnavailable,
    LoginFailed,
    LogoutFailed,
    Paused,
}

impl NetworkState {
    pub fn display_name(self) -> &'static str {
        match self {
            NetworkState::NotConfigured => "未配置",
            NetworkState::Checking => "检测中",
            NetworkState::Online => "已连接",
            NetworkState::Offline => "网络不可用",
            NetworkState::Authenticating => "正在登录",
            NetworkState::LoggingOut => "正在注销",
            NetworkState::AuthServerUnavailable => "认证服务器不可达",
            NetworkState::LoginFailed => "登录失败",
            NetworkState::LogoutFailed => "注销失败",
            NetworkState::Paused => "已暂停",
        }
    }

    pub fn uses_blue_icon(self) -> bool {
        matches!(self, NetworkState::Checking | NetworkState::Online)
    }
}

#[derive(Clone, Debug)]
pub struct NetworkStatusSnapshot {
    pub state: NetworkState,
    pub message: String,
    pub updated_at_utc: String,
}

impl Default for NetworkStatusSnapshot {
    fn default() -> Self {
        Self {
            state: NetworkState::Checking,
            message: "等待首次检测".to_string(),
            updated_at_utc: now_utc_rfc3339(),
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum NotificationKind {
    Information,
    Success,
    Warning,
    Error,
}

#[derive(Clone, Debug)]
pub struct AppNotification {
    pub kind: NotificationKind,
    pub title: String,
    pub message: String,
}

#[derive(Clone, Debug)]
pub enum MonitorEvent {
    Status(NetworkStatusSnapshot),
    Notification(AppNotification),
    SettingsChanged(AppSettings),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum MonitorCommand {
    Detect,
    ManualLogin,
    Logout,
    SettingsChanged,
    PauseChanged,
    Stop,
}

pub struct MonitorHandle {
    sender: Sender<MonitorCommand>,
    user_paused: Arc<AtomicBool>,
    failure_paused: Arc<AtomicBool>,
    current: Arc<Mutex<NetworkStatusSnapshot>>,
    worker: Option<JoinHandle<()>>,
}

impl MonitorHandle {
    pub fn start(
        settings: Arc<SettingsManager>,
        auto_started: bool,
        event_sink: impl Fn(MonitorEvent) + Send + Sync + 'static,
    ) -> AppResult<Self> {
        let (sender, receiver) = mpsc::channel();
        let user_paused = Arc::new(AtomicBool::new(false));
        let failure_paused = Arc::new(AtomicBool::new(false));
        let current = Arc::new(Mutex::new(NetworkStatusSnapshot::default()));
        let sink: Arc<dyn Fn(MonitorEvent) + Send + Sync> = Arc::new(event_sink);

        let worker_user_paused = Arc::clone(&user_paused);
        let worker_failure_paused = Arc::clone(&failure_paused);
        let worker_current = Arc::clone(&current);
        let worker_sink = Arc::clone(&sink);
        let worker = thread::Builder::new()
            .name("campus-network".to_string())
            .stack_size(512 * 1024)
            .spawn(move || match WinHttpClient::new() {
                Ok(backend) => {
                    let mut worker = Worker {
                        settings,
                        backend,
                        waiter: SystemWaiter,
                        receiver,
                        pending: None,
                        once_consumed: !auto_started,
                        consecutive_failures: 0,
                        user_paused: worker_user_paused,
                        failure_paused: worker_failure_paused,
                        current: worker_current,
                        sink: worker_sink,
                    };
                    worker.run();
                }
                Err(message) => {
                    let snapshot = NetworkStatusSnapshot {
                        state: NetworkState::LoginFailed,
                        message: message.clone(),
                        updated_at_utc: now_utc_rfc3339(),
                    };
                    *worker_current
                        .lock()
                        .unwrap_or_else(|item| item.into_inner()) = snapshot.clone();
                    worker_sink(MonitorEvent::Status(snapshot));
                    worker_sink(MonitorEvent::Notification(AppNotification {
                        kind: NotificationKind::Error,
                        title: "校园网自动登录".to_string(),
                        message,
                    }));
                }
            })
            .map_err(|error| format!("无法启动网络监控线程：{error}"))?;

        Ok(Self {
            sender,
            user_paused,
            failure_paused,
            current,
            worker: Some(worker),
        })
    }

    pub fn request_detection(&self) {
        let _ = self.sender.send(MonitorCommand::Detect);
    }

    pub fn request_manual_login(&self) {
        let _ = self.sender.send(MonitorCommand::ManualLogin);
    }

    pub fn request_logout(&self) {
        let _ = self.sender.send(MonitorCommand::Logout);
    }

    pub fn notify_settings_changed(&self) {
        let _ = self.sender.send(MonitorCommand::SettingsChanged);
    }

    pub fn set_user_paused(&self, paused: bool) {
        self.user_paused.store(paused, Ordering::Release);
        if !paused {
            self.failure_paused.store(false, Ordering::Release);
        }
        let _ = self.sender.send(MonitorCommand::PauseChanged);
    }

    pub fn toggle_pause(&self) {
        self.set_user_paused(!self.is_paused());
    }

    pub fn is_user_paused(&self) -> bool {
        self.user_paused.load(Ordering::Acquire)
    }

    pub fn is_paused(&self) -> bool {
        self.user_paused.load(Ordering::Acquire) || self.failure_paused.load(Ordering::Acquire)
    }

    pub fn current(&self) -> NetworkStatusSnapshot {
        self.current
            .lock()
            .unwrap_or_else(|item| item.into_inner())
            .clone()
    }

    pub fn stop(&mut self) {
        let _ = self.sender.send(MonitorCommand::Stop);
        if let Some(worker) = self.worker.take() {
            let _ = worker.join();
        }
    }
}

impl Drop for MonitorHandle {
    fn drop(&mut self) {
        self.stop();
    }
}

enum WaitOutcome {
    Elapsed,
    Command(MonitorCommand),
    Disconnected,
}

trait CommandWaiter {
    fn wait(&mut self, receiver: &Receiver<MonitorCommand>, duration: Duration) -> WaitOutcome;
}

struct SystemWaiter;

impl CommandWaiter for SystemWaiter {
    fn wait(&mut self, receiver: &Receiver<MonitorCommand>, duration: Duration) -> WaitOutcome {
        match receiver.recv_timeout(duration) {
            Ok(command) => WaitOutcome::Command(command),
            Err(RecvTimeoutError::Timeout) => WaitOutcome::Elapsed,
            Err(RecvTimeoutError::Disconnected) => WaitOutcome::Disconnected,
        }
    }
}

struct Worker<B: NetworkBackend, W: CommandWaiter> {
    settings: Arc<SettingsManager>,
    backend: B,
    waiter: W,
    receiver: Receiver<MonitorCommand>,
    pending: Option<MonitorCommand>,
    once_consumed: bool,
    consecutive_failures: u32,
    user_paused: Arc<AtomicBool>,
    failure_paused: Arc<AtomicBool>,
    current: Arc<Mutex<NetworkStatusSnapshot>>,
    sink: Arc<dyn Fn(MonitorEvent) + Send + Sync>,
}

impl<B: NetworkBackend, W: CommandWaiter> Worker<B, W> {
    fn run(&mut self) {
        let mut next_delay = Duration::ZERO;
        loop {
            let command = if let Some(command) = self.pending.take() {
                command
            } else if next_delay.is_zero() {
                match self.receiver.try_recv() {
                    Ok(command) => command,
                    Err(mpsc::TryRecvError::Empty) => MonitorCommand::Detect,
                    Err(mpsc::TryRecvError::Disconnected) => return,
                }
            } else {
                match self.waiter.wait(&self.receiver, next_delay) {
                    WaitOutcome::Elapsed => MonitorCommand::Detect,
                    WaitOutcome::Command(command) => command,
                    WaitOutcome::Disconnected => return,
                }
            };

            match command {
                MonitorCommand::Stop => return,
                MonitorCommand::ManualLogin => {
                    self.execute_manual_login();
                    next_delay = self.normal_interval();
                }
                MonitorCommand::Logout => {
                    self.execute_logout();
                    next_delay = self.normal_interval();
                }
                MonitorCommand::PauseChanged => {
                    if self.is_paused() {
                        self.publish(NetworkState::Paused, "自动重连已暂停，仍会继续检测网络。");
                        next_delay = self.normal_interval();
                    } else {
                        next_delay = Duration::ZERO;
                    }
                }
                MonitorCommand::SettingsChanged => {
                    self.consecutive_failures = 0;
                    next_delay = Duration::ZERO;
                }
                MonitorCommand::Detect => next_delay = self.execute_detection_cycle(),
            }
            // Wake at the next daily boundary even when the normal interval is long.
            if let Ok(periods) =
                crate::settings::parse_pause_periods(&self.settings.snapshot().pause_periods)
            {
                let now = crate::time_utils::local_second_of_day();
                for (start, end) in periods {
                    for minute in [start, end] {
                        let seconds = (u32::from(minute) * 60 + 86400 - now) % 86400;
                        if seconds > 0 {
                            next_delay = next_delay.min(Duration::from_secs(u64::from(seconds)));
                        }
                    }
                }
            }
        }
    }

    fn execute_detection_cycle(&mut self) -> Duration {
        let settings = self.settings.snapshot();
        if !settings.has_credentials() {
            self.publish(NetworkState::NotConfigured, "请先填写账号和密码。");
            return self.normal_interval();
        }

        if let Some(seconds) = crate::settings::pause_remaining_seconds(
            &settings.pause_periods,
            crate::time_utils::local_second_of_day(),
        ) {
            self.publish(
                NetworkState::Paused,
                "当前处于暂停时段，自动登录和检测均已暂停。",
            );
            return Duration::from_secs(seconds.min(60));
        }
        if settings.autostart_login_once && self.once_consumed {
            self.publish(
                NetworkState::Paused,
                "仅开机自启登录一次模式：自动任务已暂停，可手动登录。",
            );
            return self.normal_interval();
        }
        self.publish(NetworkState::Checking, "正在检测公网连接。");
        if self.backend.internet_available(&settings) {
            self.failure_paused.store(false, Ordering::Release);
            self.consecutive_failures = 0;
            self.once_consumed = true;
            self.publish(NetworkState::Online, "公网连接正常。");
            return self.normal_interval();
        }

        if !self.sleep_interruptible(Duration::from_secs(2)) {
            return Duration::ZERO;
        }
        if crate::settings::pause_remaining_seconds(
            &settings.pause_periods,
            crate::time_utils::local_second_of_day(),
        )
        .is_some()
        {
            return Duration::ZERO;
        }
        if self.backend.internet_available(&settings) {
            self.failure_paused.store(false, Ordering::Release);
            self.consecutive_failures = 0;
            self.once_consumed = true;
            self.publish(NetworkState::Online, "公网连接正常。");
            return self.normal_interval();
        }

        if self.is_paused() {
            self.publish(NetworkState::Paused, "网络不可用，自动重连当前已暂停。");
            return self.normal_interval();
        }

        self.publish(NetworkState::Offline, "公网不可用，正在检查认证服务器。");
        if !self.backend.authentication_server_reachable() {
            self.publish(
                NetworkState::AuthServerUnavailable,
                "无法访问校园网认证服务器。",
            );
            self.once_consumed = true;
            self.consecutive_failures = self.consecutive_failures.saturating_add(1);
            return failure_delay(&settings, self.consecutive_failures);
        }

        self.execute_automatic_login(&settings)
    }

    fn execute_automatic_login(&mut self, settings: &AppSettings) -> Duration {
        if crate::settings::pause_remaining_seconds(
            &settings.pause_periods,
            crate::time_utils::local_second_of_day(),
        )
        .is_some()
        {
            return Duration::ZERO;
        }
        self.once_consumed = true;
        let password = match decrypt_password(&settings.encrypted_password) {
            Ok(password) => password,
            Err(message) => {
                self.publish(NetworkState::NotConfigured, &message);
                self.notify(NotificationKind::Error, "校园网自动登录", &message);
                return self.normal_interval();
            }
        };
        let submitted_account = settings.provider.submitted_account(&settings.account);

        if crate::settings::pause_remaining_seconds(
            &settings.pause_periods,
            crate::time_utils::local_second_of_day(),
        )
        .is_some()
        {
            return Duration::ZERO;
        }
        self.once_consumed = true;
        self.publish(NetworkState::Authenticating, "正在自动登录。");
        let _ = self.backend.login(&submitted_account, password.as_str());
        if !self.sleep_interruptible(Duration::from_secs(3)) {
            return Duration::ZERO;
        }
        if crate::settings::pause_remaining_seconds(
            &settings.pause_periods,
            crate::time_utils::local_second_of_day(),
        )
        .is_some()
        {
            return Duration::ZERO;
        }
        if self.backend.internet_available(settings) {
            self.record_success();
            return self.normal_interval();
        }
        self.consecutive_failures = self.consecutive_failures.saturating_add(1);
        self.publish(NetworkState::LoginFailed, "登录后公网仍未恢复。");
        self.notify(
            NotificationKind::Error,
            "校园网登录失败",
            "公网仍未恢复，请检查账号、密码或运营商。",
        );
        if settings.failure_cooldown_seconds < 0 {
            self.failure_paused.store(true, Ordering::Release);
            self.publish(NetworkState::Paused, "登录失败，已按设置停止自动重试。");
            self.normal_interval()
        } else {
            failure_delay(settings, self.consecutive_failures)
        }
    }

    fn execute_manual_login(&mut self) {
        let settings = self.settings.snapshot();
        if !settings.has_credentials() {
            self.publish(NetworkState::NotConfigured, "请先填写账号和密码。");
            self.notify(
                NotificationKind::Warning,
                "校园网自动登录",
                "请先在设置中填写账号和密码。",
            );
            return;
        }
        let password = match decrypt_password(&settings.encrypted_password) {
            Ok(password) => password,
            Err(message) => {
                self.publish(NetworkState::NotConfigured, &message);
                self.notify(NotificationKind::Error, "校园网自动登录", &message);
                return;
            }
        };

        self.publish(NetworkState::Authenticating, "正在执行手动重新登录。");
        let submitted = settings.provider.submitted_account(&settings.account);
        let _ = self.backend.login(&submitted, password.as_str());
        if !self.sleep_interruptible(Duration::from_secs(3)) {
            return;
        }
        if self.backend.internet_available(&self.settings.snapshot()) {
            self.failure_paused.store(false, Ordering::Release);
            self.record_success();
        } else {
            self.publish(NetworkState::LoginFailed, "手动登录后公网仍未恢复。");
            self.notify(
                NotificationKind::Error,
                "校园网登录失败",
                "登录请求已发送，但公网仍未恢复。",
            );
        }
    }

    fn execute_logout(&mut self) {
        self.publish(
            NetworkState::LoggingOut,
            "正在向校园网认证服务器发送注销请求。",
        );
        match self.backend.logout() {
            Ok(()) => {
                self.user_paused.store(true, Ordering::Release);
                self.failure_paused.store(false, Ordering::Release);
                self.publish(NetworkState::Paused, "校园网已注销，自动重连已暂停。");
                self.notify(
                    NotificationKind::Information,
                    "校园网已注销",
                    "自动重连已暂停；需要联网时请从托盘恢复。",
                );
            }
            Err(message) => {
                self.publish(NetworkState::LogoutFailed, &message);
                self.notify(NotificationKind::Error, "校园网注销失败", &message);
            }
        }
    }

    fn record_success(&mut self) {
        self.consecutive_failures = 0;
        let update = self.settings.update(|settings| {
            settings.last_successful_login_utc = Some(now_utc_rfc3339());
        });
        if let Ok(settings) = update {
            (self.sink)(MonitorEvent::SettingsChanged(settings.clone()));
            self.publish(NetworkState::Online, "登录成功，公网连接已恢复。");
            if settings.notifications_enabled {
                self.notify(
                    NotificationKind::Success,
                    "校园网登录成功",
                    "公网连接已恢复。",
                );
            }
        } else if let Err(message) = update {
            self.publish(NetworkState::Online, "公网已恢复，但最近登录时间保存失败。");
            self.notify(NotificationKind::Warning, "校园网自动登录", &message);
        }
    }

    fn sleep_interruptible(&mut self, duration: Duration) -> bool {
        match self.waiter.wait(&self.receiver, duration) {
            WaitOutcome::Elapsed => true,
            WaitOutcome::Command(command) => {
                self.pending = Some(command);
                false
            }
            WaitOutcome::Disconnected => {
                self.pending = Some(MonitorCommand::Stop);
                false
            }
        }
    }

    fn normal_interval(&self) -> Duration {
        Duration::from_secs(self.settings.snapshot().check_interval_seconds as u64)
    }

    fn is_paused(&self) -> bool {
        self.user_paused.load(Ordering::Acquire) || self.failure_paused.load(Ordering::Acquire)
    }

    fn publish(&self, state: NetworkState, message: &str) {
        let snapshot = NetworkStatusSnapshot {
            state,
            message: message.to_string(),
            updated_at_utc: now_utc_rfc3339(),
        };
        *self.current.lock().unwrap_or_else(|item| item.into_inner()) = snapshot.clone();
        (self.sink)(MonitorEvent::Status(snapshot));
    }

    fn notify(&self, kind: NotificationKind, title: &str, message: &str) {
        if !self.settings.snapshot().notifications_enabled {
            return;
        }
        (self.sink)(MonitorEvent::Notification(AppNotification {
            kind,
            title: title.to_string(),
            message: message.to_string(),
        }));
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::credentials::encrypt_password;
    use crate::settings::{Provider, SettingsStore};
    use std::collections::VecDeque;
    use std::env;
    use std::fs;
    use std::time::{SystemTime, UNIX_EPOCH};

    struct FakeBackend {
        internet: VecDeque<bool>,
        auth_reachable: bool,
        login_count: usize,
        logout_result: AppResult<()>,
        logout_count: usize,
    }

    impl NetworkBackend for FakeBackend {
        fn internet_available(&mut self, _settings: &AppSettings) -> bool {
            self.internet
                .pop_front()
                .expect("internet result exhausted")
        }

        fn authentication_server_reachable(&mut self) -> bool {
            self.auth_reachable
        }

        fn login(&mut self, _submitted_account: &str, _password: &str) -> AppResult<()> {
            self.login_count += 1;
            Ok(())
        }

        fn logout(&mut self) -> AppResult<()> {
            self.logout_count += 1;
            self.logout_result.clone()
        }
    }

    #[derive(Default)]
    struct FakeWaiter {
        delays: Vec<Duration>,
    }

    impl CommandWaiter for FakeWaiter {
        fn wait(
            &mut self,
            _receiver: &Receiver<MonitorCommand>,
            duration: Duration,
        ) -> WaitOutcome {
            self.delays.push(duration);
            WaitOutcome::Elapsed
        }
    }

    fn fixture(
        internet: impl IntoIterator<Item = bool>,
        cooldown: i32,
    ) -> (
        Worker<FakeBackend, FakeWaiter>,
        Arc<SettingsManager>,
        std::path::PathBuf,
    ) {
        let id = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let directory = env::temp_dir().join(format!("CampusNetMonitor-Test-{id}"));
        let manager = Arc::new(SettingsManager::new(SettingsStore::new(
            directory.join("settings.json"),
        )));
        manager
            .replace(AppSettings {
                account: "11230909".to_string(),
                provider: Provider::ChinaMobile,
                encrypted_password: encrypt_password("password").unwrap(),
                failure_cooldown_seconds: cooldown,
                ..Default::default()
            })
            .unwrap();
        let (_sender, receiver) = mpsc::channel();
        let events = Arc::new(Mutex::new(Vec::new()));
        let captured = Arc::clone(&events);
        let sink: Arc<dyn Fn(MonitorEvent) + Send + Sync> = Arc::new(move |event| {
            captured.lock().unwrap().push(event);
        });
        (
            Worker {
                settings: Arc::clone(&manager),
                backend: FakeBackend {
                    internet: internet.into_iter().collect(),
                    auth_reachable: true,
                    login_count: 0,
                    logout_result: Ok(()),
                    logout_count: 0,
                },
                waiter: FakeWaiter::default(),
                receiver,
                pending: None,
                once_consumed: false,
                consecutive_failures: 0,
                user_paused: Arc::new(AtomicBool::new(false)),
                failure_paused: Arc::new(AtomicBool::new(false)),
                current: Arc::new(Mutex::new(NetworkStatusSnapshot::default())),
                sink,
            },
            manager,
            directory,
        )
    }

    #[test]
    fn cooldown_doubles_after_third_failure_and_caps() {
        let settings = AppSettings::default();
        let seconds: Vec<_> = (1..=11)
            .map(|n| failure_delay(&settings, n).as_secs())
            .collect();
        assert_eq!(
            seconds,
            [20, 20, 40, 80, 160, 320, 640, 1280, 1800, 1800, 1800]
        );
        assert_eq!(failure_delay(&settings, u32::MAX).as_secs(), 1800);
    }

    #[test]
    fn one_shot_never_retries_and_manual_launch_does_not_probe() {
        let (mut worker, manager, directory) = fixture([false, false, false], 20);
        manager.update(|s| s.autostart_login_once = true).unwrap();
        worker.execute_detection_cycle();
        worker.execute_detection_cycle();
        assert_eq!(worker.backend.login_count, 1);
        assert!(worker.backend.internet.is_empty());
        // A manually launched worker starts with its automatic opportunity consumed.
        worker.once_consumed = true;
        worker.execute_detection_cycle();
        assert_eq!(worker.backend.login_count, 1);
        let _ = fs::remove_dir_all(directory);
    }

    #[test]
    fn pause_window_prevents_all_automatic_network_requests() {
        let (mut worker, manager, directory) = fixture([], 20);
        manager
            .update(|s| s.pause_periods = "00:00-12:00;12:00-00:00".into())
            .unwrap();
        worker.execute_detection_cycle();
        assert_eq!(worker.backend.login_count, 0);
        assert!(!worker.once_consumed);
        assert_eq!(worker.current.lock().unwrap().state, NetworkState::Paused);
        let _ = fs::remove_dir_all(directory);
    }

    #[test]
    fn recovery_resets_failure_backoff() {
        let (mut worker, _, directory) = fixture([true], 20);
        worker.consecutive_failures = 8;
        worker.execute_detection_cycle();
        assert_eq!(worker.consecutive_failures, 0);
        let _ = fs::remove_dir_all(directory);
    }

    #[test]
    fn notification_switch_suppresses_every_kind() {
        let (mut worker, manager, directory) = fixture([], 20);
        let events = Arc::new(Mutex::new(Vec::new()));
        let captured = events.clone();
        worker.sink = Arc::new(move |e| captured.lock().unwrap().push(e));
        manager.update(|s| s.notifications_enabled = false).unwrap();
        for kind in [
            NotificationKind::Success,
            NotificationKind::Error,
            NotificationKind::Warning,
            NotificationKind::Information,
        ] {
            worker.notify(kind, "test", "test");
        }
        assert!(events.lock().unwrap().is_empty());
        manager.update(|s| s.notifications_enabled = true).unwrap();
        worker.notify(NotificationKind::Information, "test", "test");
        assert_eq!(events.lock().unwrap().len(), 1);
        let _ = fs::remove_dir_all(directory);
    }

    #[test]
    fn transient_failure_recovers_without_login() {
        let (mut worker, _, directory) = fixture([false, true], 300);
        let next = worker.execute_detection_cycle();
        assert_eq!(worker.backend.login_count, 0);
        assert_eq!(worker.current.lock().unwrap().state, NetworkState::Online);
        assert_eq!(next, Duration::from_secs(1800));
        assert_eq!(worker.waiter.delays, [Duration::from_secs(2)]);
        let _ = fs::remove_dir_all(directory);
    }

    #[test]
    fn automatic_login_uses_one_attempt_per_cycle() {
        let (mut worker, _, directory) = fixture([false, false, false, false, false, false], 300);
        let next = worker.execute_detection_cycle();
        assert_eq!(worker.backend.login_count, 1);
        assert_eq!(
            worker.current.lock().unwrap().state,
            NetworkState::LoginFailed
        );
        assert_eq!(next, Duration::from_secs(300));
        let seconds: Vec<u64> = worker.waiter.delays.iter().map(Duration::as_secs).collect();
        assert_eq!(seconds, [2, 3]);
        let _ = fs::remove_dir_all(directory);
    }

    #[test]
    fn negative_cooldown_pauses_future_login() {
        let (mut worker, _, directory) = fixture([false, false, false, false, false, false], -1);
        worker.execute_detection_cycle();
        assert!(worker.failure_paused.load(Ordering::Acquire));
        assert_eq!(worker.current.lock().unwrap().state, NetworkState::Paused);
        let _ = fs::remove_dir_all(directory);
    }

    #[test]
    fn user_pause_still_probes_but_does_not_login() {
        let (mut worker, _, directory) = fixture([false, false], 300);
        worker.user_paused.store(true, Ordering::Release);
        let next = worker.execute_detection_cycle();
        assert_eq!(worker.backend.login_count, 0);
        assert_eq!(worker.current.lock().unwrap().state, NetworkState::Paused);
        assert_eq!(next, Duration::from_secs(1800));
        let _ = fs::remove_dir_all(directory);
    }

    #[test]
    fn zero_cooldown_returns_to_normal_interval() {
        let (mut worker, _, directory) = fixture([false, false, false, false, false, false], 0);
        assert_eq!(worker.execute_detection_cycle(), Duration::from_secs(1800));
        assert!(!worker.failure_paused.load(Ordering::Acquire));
        let _ = fs::remove_dir_all(directory);
    }

    #[test]
    fn successful_logout_pauses_automatic_reconnect() {
        let (mut worker, _, directory) = fixture([], 300);
        worker.execute_logout();
        assert_eq!(worker.backend.logout_count, 1);
        assert!(worker.user_paused.load(Ordering::Acquire));
        assert!(!worker.failure_paused.load(Ordering::Acquire));
        assert_eq!(worker.current.lock().unwrap().state, NetworkState::Paused);
        let _ = fs::remove_dir_all(directory);
    }

    #[test]
    fn failed_logout_does_not_change_pause_state() {
        let (mut worker, _, directory) = fixture([], 300);
        worker.backend.logout_result = Err("认证服务器未确认注销成功。".to_string());
        worker.execute_logout();
        assert_eq!(worker.backend.logout_count, 1);
        assert!(!worker.user_paused.load(Ordering::Acquire));
        assert_eq!(
            worker.current.lock().unwrap().state,
            NetworkState::LogoutFailed
        );
        let _ = fs::remove_dir_all(directory);
    }
}
