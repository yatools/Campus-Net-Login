mod controls;
mod dialog;
mod system;
mod tray;

use std::env;
use std::sync::atomic::{AtomicBool, AtomicIsize, AtomicU32, Ordering};
use std::sync::{Arc, Mutex};
use std::thread::JoinHandle;

use windows::Win32::Foundation::{HINSTANCE, HWND, LPARAM, LRESULT, WPARAM};
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::System::RemoteDesktop::{
    NOTIFY_FOR_THIS_SESSION, WTSRegisterSessionNotification, WTSUnRegisterSessionNotification,
};
use windows::Win32::UI::Input::Ime::ImmDisableIME;
use windows::Win32::UI::WindowsAndMessaging::{
    CreateWindowExW, DefWindowProcW, DestroyWindow, DispatchMessageW, GWLP_USERDATA, GetMessageW,
    GetWindowLongPtrW, HICON, IsWindow, MSG, PBT_APMRESUMEAUTOMATIC, PBT_APMRESUMESUSPEND,
    PM_REMOVE, PeekMessageW, PostQuitMessage, RegisterWindowMessageW, SetWindowLongPtrW,
    TranslateMessage, WINDOW_EX_STYLE, WM_APP, WM_CLOSE, WM_COMMAND, WM_DESTROY, WM_LBUTTONUP,
    WM_POWERBROADCAST, WM_RBUTTONUP, WM_WTSSESSION_CHANGE, WS_OVERLAPPED, WTS_SESSION_UNLOCK,
};

use crate::AppResult;
use crate::monitor::{AppNotification, MonitorEvent, MonitorHandle, NotificationKind, TriggerSource};
use crate::settings::{SettingsManager, SettingsStore};
use crate::startup::StartupManager;
use crate::wide::{pcwstr, to_wide};

use self::system::{
    SingleInstance, load_icon, notify_existing_instance, register_window_class, show_message,
};

const CLASS_NAME: &str = "CampusNetAutoLogin.NativeWindow";
const WINDOW_NAME: &str = "CampusNetAutoLogin";
const MUTEX_NAME: &str = "Local\\CampusNetAutoLogin.Native.SingleInstance";
const APP_TITLE: &str = "南湖校园网自动登录";

const IDI_BLUE: usize = 101;
const IDI_RED: usize = 102;
const IDD_SETTINGS: usize = 201;

const IDC_ACCOUNT: i32 = 1001;
const IDC_PASSWORD: i32 = 1002;
const IDC_PROVIDER: i32 = 1003;
const IDC_INTERVAL: i32 = 1004;
const IDC_COOLDOWN: i32 = 1005;
const IDC_FALLBACK: i32 = 1006;
const IDC_AUTOSTART: i32 = 1007;
const IDC_NOTIFY_SUCCESS: i32 = 1008;
const IDC_LAST_LOGIN: i32 = 1009;
const IDC_STATUS: i32 = 1010;
const IDC_CLEAR_DATA: u16 = 1011;
const IDC_INTERVAL_SPIN: i32 = 1013;
const IDC_COOLDOWN_SPIN: i32 = 1014;
const IDC_SECURITY_NOTICE: i32 = 1015;
const IDC_GITHUB: i32 = 1018;
const IDC_NOTIFY_FAILURE: i32 = 1019;
const IDC_COOLDOWN_FRAME: i32 = 1027;
const IDC_PASSWORD_TOGGLE: i32 = 1028;
const IDC_TRIGGER_PERIODIC: i32 = 1029;
const IDC_TRIGGER_AUTOSTART: i32 = 1030;
const IDC_TRIGGER_RESUME: i32 = 1031;
const IDC_TRIGGER_UNLOCK: i32 = 1032;

const IDM_OPEN_PORTAL: u32 = 4001;
const IDM_DETECT: u32 = 4002;
const IDM_LOGIN: u32 = 4003;
const IDM_PAUSE: u32 = 4004;
const IDM_SETTINGS: u32 = 4005;
const IDM_AUTOSTART: u32 = 4006;
const IDM_EXIT: u32 = 4007;
const IDM_LOGOUT: u32 = 4008;

const WM_TRAY: u32 = WM_APP + 1;
const WM_MONITOR_EVENT: u32 = WM_APP + 2;
const WM_SHOW_SETTINGS: u32 = WM_APP + 3;
const WM_DIALOG_REFRESH: u32 = WM_APP + 10;
const TRAY_ID: u32 = 1;

struct App {
    hwnd: HWND,
    hinstance: HINSTANCE,
    settings: Arc<SettingsManager>,
    startup: StartupManager,
    monitor: MonitorHandle,
    blue_icon: HICON,
    red_icon: HICON,
    taskbar_created: u32,
    wts_registered: AtomicBool,
    window_destroyed: AtomicBool,
    menu_open: AtomicBool,
    settings_dialog_open: AtomicBool,
    settings_dialog_hwnd: AtomicIsize,
    settings_thread: Mutex<Option<JoinHandle<()>>>,
    password_visible: AtomicBool,
    default_password_char: AtomicU32,
    _instance: SingleInstance,
}

