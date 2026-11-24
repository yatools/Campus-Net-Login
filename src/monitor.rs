use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{self, Receiver, RecvTimeoutError, Sender};
use std::sync::{Arc, Mutex};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

use crate::AppResult;
use crate::credentials::decrypt_password;
use crate::network::{LoginResponse, NetworkBackend, ProbeOutcome, WinHttpClient};
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

fn login_failure_message(login_result: AppResult<LoginResponse>, fallback: &str) -> String {
    match login_result {
        Err(http_err) => http_err,
        Ok(LoginResponse::Rejected(reason)) => reason,
        Ok(LoginResponse::Accepted | LoginResponse::Unknown) => fallback.to_string(),
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TriggerSource {
    Resume,
    Unlock,
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

impl NotificationKind {
    pub fn is_enabled(self, settings: &AppSettings) -> bool {
        match self {
            Self::Success | Self::Information => settings.success_notifications_enabled,
            Self::Warning | Self::Error => settings.failure_notifications_enabled,
        }
    }
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
    SystemEvent(TriggerSource),
    ManualLogin,
    Logout,
    SettingsChanged,
    PauseChanged,
    Stop,
}

pub struct MonitorHandle {
    sender: Sender<MonitorCommand>,
    stopping: Arc<AtomicBool>,
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
        let stopping = Arc::new(AtomicBool::new(false));
        let user_paused = Arc::new(AtomicBool::new(false));
        let failure_paused = Arc::new(AtomicBool::new(false));
        let current = Arc::new(Mutex::new(NetworkStatusSnapshot::default()));
        let sink: Arc<dyn Fn(MonitorEvent) + Send + Sync> = Arc::new(event_sink);

        let worker_user_paused = Arc::clone(&user_paused);
        let worker_stopping = Arc::clone(&stopping);
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
                        stopping: worker_stopping,
                        pending: None,
                        autostart_consumed: !auto_started,
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
            stopping,
            user_paused,
            failure_paused,
            current,
            worker: Some(worker),
        })
    }

    pub fn request_detection(&self) {
        let _ = self.sender.send(MonitorCommand::Detect);
    }

    pub fn notify_system_event(&self, source: TriggerSource) {
        let _ = self.sender.send(MonitorCommand::SystemEvent(source));
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

    pub fn request_stop(&self) {
        self.stopping.store(true, Ordering::Release);
        let _ = self.sender.send(MonitorCommand::Stop);
    }

    pub fn stop(&mut self) {
        self.request_stop();
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
    fn wait(
        &mut self,
        receiver: &Receiver<MonitorCommand>,
        duration: Option<Duration>,
    ) -> WaitOutcome;
}

struct SystemWaiter;

impl CommandWaiter for SystemWaiter {
    fn wait(
        &mut self,
        receiver: &Receiver<MonitorCommand>,
        duration: Option<Duration>,
    ) -> WaitOutcome {
        match duration {
            None => match receiver.recv() {
                Ok(command) => WaitOutcome::Command(command),
                Err(_) => WaitOutcome::Disconnected,
            },
            Some(duration) => match receiver.recv_timeout(duration) {
                Ok(command) => WaitOutcome::Command(command),
                Err(RecvTimeoutError::Timeout) => WaitOutcome::Elapsed,
                Err(RecvTimeoutError::Disconnected) => WaitOutcome::Disconnected,
            },
        }
    }
}

enum CycleOutcome {
    Interrupted,
    AuthServerUnavailable,
    Done(Option<Duration>),
}

struct Worker<B: NetworkBackend, W: CommandWaiter> {
    settings: Arc<SettingsManager>,
    backend: B,
    waiter: W,
    receiver: Receiver<MonitorCommand>,
    stopping: Arc<AtomicBool>,
    pending: Option<MonitorCommand>,
    autostart_consumed: bool,
    consecutive_failures: u32,
    user_paused: Arc<AtomicBool>,
    failure_paused: Arc<AtomicBool>,
    current: Arc<Mutex<NetworkStatusSnapshot>>,
    sink: Arc<dyn Fn(MonitorEvent) + Send + Sync>,
}

impl<B: NetworkBackend, W: CommandWaiter> Worker<B, W> {
    fn run(&mut self) {
        let mut next_delay = self.initial_delay();
        loop {
            // Stop takes priority over work already queued by repeated menu clicks.
            if self.stopping.load(Ordering::Acquire) {
                return;
            }
            let command = if let Some(command) = self.pending.take() {
                command
            } else if matches!(next_delay, Some(delay) if delay.is_zero()) {
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
                    let settings = self.settings.snapshot();
                    next_delay = self.next_periodic_delay(&settings);
                }
                MonitorCommand::Logout => {
                    self.execute_logout();
                    let settings = self.settings.snapshot();
                    next_delay = self.next_periodic_delay(&settings);
                }
                MonitorCommand::PauseChanged => {
                    let settings = self.settings.snapshot();
                    if self.is_paused() {
                        let message = if settings.trigger_periodic {
                            "自动重连已暂停，仍会继续检测网络。"
                        } else {
                            "自动重连已暂停。"
                        };
                        self.publish(NetworkState::Paused, message);
                        next_delay = self.next_periodic_delay(&settings);
                    } else {
                        next_delay = Some(Duration::ZERO);
                    }
                }
                MonitorCommand::SettingsChanged => {
                    self.consecutive_failures = 0;
                    self.failure_paused.store(false, Ordering::Release);
                    let settings = self.settings.snapshot();
                    if !settings.has_credentials() {
                        self.publish(NetworkState::NotConfigured, "请先填写账号和密码。");
                        next_delay = None;
                    } else if settings.trigger_periodic {
                        next_delay = Some(Duration::ZERO);
                    } else {
                        self.publish_idle_paused(&settings);
                        next_delay = None;
                    }
                }
                MonitorCommand::Detect => next_delay = self.execute_detection_cycle(),
                MonitorCommand::SystemEvent(source) => {
                    next_delay = self.execute_system_event(source);
                }
            }
        }
    }

    fn initial_delay(&mut self) -> Option<Duration> {
        let settings = self.settings.snapshot();
        if !settings.has_credentials() {
            self.publish(NetworkState::NotConfigured, "请先填写账号和密码。");
            return None;
        }
        if (!self.autostart_consumed && settings.trigger_on_autostart) || settings.trigger_periodic
        {
            Some(Duration::ZERO)
        } else {
            self.autostart_consumed = true;
            self.publish_idle_paused(&settings);
            None
        }
    }

    fn publish_idle_paused(&self, settings: &AppSettings) {
        let message = if settings.has_any_auto_trigger() {
            "按事件触发模式已就绪（不会定时轮询），等待唤醒/解锁或手动操作。"
        } else {
            "自动触发均已关闭，可通过托盘手动检测或登录。"
        };
        self.publish(NetworkState::Paused, message);
    }

    fn execute_detection_cycle(&mut self) -> Option<Duration> {
        match self.run_detection_attempt(false) {
            CycleOutcome::Interrupted => Some(Duration::ZERO),
            CycleOutcome::Done(delay) => delay,
            CycleOutcome::AuthServerUnavailable => {
                let settings = self.settings.snapshot();
                self.publish(
                    NetworkState::AuthServerUnavailable,
                    "无法访问校园网认证服务器。",
                );
                self.autostart_consumed = true;
                self.consecutive_failures = self.consecutive_failures.saturating_add(1);
                if settings.trigger_periodic {
                    Some(failure_delay(&settings, self.consecutive_failures))
                } else {
                    None
                }
            }
        }
    }

    fn execute_system_event(&mut self, source: TriggerSource) -> Option<Duration> {
        let settings = self.settings.snapshot();
        let enabled = match source {
            TriggerSource::Resume => settings.trigger_on_resume,
            TriggerSource::Unlock => settings.trigger_on_unlock,
        };
        if !enabled {
            return self.next_periodic_delay(&settings);
        }
        if !settings.has_credentials() {
            self.publish(NetworkState::NotConfigured, "请先填写账号和密码。");
            return self.next_periodic_delay(&settings);
        }
        if self.is_paused() {
            self.publish(NetworkState::Paused, "网络检测触发已跳过：自动重连当前已暂停。");
            return self.next_periodic_delay(&settings);
        }

        // Coalesce bursty wake/unlock notifications over a 3s debounce window.
        if !self.sleep_with_coalesce(Duration::from_secs(3), true) {
            return Some(Duration::ZERO);
        }

        const NIC_RETRY_DELAYS: [Duration; 2] =
            [Duration::from_secs(5), Duration::from_secs(10)];
        for attempt in 0..=NIC_RETRY_DELAYS.len() {
            match self.run_detection_attempt(true) {
                CycleOutcome::Interrupted => return Some(Duration::ZERO),
                CycleOutcome::Done(delay) => return delay,
                CycleOutcome::AuthServerUnavailable => {
                    if let Some(&retry_delay) = NIC_RETRY_DELAYS.get(attempt) {
                        self.publish(
                            NetworkState::AuthServerUnavailable,
                            "认证服务器暂不可达，等待网络就绪后重试。",
                        );
                        if !self.sleep_with_coalesce(retry_delay, true) {
                            return Some(Duration::ZERO);
                        }
                    } else {
                        let settings = self.settings.snapshot();
                        self.publish(
                            NetworkState::AuthServerUnavailable,
                            "无法访问校园网认证服务器。",
                        );
                        self.autostart_consumed = true;
                        self.consecutive_failures = self.consecutive_failures.saturating_add(1);
                        return if settings.trigger_periodic {
                            Some(failure_delay(&settings, self.consecutive_failures))
                        } else {
                            None
                        };
                    }
                }
            }
        }

        unreachable!("bounded NIC retry loop always returns on final attempt")
    }

    fn run_detection_attempt(&mut self, coalesce_events: bool) -> CycleOutcome {
        if self.interrupted_with(coalesce_events) {
            return CycleOutcome::Interrupted;
        }
        let settings = self.settings.snapshot();
        if !settings.has_credentials() {
            self.publish(NetworkState::NotConfigured, "请先填写账号和密码。");
            return CycleOutcome::Done(self.next_periodic_delay(&settings));
        }

        self.publish(NetworkState::Checking, "正在检测公网连接。");
        let probe = self.backend.probe_internet(&settings);
        if self.interrupted_with(coalesce_events) {
            return CycleOutcome::Interrupted;
        }
        match probe {
            ProbeOutcome::Online => {
                self.failure_paused.store(false, Ordering::Release);
                self.consecutive_failures = 0;
                self.autostart_consumed = true;
                self.publish(NetworkState::Online, "公网连接正常。");
                return CycleOutcome::Done(self.next_periodic_delay(&settings));
            }
            ProbeOutcome::CaptivePortal => {
                // Captive portal response confirms interception immediately; skip 2s offline re-probe.
            }
            ProbeOutcome::Offline => {
                if !self.sleep_with_coalesce(Duration::from_secs(2), coalesce_events) {
                    return CycleOutcome::Interrupted;
                }
                let re_probe = self.backend.probe_internet(&settings);
                if self.interrupted_with(coalesce_events) {
                    return CycleOutcome::Interrupted;
                }
                if re_probe == ProbeOutcome::Online {
                    self.failure_paused.store(false, Ordering::Release);
                    self.consecutive_failures = 0;
                    self.autostart_consumed = true;
                    self.publish(NetworkState::Online, "公网连接正常。");
                    return CycleOutcome::Done(self.next_periodic_delay(&settings));
                }
            }
        }

        if self.is_paused() {
            self.publish(NetworkState::Paused, "网络不可用，自动重连当前已暂停。");
            return CycleOutcome::Done(self.next_periodic_delay(&settings));
        }

        self.publish(NetworkState::Offline, "公网不可用，正在检查认证服务器。");
        let reachable = self.backend.authentication_server_reachable();
        if self.interrupted_with(coalesce_events) {
            return CycleOutcome::Interrupted;
        }
        if !reachable {
            return CycleOutcome::AuthServerUnavailable;
        }

        match self.execute_automatic_login(&settings) {
            Some(delay) => CycleOutcome::Done(delay),
            None => CycleOutcome::Interrupted,
        }
    }

    fn execute_automatic_login(&mut self, settings: &AppSettings) -> Option<Option<Duration>> {
        if self.interrupted() {
            return None;
        }
        if self.is_paused() {
            self.publish(NetworkState::Paused, "网络不可用，自动重连当前已暂停。");
            return Some(self.next_periodic_delay(settings));
        }
        self.autostart_consumed = true;
        let password = match decrypt_password(&settings.encrypted_password) {
            Ok(password) => password,
            Err(message) => {
                // Pause automatic retries until settings are updated so decryption errors don't spam.
                self.failure_paused.store(true, Ordering::Release);
                self.publish(NetworkState::NotConfigured, &message);
                self.notify(NotificationKind::Error, "校园网自动登录", &message);
                return Some(self.next_periodic_delay(settings));
            }
        };
        let submitted_account = settings.provider.submitted_account(&settings.account);

        self.publish(NetworkState::Authenticating, "正在自动登录。");
        let login_result = self.backend.login(&submitted_account, password.as_str());
        drop(password);

        let online = self.confirm_online_after_login(settings, false)?;
        // Restored connectivity wins even if the login response was lost or reported rejection.
        if online {
            self.failure_paused.store(false, Ordering::Release);
            self.record_success();
            return Some(self.next_periodic_delay(settings));
        }

        self.consecutive_failures = self.consecutive_failures.saturating_add(1);
        let message = login_failure_message(
            login_result,
            "公网仍未恢复，请检查账号、密码或运营商。",
        );
        self.publish(NetworkState::LoginFailed, &message);
        self.notify(NotificationKind::Error, "校园网登录失败", &message);
        if settings.trigger_periodic {
            if settings.failure_cooldown_seconds < 0 {
                self.failure_paused.store(true, Ordering::Release);
                self.publish(NetworkState::Paused, "登录失败，已按设置停止自动重试。");
                Some(self.next_periodic_delay(settings))
            } else {
                Some(Some(failure_delay(settings, self.consecutive_failures)))
            }
        } else {
            Some(None)
        }
    }

    fn execute_manual_login(&mut self) {
        if self.stopping.load(Ordering::Acquire) {
            return;
        }
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
        let login_result = self.backend.login(&submitted, password.as_str());
        drop(password);

        let Some(online) = self.confirm_online_after_login(&settings, true) else {
            return;
        };
        if online {
            self.failure_paused.store(false, Ordering::Release);
            self.record_success();
        } else {
            let message =
                login_failure_message(login_result, "登录请求已发送，但公网仍未恢复。");
            self.publish(NetworkState::LoginFailed, &message);
            self.notify(NotificationKind::Error, "校园网登录失败", &message);
        }
    }

    fn confirm_online_after_login(
        &mut self,
        settings: &AppSettings,
        ignore_detect: bool,
    ) -> Option<bool> {
        const SCHEDULE: [Duration; 3] = [
            Duration::from_millis(500),
            Duration::from_millis(1000),
            Duration::from_millis(1500),
        ];
        for delay in SCHEDULE {
            if !self.sleep_with_coalesce(delay, ignore_detect) {
                return None;
            }
            let outcome = self.backend.probe_internet(settings);
            if self.interrupted_with(ignore_detect) {
                return None;
            }
            if outcome == ProbeOutcome::Online {
                return Some(true);
            }
        }
        Some(false)
    }

    fn execute_logout(&mut self) {
        if self.stopping.load(Ordering::Acquire) {
            return;
        }
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
            (self.sink)(MonitorEvent::SettingsChanged(settings));
            self.publish(NetworkState::Online, "登录成功，公网连接已恢复。");
            self.notify(
                NotificationKind::Success,
                "校园网登录成功",
                "公网连接已恢复。",
            );
        } else if let Err(message) = update {
            self.publish(NetworkState::Online, "公网已恢复，但最近登录时间保存失败。");
            self.notify(NotificationKind::Warning, "校园网自动登录", &message);
        }
    }

    fn interrupted(&mut self) -> bool {
        self.interrupted_with(false)
    }

    fn interrupted_with(&mut self, coalesce_events: bool) -> bool {
        if self.stopping.load(Ordering::Acquire) {
            self.pending = Some(MonitorCommand::Stop);
            return true;
        }
        if let Some(command) = self.pending {
            if coalesce_events
                && matches!(
                    command,
                    MonitorCommand::Detect | MonitorCommand::SystemEvent(_)
                )
            {
                self.pending = None;
            } else {
                return true;
            }
        }
        loop {
            match self.receiver.try_recv() {
                Ok(command) => {
                    if coalesce_events
                        && matches!(
                            command,
                            MonitorCommand::Detect | MonitorCommand::SystemEvent(_)
                        )
                    {
                        continue;
                    }
                    self.pending = Some(command);
                    return true;
                }
                Err(mpsc::TryRecvError::Disconnected) => {
                    self.pending = Some(MonitorCommand::Stop);
                    return true;
                }
                Err(mpsc::TryRecvError::Empty) => return false,
            }
        }
    }

    fn sleep_with_coalesce(&mut self, duration: Duration, coalesce_events: bool) -> bool {
        if self.interrupted_with(coalesce_events) {
            return false;
        }
        if duration.is_zero() {
            return true;
        }
        let deadline = Instant::now() + duration;
        let mut remaining = duration;
        loop {
            match self.waiter.wait(&self.receiver, Some(remaining)) {
                WaitOutcome::Elapsed => return true,
                WaitOutcome::Command(command) => {
                    if coalesce_events
                        && matches!(
                            command,
                            MonitorCommand::Detect | MonitorCommand::SystemEvent(_)
                        )
                    {
                        if self.interrupted_with(true) {
                            return false;
                        }
                        remaining = deadline.saturating_duration_since(Instant::now());
                        if remaining.is_zero() {
                            return true;
                        }
                        continue;
                    }
                    self.pending = Some(command);
                    return false;
                }
                WaitOutcome::Disconnected => {
                    self.pending = Some(MonitorCommand::Stop);
                    return false;
                }
            }
        }
    }

    fn next_periodic_delay(&self, settings: &AppSettings) -> Option<Duration> {
        if settings.trigger_periodic {
            Some(Duration::from_secs(settings.check_interval_seconds as u64))
        } else {
            None
        }
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
        if !kind.is_enabled(&self.settings.snapshot()) {
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
        internet: VecDeque<ProbeOutcome>,
        auth_reachable: VecDeque<bool>,
        default_auth_reachable: bool,
        login_count: usize,
        login_result: AppResult<LoginResponse>,
        after_auth_check: Option<Box<dyn FnMut()>>,
        after_login: Option<Box<dyn FnMut()>>,
        logout_result: AppResult<()>,
        logout_count: usize,
    }

    impl NetworkBackend for FakeBackend {
        fn probe_internet(&mut self, _settings: &AppSettings) -> ProbeOutcome {
            self.internet
                .pop_front()
                .expect("internet result exhausted")
        }

        fn authentication_server_reachable(&mut self) -> bool {
            if let Some(callback) = &mut self.after_auth_check {
                callback();
            }
            self.auth_reachable
                .pop_front()
                .unwrap_or(self.default_auth_reachable)
        }

        fn login(
            &mut self,
            _submitted_account: &str,
            _password: &str,
        ) -> AppResult<LoginResponse> {
            self.login_count += 1;
            let result = self.login_result.clone();
            if let Some(callback) = &mut self.after_login {
                callback();
            }
            result
        }

        fn logout(&mut self) -> AppResult<()> {
            self.logout_count += 1;
            self.logout_result.clone()
        }
    }

    #[derive(Default)]
    struct FakeWaiter {
        delays: Vec<Option<Duration>>,
        idle_outcomes: VecDeque<WaitOutcome>,
        timed_outcomes: VecDeque<WaitOutcome>,
        sender: Option<Sender<MonitorCommand>>,
    }

    impl CommandWaiter for FakeWaiter {
        fn wait(
            &mut self,
            receiver: &Receiver<MonitorCommand>,
            duration: Option<Duration>,
        ) -> WaitOutcome {
            self.delays.push(duration);
            if duration.is_none() {
                if let Some(outcome) = self.idle_outcomes.pop_front() {
                    return outcome;
                }
                match receiver.try_recv() {
                    Ok(command) => WaitOutcome::Command(command),
                    Err(_) => WaitOutcome::Disconnected,
                }
            } else {
                self.timed_outcomes
                    .pop_front()
                    .unwrap_or(WaitOutcome::Elapsed)
            }
        }
    }

    fn fixture(
        internet: impl IntoIterator<Item = ProbeOutcome>,
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
        let (sender, receiver) = mpsc::channel();
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
                    auth_reachable: VecDeque::new(),
                    default_auth_reachable: true,
                    login_count: 0,
                    login_result: Ok(LoginResponse::Accepted),
                    after_auth_check: None,
                    after_login: None,
                    logout_result: Ok(()),
                    logout_count: 0,
                },
                waiter: FakeWaiter {
                    sender: Some(sender),
                    ..Default::default()
                },
                receiver,
                stopping: Arc::new(AtomicBool::new(false)),
                pending: None,
                autostart_consumed: false,
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
    fn commands_received_during_auth_check_prevent_stale_automatic_login() {
        for command in [
            MonitorCommand::SettingsChanged,
            MonitorCommand::PauseChanged,
            MonitorCommand::Logout,
            MonitorCommand::Stop,
        ] {
            let (mut worker, _, directory) =
                fixture([ProbeOutcome::Offline, ProbeOutcome::Offline], 20);
            let sender = worker.waiter.sender.as_ref().unwrap().clone();
            worker.backend.after_auth_check = Some(Box::new(move || {
                sender.send(command).unwrap();
            }));
            assert_eq!(worker.execute_detection_cycle(), Some(Duration::ZERO));
            assert_eq!(worker.pending, Some(command));
            assert_eq!(worker.backend.login_count, 0);
            assert!(!worker.autostart_consumed);
            let _ = fs::remove_dir_all(directory);
        }
    }

    #[test]
    fn stop_during_auth_check_takes_priority_over_queued_work() {
        let (mut worker, _, directory) =
            fixture([ProbeOutcome::Offline, ProbeOutcome::Offline], 20);
        let stopping = Arc::clone(&worker.stopping);
        let sender = worker.waiter.sender.as_ref().unwrap().clone();
        worker.backend.after_auth_check = Some(Box::new(move || {
            sender.send(MonitorCommand::ManualLogin).unwrap();
            stopping.store(true, Ordering::Release);
        }));
        assert_eq!(worker.execute_detection_cycle(), Some(Duration::ZERO));
        assert_eq!(worker.pending, Some(MonitorCommand::Stop));
        worker.run();
        assert_eq!(worker.backend.login_count, 0);
        let _ = fs::remove_dir_all(directory);
    }

    #[test]
    fn queued_detection_does_not_discard_explicit_logout() {
        let (mut worker, _, directory) = fixture([], 20);
        worker
            .waiter
            .sender
            .as_ref()
            .unwrap()
            .send(MonitorCommand::Detect)
            .unwrap();
        worker.execute_logout();
        assert_eq!(worker.backend.logout_count, 1);
        assert!(worker.user_paused.load(Ordering::Acquire));
        assert_eq!(worker.receiver.try_recv().unwrap(), MonitorCommand::Detect);
        let _ = fs::remove_dir_all(directory);
    }

    #[test]
    fn automatic_and_manual_login_report_request_errors_when_still_offline() {
        for manual in [false, true] {
            let (mut worker, _, directory) = fixture(
                [
                    ProbeOutcome::Offline,
                    ProbeOutcome::Offline,
                    ProbeOutcome::Offline,
                ],
                20,
            );
            let message = "登录请求返回 HTTP 503。";
            worker.backend.login_result = Err(message.to_string());
            if manual {
                worker.execute_manual_login();
            } else {
                worker.execute_automatic_login(&worker.settings.snapshot());
            }
            let status = worker.current.lock().unwrap();
            assert_eq!(status.state, NetworkState::LoginFailed);
            assert_eq!(status.message, message);
            assert_eq!(worker.backend.login_count, 1);
            let _ = fs::remove_dir_all(directory);
        }
    }

    #[test]
    fn restored_connectivity_wins_over_a_lost_login_response() {
        for manual in [false, true] {
            let (mut worker, manager, directory) = fixture([ProbeOutcome::Online], 20);
            worker.backend.login_result = Err("登录请求超时。".to_string());
            if manual {
                worker.execute_manual_login();
            } else {
                worker.execute_automatic_login(&worker.settings.snapshot());
            }
            assert_eq!(worker.current.lock().unwrap().state, NetworkState::Online);
            assert!(manager.snapshot().last_successful_login_utc.is_some());
            assert_eq!(worker.consecutive_failures, 0);
            let _ = fs::remove_dir_all(directory);
        }
    }

    #[test]
    fn autostart_runs_once_without_periodic_and_manual_launch_starts_idle() {
        // Manual launch (autostart_consumed = true) with periodic disabled must not probe at startup.
        let (mut manual_worker, manager, directory) = fixture([], 20);
        manager
            .update(|s| {
                s.trigger_periodic = false;
                s.trigger_on_autostart = true;
                s.trigger_on_resume = false;
                s.trigger_on_unlock = false;
            })
            .unwrap();
        manual_worker.autostart_consumed = true;
        manual_worker.run();
        assert_eq!(manual_worker.backend.login_count, 0);
        assert_eq!(
            manual_worker.current.lock().unwrap().state,
            NetworkState::Paused
        );
        assert_eq!(manual_worker.waiter.delays, vec![None]);
        let _ = fs::remove_dir_all(directory);

        // Autostart launch (autostart_consumed = false) runs once immediately, then sleeps with None.
        let (mut auto_worker, manager, directory) =
            fixture([ProbeOutcome::CaptivePortal, ProbeOutcome::Online], 20);
        manager
            .update(|s| {
                s.trigger_periodic = false;
                s.trigger_on_autostart = true;
                s.trigger_on_resume = false;
                s.trigger_on_unlock = false;
            })
            .unwrap();
        auto_worker.autostart_consumed = false;
        auto_worker.run();
        assert!(auto_worker.autostart_consumed);
        assert_eq!(auto_worker.backend.login_count, 1);
        assert!(auto_worker.backend.internet.is_empty());
        assert_eq!(
            auto_worker.waiter.delays,
            vec![Some(Duration::from_millis(500)), None]
        );
        let _ = fs::remove_dir_all(directory);
    }

    #[test]
    fn recovery_resets_failure_backoff() {
        let (mut worker, _, directory) = fixture([ProbeOutcome::Online], 20);
        worker.consecutive_failures = 8;
        worker.execute_detection_cycle();
        assert_eq!(worker.consecutive_failures, 0);
        let _ = fs::remove_dir_all(directory);
    }

    #[test]
    fn success_and_failure_notifications_are_independently_filtered() {
        let (mut worker, manager, directory) = fixture([], 20);
        let events = Arc::new(Mutex::new(Vec::new()));
        let captured = events.clone();
        worker.sink = Arc::new(move |event| captured.lock().unwrap().push(event));
        for (success, failure) in [(false, true), (true, false), (false, false), (true, true)] {
            manager
                .update(|s| {
                    s.success_notifications_enabled = success;
                    s.failure_notifications_enabled = failure;
                })
                .unwrap();
            events.lock().unwrap().clear();
            for kind in [
                NotificationKind::Success,
                NotificationKind::Error,
                NotificationKind::Warning,
                NotificationKind::Information,
            ] {
                worker.notify(kind, "test", "test");
            }
            let kinds: Vec<_> = events
                .lock()
                .unwrap()
                .iter()
                .filter_map(|event| match event {
                    MonitorEvent::Notification(n) => Some(n.kind),
                    _ => None,
                })
                .collect();
            assert_eq!(kinds.contains(&NotificationKind::Success), success);
            assert_eq!(kinds.contains(&NotificationKind::Information), success);
            assert_eq!(kinds.contains(&NotificationKind::Error), failure);
            assert_eq!(kinds.contains(&NotificationKind::Warning), failure);
        }
        let _ = fs::remove_dir_all(directory);
    }

    #[test]
    fn transient_failure_recovers_without_login() {
        let (mut worker, _, directory) =
            fixture([ProbeOutcome::Offline, ProbeOutcome::Online], 300);
        let next = worker.execute_detection_cycle();
        assert_eq!(worker.backend.login_count, 0);
        assert_eq!(worker.current.lock().unwrap().state, NetworkState::Online);
        assert_eq!(next, Some(Duration::from_secs(1800)));
        assert_eq!(worker.waiter.delays, [Some(Duration::from_secs(2))]);
        let _ = fs::remove_dir_all(directory);
    }

    #[test]
    fn automatic_login_uses_one_attempt_per_cycle() {
        let (mut worker, _, directory) = fixture([ProbeOutcome::Offline; 5], 300);
        let next = worker.execute_detection_cycle();
        assert_eq!(worker.backend.login_count, 1);
        assert_eq!(
            worker.current.lock().unwrap().state,
            NetworkState::LoginFailed
        );
        assert_eq!(next, Some(Duration::from_secs(300)));
        assert_eq!(
            worker.waiter.delays,
            [
                Some(Duration::from_secs(2)),
                Some(Duration::from_millis(500)),
                Some(Duration::from_millis(1000)),
                Some(Duration::from_millis(1500)),
            ]
        );
        let _ = fs::remove_dir_all(directory);
    }

    #[test]
    fn negative_cooldown_pauses_future_login() {
        let (mut worker, _, directory) = fixture([ProbeOutcome::Offline; 5], -1);
        worker.execute_detection_cycle();
        assert!(worker.failure_paused.load(Ordering::Acquire));
        assert_eq!(worker.current.lock().unwrap().state, NetworkState::Paused);
        let _ = fs::remove_dir_all(directory);
    }

    #[test]
    fn user_pause_still_probes_but_does_not_login() {
        let (mut worker, _, directory) =
            fixture([ProbeOutcome::Offline, ProbeOutcome::Offline], 300);
        worker.user_paused.store(true, Ordering::Release);
        let next = worker.execute_detection_cycle();
        assert_eq!(worker.backend.login_count, 0);
        assert_eq!(worker.current.lock().unwrap().state, NetworkState::Paused);
        assert_eq!(next, Some(Duration::from_secs(1800)));
        let _ = fs::remove_dir_all(directory);
    }

    #[test]
    fn zero_cooldown_returns_to_normal_interval() {
        let (mut worker, _, directory) = fixture([ProbeOutcome::Offline; 5], 0);
        assert_eq!(
            worker.execute_detection_cycle(),
            Some(Duration::from_secs(1800))
        );
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

    #[test]
    fn settings_changed_clears_failure_paused_and_resets_failures() {
        let (mut worker, manager, directory) = fixture([], 20);
        manager
            .update(|s| {
                s.trigger_periodic = false;
                s.trigger_on_resume = true;
            })
            .unwrap();
        worker.autostart_consumed = true;
        worker.failure_paused.store(true, Ordering::Release);
        worker.consecutive_failures = 4;
        worker
            .waiter
            .idle_outcomes
            .push_back(WaitOutcome::Command(MonitorCommand::SettingsChanged));
        worker
            .waiter
            .idle_outcomes
            .push_back(WaitOutcome::Command(MonitorCommand::Stop));

        worker.run();
        assert!(!worker.failure_paused.load(Ordering::Acquire));
        assert_eq!(worker.consecutive_failures, 0);
        assert_eq!(worker.current.lock().unwrap().state, NetworkState::Paused);
        assert_eq!(worker.waiter.delays, vec![None, None]);
        let _ = fs::remove_dir_all(directory);
    }

    #[test]
    fn manual_login_verification_is_not_aborted_by_queued_detect_or_system_event() {
        let (mut worker, manager, directory) = fixture([ProbeOutcome::Online], 20);
        let sender = worker.waiter.sender.as_ref().unwrap().clone();
        sender.send(MonitorCommand::Detect).unwrap();
        worker.backend.after_login = Some(Box::new(move || {
            sender
                .send(MonitorCommand::SystemEvent(TriggerSource::Resume))
                .unwrap();
        }));
        worker.execute_manual_login();
        assert_eq!(worker.backend.login_count, 1);
        assert_eq!(worker.current.lock().unwrap().state, NetworkState::Online);
        assert!(manager.snapshot().last_successful_login_utc.is_some());
        assert!(worker.pending.is_none());
        assert!(worker.receiver.try_recv().is_err());
        let _ = fs::remove_dir_all(directory);
    }

    #[test]
    fn manual_login_verification_still_honors_higher_priority_commands() {
        for command in [
            MonitorCommand::Stop,
            MonitorCommand::Logout,
            MonitorCommand::SettingsChanged,
            MonitorCommand::ManualLogin,
        ] {
            let (mut worker, manager, directory) = fixture([ProbeOutcome::Online], 20);
            let sender = worker.waiter.sender.as_ref().unwrap().clone();
            worker.backend.after_login = Some(Box::new(move || {
                sender.send(command).unwrap();
            }));
            worker.execute_manual_login();
            assert_eq!(worker.pending, Some(command));
            assert!(manager.snapshot().last_successful_login_utc.is_none());
            let _ = fs::remove_dir_all(directory);
        }
    }

    #[test]
    fn captive_portal_skips_two_second_reprobe_and_logs_in_directly() {
        let (mut worker, _, directory) =
            fixture([ProbeOutcome::CaptivePortal, ProbeOutcome::Online], 20);
        let next = worker.execute_detection_cycle();
        assert_eq!(worker.backend.login_count, 1);
        assert_eq!(worker.current.lock().unwrap().state, NetworkState::Online);
        assert_eq!(next, Some(Duration::from_secs(1800)));
        assert_eq!(worker.waiter.delays, [Some(Duration::from_millis(500))]);
        let _ = fs::remove_dir_all(directory);
    }

    #[test]
    fn progressive_post_login_verification_stops_early_and_caps_at_three_steps() {
        // Stops immediately at 500ms when Online.
        let (mut worker, _, directory) = fixture([ProbeOutcome::Online], 20);
        let settings = worker.settings.snapshot();
        assert_eq!(
            worker.confirm_online_after_login(&settings, false),
            Some(true)
        );
        assert_eq!(worker.waiter.delays, [Some(Duration::from_millis(500))]);
        let _ = fs::remove_dir_all(directory);

        // Stops at 1000ms on second check.
        let (mut worker, _, directory) =
            fixture([ProbeOutcome::CaptivePortal, ProbeOutcome::Online], 20);
        let settings = worker.settings.snapshot();
        assert_eq!(
            worker.confirm_online_after_login(&settings, false),
            Some(true)
        );
        assert_eq!(
            worker.waiter.delays,
            [
                Some(Duration::from_millis(500)),
                Some(Duration::from_millis(1000)),
            ]
        );
        let _ = fs::remove_dir_all(directory);

        // Runs 500ms -> 1000ms -> 1500ms before returning Some(false).
        let (mut worker, _, directory) = fixture([ProbeOutcome::Offline; 3], 20);
        let settings = worker.settings.snapshot();
        assert_eq!(
            worker.confirm_online_after_login(&settings, false),
            Some(false)
        );
        assert_eq!(
            worker.waiter.delays,
            [
                Some(Duration::from_millis(500)),
                Some(Duration::from_millis(1000)),
                Some(Duration::from_millis(1500)),
            ]
        );
        let _ = fs::remove_dir_all(directory);
    }

    #[test]
    fn rejected_login_response_surfaces_reason_while_online_still_wins() {
        let reason = "账号或密码错误，请检查学号、密码和所选服务商。".to_string();
        let (mut worker, _, directory) = fixture(
            [
                ProbeOutcome::CaptivePortal,
                ProbeOutcome::Offline,
                ProbeOutcome::Offline,
                ProbeOutcome::Offline,
            ],
            20,
        );
        worker.backend.login_result = Ok(LoginResponse::Rejected(reason.clone()));
        worker.execute_detection_cycle();
        let status = worker.current.lock().unwrap();
        assert_eq!(status.state, NetworkState::LoginFailed);
        assert_eq!(status.message, reason);
        let _ = fs::remove_dir_all(directory);

        // Connectivity wins even if Dr.COM returned Rejected.
        let (mut worker, manager, directory) =
            fixture([ProbeOutcome::CaptivePortal, ProbeOutcome::Online], 20);
        worker.backend.login_result = Ok(LoginResponse::Rejected(reason));
        worker.execute_detection_cycle();
        assert_eq!(worker.current.lock().unwrap().state, NetworkState::Online);
        assert!(manager.snapshot().last_successful_login_utc.is_some());
        assert_eq!(worker.consecutive_failures, 0);
        let _ = fs::remove_dir_all(directory);
    }

    #[test]
    fn system_event_respects_flags_debounces_and_runs_bounded_nic_retries() {
        // Disabled Resume/Unlock triggers are ignored without probing.
        let (mut worker, manager, directory) = fixture([], 20);
        manager
            .update(|s| {
                s.trigger_periodic = false;
                s.trigger_on_resume = false;
                s.trigger_on_unlock = false;
            })
            .unwrap();
        assert_eq!(worker.execute_system_event(TriggerSource::Resume), None);
        assert_eq!(worker.execute_system_event(TriggerSource::Unlock), None);
        assert!(worker.waiter.delays.is_empty());
        let _ = fs::remove_dir_all(directory);

        // Coalesces duplicate SystemEvent/Detect during 3s debounce and succeeds on 3rd attempt (+5s, +10s).
        let (mut worker, manager, directory) = fixture(
            [
                ProbeOutcome::CaptivePortal,
                ProbeOutcome::CaptivePortal,
                ProbeOutcome::CaptivePortal,
                ProbeOutcome::Online,
            ],
            20,
        );
        manager
            .update(|s| {
                s.trigger_periodic = false;
                s.trigger_on_resume = true;
                s.trigger_on_unlock = true;
            })
            .unwrap();
        worker.backend.auth_reachable = VecDeque::from([false, false, true]);
        worker
            .waiter
            .timed_outcomes
            .push_back(WaitOutcome::Command(MonitorCommand::SystemEvent(
                TriggerSource::Unlock,
            )));
        worker.waiter.timed_outcomes.push_back(WaitOutcome::Elapsed);
        let next = worker.execute_system_event(TriggerSource::Resume);
        assert_eq!(next, None);
        assert_eq!(worker.backend.login_count, 1);
        assert_eq!(worker.current.lock().unwrap().state, NetworkState::Online);
        assert_eq!(worker.waiter.delays.len(), 5);
        assert_eq!(worker.waiter.delays[0], Some(Duration::from_secs(3)));
        assert_eq!(worker.waiter.delays[2], Some(Duration::from_secs(5)));
        assert_eq!(worker.waiter.delays[3], Some(Duration::from_secs(10)));
        assert_eq!(
            worker.waiter.delays[4],
            Some(Duration::from_millis(500))
        );
        let _ = fs::remove_dir_all(directory);

        // Auth server reachable + login failure when trigger_periodic == false does NOT run NIC retries or pause.
        let (mut worker, manager, directory) = fixture(
            [
                ProbeOutcome::CaptivePortal,
                ProbeOutcome::Offline,
                ProbeOutcome::Offline,
                ProbeOutcome::Offline,
            ],
            -1,
        );
        manager
            .update(|s| {
                s.trigger_periodic = false;
                s.trigger_on_unlock = true;
            })
            .unwrap();
        worker.backend.login_result = Ok(LoginResponse::Rejected("密码错误".to_string()));
        let next = worker.execute_system_event(TriggerSource::Unlock);
        assert_eq!(next, None);
        assert_eq!(worker.backend.login_count, 1);
        assert!(!worker.failure_paused.load(Ordering::Acquire));
        assert_eq!(
            worker.current.lock().unwrap().state,
            NetworkState::LoginFailed
        );
        assert_eq!(
            worker.waiter.delays,
            [
                Some(Duration::from_secs(3)),
                Some(Duration::from_millis(500)),
                Some(Duration::from_millis(1000)),
                Some(Duration::from_millis(1500)),
            ]
        );
        let _ = fs::remove_dir_all(directory);

        // Exhausted NIC retries (initial + 2 retries = 3 attempts) publish AuthServerUnavailable and return None.
        let (mut worker, manager, directory) = fixture([ProbeOutcome::CaptivePortal; 3], 20);
        manager
            .update(|s| {
                s.trigger_periodic = false;
                s.trigger_on_resume = true;
            })
            .unwrap();
        worker.backend.auth_reachable = VecDeque::from([false, false, false]);
        let next = worker.execute_system_event(TriggerSource::Resume);
        assert_eq!(next, None);
        assert_eq!(worker.backend.login_count, 0);
        assert_eq!(
            worker.current.lock().unwrap().state,
            NetworkState::AuthServerUnavailable
        );
        assert_eq!(
            worker.waiter.delays,
            [
                Some(Duration::from_secs(3)),
                Some(Duration::from_secs(5)),
                Some(Duration::from_secs(10)),
            ]
        );
        let _ = fs::remove_dir_all(directory);

        // SettingsChanged interrupts 3s debounce without probing and stores pending.
        let (mut worker, manager, directory) = fixture([], 20);
        manager
            .update(|s| {
                s.trigger_periodic = false;
                s.trigger_on_resume = true;
            })
            .unwrap();
        worker
            .waiter
            .timed_outcomes
            .push_back(WaitOutcome::Command(MonitorCommand::SettingsChanged));
        let next = worker.execute_system_event(TriggerSource::Resume);
        assert_eq!(next, Some(Duration::ZERO));
        assert_eq!(worker.pending, Some(MonitorCommand::SettingsChanged));
        assert_eq!(worker.waiter.delays, [Some(Duration::from_secs(3))]);
        let _ = fs::remove_dir_all(directory);
    }

    #[test]
    fn decrypt_error_in_automatic_login_sets_failure_paused() {
        let (mut worker, manager, directory) = fixture([ProbeOutcome::CaptivePortal], 20);
        manager
            .update(|s| s.encrypted_password = "not-valid-dpapi-base64".to_string())
            .unwrap();
        let next = worker.execute_detection_cycle();
        assert!(worker.failure_paused.load(Ordering::Acquire));
        assert_eq!(
            worker.current.lock().unwrap().state,
            NetworkState::NotConfigured
        );
        assert_eq!(next, Some(Duration::from_secs(1800)));
        let _ = fs::remove_dir_all(directory);
    }

    #[test]
    fn trigger_periodic_false_waits_with_none_and_zero_polling() {
        let (mut worker, manager, directory) = fixture([], 20);
        manager
            .update(|s| {
                s.trigger_periodic = false;
                s.trigger_on_autostart = false;
                s.trigger_on_resume = true;
                s.trigger_on_unlock = false;
            })
            .unwrap();
        worker.autostart_consumed = true;
        worker.run();
        assert_eq!(worker.waiter.delays, vec![None]);
        assert_eq!(worker.current.lock().unwrap().state, NetworkState::Paused);
        assert!(
            worker
                .current
                .lock()
                .unwrap()
                .message
                .contains("按事件触发模式已就绪")
        );
        let _ = fs::remove_dir_all(directory);

        let (mut worker, manager, directory) = fixture([], 20);
        manager
            .update(|s| {
                s.trigger_periodic = false;
                s.trigger_on_autostart = false;
                s.trigger_on_resume = false;
                s.trigger_on_unlock = false;
            })
            .unwrap();
        worker.autostart_consumed = true;
        assert_eq!(worker.initial_delay(), None);
        assert_eq!(
            worker.current.lock().unwrap().message,
            "自动触发均已关闭，可通过托盘手动检测或登录。"
        );
        let _ = fs::remove_dir_all(directory);

        let (mut worker, manager, directory) = fixture([ProbeOutcome::Online], 20);
        manager
            .update(|s| {
                s.trigger_periodic = false;
                s.trigger_on_autostart = false;
                s.trigger_on_resume = false;
                s.trigger_on_unlock = false;
            })
            .unwrap();
        worker.autostart_consumed = true;
        worker
            .waiter
            .idle_outcomes
            .push_back(WaitOutcome::Command(MonitorCommand::Detect));
        worker
            .waiter
            .idle_outcomes
            .push_back(WaitOutcome::Command(MonitorCommand::Stop));
        worker.run();
        assert_eq!(worker.waiter.delays, vec![None, None]);
        assert_eq!(worker.current.lock().unwrap().state, NetworkState::Online);
        let _ = fs::remove_dir_all(directory);
    }
}