struct EarlyWindowGuard {
    hwnd: Option<HWND>,
    wts_registered: bool,
}

impl EarlyWindowGuard {
    fn disarm(&mut self) {
        self.hwnd = None;
        self.wts_registered = false;
    }
}

impl Drop for EarlyWindowGuard {
    fn drop(&mut self) {
        if let Some(hwnd) = self.hwnd.take() {
            if self.wts_registered {
                unsafe {
                    let _ = WTSUnRegisterSessionNotification(hwnd);
                }
            }
            unsafe {
                let _ = DestroyWindow(hwnd);
            }
        }
    }
}

struct AppGuard {
    app_pointer: *mut App,
    tray_added: bool,
}

impl Drop for AppGuard {
    fn drop(&mut self) {
        if self.app_pointer.is_null() {
            return;
        }
        let hwnd = unsafe { (*self.app_pointer).hwnd };
        unsafe {
            SetWindowLongPtrW(hwnd, GWLP_USERDATA, 0);
        }
        let mut app = unsafe { Box::from_raw(self.app_pointer) };
        self.app_pointer = std::ptr::null_mut();

        app.monitor.request_stop();
        app.close_settings_dialog_and_wait();
        app.monitor.stop();
        drain_queued_monitor_events(hwnd);
        if self.tray_added {
            app.remove_tray_icon();
        }
        if app.wts_registered.swap(false, Ordering::AcqRel) {
            unsafe {
                let _ = WTSUnRegisterSessionNotification(hwnd);
            }
        }
        let should_destroy = !app.window_destroyed.load(Ordering::Acquire)
            && unsafe { IsWindow(Some(hwnd)).as_bool() };
        if should_destroy {
            unsafe {
                let _ = DestroyWindow(hwnd);
            }
        }
        drop(app);
    }
}

fn drain_queued_monitor_events(hwnd: HWND) {
    let mut message = MSG::default();
    while unsafe {
        PeekMessageW(
            &mut message,
            None,
            WM_MONITOR_EVENT,
            WM_MONITOR_EVENT,
            PM_REMOVE,
        )
    }
    .as_bool()
    {
        if message.hwnd == hwnd {
            let event_pointer = message.lParam.0 as *mut MonitorEvent;
            if !event_pointer.is_null() {
                unsafe {
                    drop(Box::from_raw(event_pointer));
                }
            }
        }
    }
}

pub fn run() -> AppResult<()> {
    let auto_started = env::args().skip(1).any(|item| item == "--autostart");
    let instance = SingleInstance::acquire()?;
    if instance.already_exists {
        if !auto_started {
            notify_existing_instance();
        }
        return Ok(());
    }

    unsafe {
        let _ = ImmDisableIME(u32::MAX);
    }
    let module =
        unsafe { GetModuleHandleW(None) }.map_err(|error| format!("无法读取程序模块：{error}"))?;
    let hinstance = HINSTANCE(module.0);
    register_window_class(hinstance)?;

    let class_name = to_wide(CLASS_NAME);
    let window_name = to_wide(WINDOW_NAME);
    let hwnd = unsafe {
        CreateWindowExW(
            WINDOW_EX_STYLE::default(),
            pcwstr(&class_name),
            pcwstr(&window_name),
            WS_OVERLAPPED,
            0,
            0,
            0,
            0,
            None,
            None,
            Some(hinstance),
            None,
        )
    }
    .map_err(|error| format!("无法创建托盘消息窗口：{error}"))?;

    let mut early_guard = EarlyWindowGuard {
        hwnd: Some(hwnd),
        wts_registered: false,
    };
    early_guard.wts_registered =
        unsafe { WTSRegisterSessionNotification(hwnd, NOTIFY_FOR_THIS_SESSION) }.is_ok();

    let (settings_manager, settings_warning) =
        SettingsManager::new_with_warning(SettingsStore::system_default()?);
    let settings = Arc::new(settings_manager);
    let startup = StartupManager::new(
        env::current_exe().map_err(|error| format!("无法确定程序路径：{error}"))?,
    )?;
    let startup_warning = if settings.snapshot().auto_start {
        startup.sync_if_needed(true).err()
    } else {
        None
    };

    let blue_icon = load_icon(hinstance, IDI_BLUE)?;
    let red_icon = load_icon(hinstance, IDI_RED)?;
    let event_hwnd = hwnd.0 as isize;
    let monitor = MonitorHandle::start(Arc::clone(&settings), auto_started, move |event| {
        let pointer = Box::into_raw(Box::new(event));
        let event_hwnd = HWND(event_hwnd as *mut _);
        if unsafe {
            windows::Win32::UI::WindowsAndMessaging::PostMessageW(
                Some(event_hwnd),
                WM_MONITOR_EVENT,
                WPARAM::default(),
                LPARAM(pointer as isize),
            )
        }
        .is_err()
        {
            unsafe {
                drop(Box::from_raw(pointer));
            }
        }
    })?;

    let taskbar_created = {
        let value = to_wide("TaskbarCreated");
        unsafe { RegisterWindowMessageW(pcwstr(&value)) }
    };
    let wts_registered = early_guard.wts_registered;
    let app = Box::new(App {
        hwnd,
        hinstance,
        settings,
        startup,
        monitor,
        blue_icon,
        red_icon,
        taskbar_created,
        wts_registered: AtomicBool::new(wts_registered),
        window_destroyed: AtomicBool::new(false),
        menu_open: AtomicBool::new(false),
        settings_dialog_open: AtomicBool::new(false),
        settings_dialog_hwnd: AtomicIsize::new(0),
        settings_thread: Mutex::new(None),
        password_visible: AtomicBool::new(false),
        default_password_char: AtomicU32::new(0x25CF),
        _instance: instance,
    });
    let app_pointer = Box::into_raw(app);
    early_guard.disarm();
    let mut app_guard = AppGuard {
        app_pointer,
        tray_added: false,
    };
    unsafe {
        SetWindowLongPtrW(hwnd, GWLP_USERDATA, app_pointer as isize);
    }
    let app_ref = unsafe { &*app_pointer };
    app_ref.add_tray_icon()?;
    app_guard.tray_added = true;

    if let Some(message) = settings_warning {
        app_ref.show_notification(&AppNotification {
            kind: NotificationKind::Warning,
            title: APP_TITLE.to_owned(),
            message,
        });
    }
    if let Some(message) = startup_warning {
        app_ref.show_notification(&AppNotification {
            kind: NotificationKind::Warning,
            title: APP_TITLE.to_owned(),
            message,
        });
    }
    if !auto_started && !app_ref.settings.snapshot().has_credentials() {
        app_ref.show_settings();
    }

    let mut message = MSG::default();
    loop {
        let result = unsafe { GetMessageW(&mut message, None, 0, 0) };
        if result.0 == -1 || !result.as_bool() {
            break;
        }
        unsafe {
            let _ = TranslateMessage(&message);
            DispatchMessageW(&message);
        }
    }

    drop(app_guard);
    Ok(())
}

pub fn show_fatal_error(message: &str) {
    use windows::Win32::UI::WindowsAndMessaging::{MB_ICONERROR, MB_OK};
    show_message(None, message, MB_OK | MB_ICONERROR);
}

unsafe extern "system" fn window_proc(
    hwnd: HWND,
    message: u32,
    wparam: WPARAM,
    lparam: LPARAM,
) -> LRESULT {
    let pointer = unsafe { GetWindowLongPtrW(hwnd, GWLP_USERDATA) } as *const App;
    if !pointer.is_null() {
        let app = unsafe { &*pointer };
        if message == app.taskbar_created && message != 0 {
            let _ = app.add_tray_icon();
            return LRESULT::default();
        }
        match message {
            WM_TRAY => {
                match lparam.0 as u32 {
                    WM_LBUTTONUP | WM_RBUTTONUP => app.show_context_menu(),
                    _ => {}
                }
                return LRESULT::default();
            }
            WM_COMMAND => {
                let command = (wparam.0 & 0xffff) as u32;
                if matches!(
                    command,
                    IDM_OPEN_PORTAL
                        | IDM_DETECT
                        | IDM_LOGIN
                        | IDM_LOGOUT
                        | IDM_PAUSE
                        | IDM_SETTINGS
                        | IDM_AUTOSTART
                        | IDM_EXIT
                ) {
                    app.execute_menu_command(command);
                    return LRESULT::default();
                }
            }
            WM_POWERBROADCAST => {
                let event = wparam.0 as u32;
                if event == PBT_APMRESUMEAUTOMATIC || event == PBT_APMRESUMESUSPEND {
                    app.monitor.notify_system_event(TriggerSource::Resume);
                    return LRESULT(1);
                }
            }
            WM_WTSSESSION_CHANGE => {
                if wparam.0 as u32 == WTS_SESSION_UNLOCK {
                    app.monitor.notify_system_event(TriggerSource::Unlock);
                    return LRESULT::default();
                }
            }
            WM_MONITOR_EVENT => {
                let event_pointer = lparam.0 as *mut MonitorEvent;
                if !event_pointer.is_null() {
                    let event = unsafe { *Box::from_raw(event_pointer) };
                    app.handle_monitor_event(event);
                }
                return LRESULT::default();
            }
            WM_SHOW_SETTINGS => {
                app.show_settings();
                return LRESULT::default();
            }
            WM_CLOSE => {
                app.monitor.request_stop();
                app.request_close_settings_dialog();
                if app.wts_registered.swap(false, Ordering::AcqRel) {
                    unsafe {
                        let _ = WTSUnRegisterSessionNotification(hwnd);
                    }
                }
                unsafe {
                    let _ = DestroyWindow(hwnd);
                }
                return LRESULT::default();
            }
            WM_DESTROY => {
                if app.wts_registered.swap(false, Ordering::AcqRel) {
                    unsafe {
                        let _ = WTSUnRegisterSessionNotification(hwnd);
                    }
                }
                app.window_destroyed.store(true, Ordering::Release);
                unsafe { PostQuitMessage(0) };
                return LRESULT::default();
            }
            _ => {}
        }
    }
    unsafe { DefWindowProcW(hwnd, message, wparam, lparam) }
}
