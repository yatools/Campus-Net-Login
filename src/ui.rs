use std::env;
use std::mem::size_of;
use std::sync::atomic::{AtomicBool, AtomicIsize, Ordering};
use std::sync::{Arc, Mutex};
use std::thread::{self, JoinHandle};

use windows::Win32::Foundation::{
    COLORREF, CloseHandle, ERROR_ALREADY_EXISTS, GetLastError, HANDLE, HINSTANCE, HWND, LPARAM,
    LRESULT, POINT, WPARAM,
};
use windows::Win32::Graphics::Gdi::{
    COLOR_3DFACE, CreateSolidBrush, DT_CENTER, DT_SINGLELINE, DT_VCENTER, DeleteObject,
    DrawFocusRect, DrawTextW, FillRect, FrameRect, GetMonitorInfoW, GetSysColorBrush, HDC, HGDIOBJ,
    InvalidateRect, MONITOR_DEFAULTTONEAREST, MONITORINFO, MonitorFromWindow, SelectObject,
    SetBkMode, SetTextColor, TRANSPARENT,
};
use windows::Win32::System::Com::{
    CLSCTX_INPROC_SERVER, COINIT_APARTMENTTHREADED, CoCreateInstance, CoFreeUnusedLibrariesEx,
    CoInitializeEx, CoUninitialize,
};
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::System::Threading::{
    CreateMutexW, GetCurrentProcess, SetProcessWorkingSetSize, Sleep,
};
use windows::Win32::UI::Controls::{
    BST_CHECKED, BST_UNCHECKED, CheckDlgButton, DRAWITEMSTRUCT, EM_SETCUEBANNER,
    ICC_STANDARD_CLASSES, INITCOMMONCONTROLSEX, InitCommonControlsEx, IsDlgButtonChecked,
    ODS_FOCUS, ODS_SELECTED, UDM_SETBUDDY, UDM_SETPOS32, UDM_SETRANGE32,
};
use windows::Win32::UI::Input::Ime::ImmDisableIME;
use windows::Win32::UI::Shell::{
    NIF_ICON, NIF_INFO, NIF_MESSAGE, NIF_TIP, NIIF_ERROR, NIIF_INFO, NIIF_WARNING, NIM_ADD,
    NIM_DELETE, NIM_MODIFY, NOTIFYICONDATAW, Shell_NotifyIconW, ShellExecuteW,
};
use windows::Win32::UI::TextServices::{CLSID_TF_ThreadMgr, ITfThreadMgr};
use windows::Win32::UI::WindowsAndMessaging::{
    AppendMenuW, CB_ADDSTRING, CB_GETCURSEL, CB_SETCURSEL, CBN_SELCHANGE, CreatePopupMenu,
    CreateWindowExW, DefWindowProcW, DestroyMenu, DestroyWindow, DialogBoxParamW, DispatchMessageW,
    EN_CHANGE, EndDialog, FindWindowW, GWLP_USERDATA, GetCursorPos, GetDlgCtrlID, GetDlgItem,
    GetMessageW, GetWindowRect, GetWindowTextW, HICON, ICON_BIG, ICON_SMALL, IDC_ARROW, IDCANCEL,
    IDOK, IDYES, IMAGE_ICON, LR_SHARED, LoadCursorW, LoadImageW, MB_DEFBUTTON2, MB_ICONERROR,
    MB_ICONWARNING, MB_OK, MB_YESNO, MENU_ITEM_FLAGS, MF_CHECKED, MF_GRAYED, MF_SEPARATOR,
    MF_STRING, MSG, MessageBoxW, PostMessageW, PostQuitMessage, RegisterClassExW,
    RegisterWindowMessageW, SW_SHOWNORMAL, SWP_NOACTIVATE, SWP_NOSIZE, SWP_NOZORDER, SendMessageW,
    SetForegroundWindow, SetWindowLongPtrW, SetWindowPos, SetWindowTextW, TPM_LEFTALIGN,
    TPM_RETURNCMD, TPM_RIGHTBUTTON, TrackPopupMenu, TranslateMessage, WINDOW_EX_STYLE, WM_APP,
    WM_CLOSE, WM_COMMAND, WM_CTLCOLORSTATIC, WM_DESTROY, WM_DISPLAYCHANGE, WM_DRAWITEM, WM_GETFONT,
    WM_INITDIALOG, WM_LBUTTONDBLCLK, WM_RBUTTONUP, WM_SETICON, WNDCLASSEXW, WS_OVERLAPPED,
};
use windows::core::{Error as WindowsError, PCWSTR};
use zeroize::Zeroize;

use crate::AppResult;
use crate::credentials::encrypt_password;
use crate::monitor::{
    AppNotification, MonitorEvent, MonitorHandle, NetworkState, NotificationKind,
};
use crate::network::CAMPUS_LOGIN_PAGE_URL;
use crate::settings::{Provider, SettingsManager, SettingsStore};
use crate::startup::StartupManager;
use crate::time_utils::display_local_time;
use crate::wide::{pcwstr, to_wide};

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
const IDC_LOGIN_ONCE: i32 = 1016;
const IDC_PAUSE_PERIODS: i32 = 1017;
const IDC_GITHUB: i32 = 1018;
const IDC_LAST_LOGIN: i32 = 1009;
const IDC_STATUS: i32 = 1010;
const IDC_CLEAR_DATA: u16 = 1011;
const IDC_SUBMITTED_ACCOUNT: i32 = 1012;
const IDC_INTERVAL_SPIN: i32 = 1013;
const IDC_COOLDOWN_SPIN: i32 = 1014;
const IDC_SECURITY_NOTICE: i32 = 1015;

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
const TRAY_ID: u32 = 1;

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

    let settings = Arc::new(SettingsManager::new(SettingsStore::system_default()?));
    let startup = StartupManager::new(
        env::current_exe().map_err(|error| format!("无法确定程序路径：{error}"))?,
    )?;
    let startup_warning = if settings.snapshot().auto_start {
        startup.enable().err()
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
            PostMessageW(
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
    let app = Box::new(App {
        hwnd,
        hinstance,
        settings,
        startup,
        monitor,
        blue_icon,
        red_icon,
        taskbar_created,
        settings_dialog_open: AtomicBool::new(false),
        settings_dialog_hwnd: AtomicIsize::new(0),
        settings_thread: Mutex::new(None),
        _instance: instance,
    });
    let app_pointer = Box::into_raw(app);
    unsafe {
        SetWindowLongPtrW(hwnd, GWLP_USERDATA, app_pointer as isize);
    }
    let app_ref = unsafe { &*app_pointer };
    app_ref.add_tray_icon()?;

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
        if result.0 == -1 {
            break;
        }
        if !result.as_bool() {
            break;
        }
        unsafe {
            let _ = TranslateMessage(&message);
            DispatchMessageW(&message);
        }
    }

    let app = unsafe { Box::from_raw(app_pointer) };
    app.close_settings_dialog_and_wait();
    app.remove_tray_icon();
    unsafe {
        SetWindowLongPtrW(hwnd, GWLP_USERDATA, 0);
    }
    drop(app);
    Ok(())
}

pub fn show_fatal_error(message: &str) {
    show_message(None, message, MB_OK | MB_ICONERROR);
}

struct App {
    hwnd: HWND,
    hinstance: HINSTANCE,
    settings: Arc<SettingsManager>,
    startup: StartupManager,
    monitor: MonitorHandle,
    blue_icon: HICON,
    red_icon: HICON,
    taskbar_created: u32,
    settings_dialog_open: AtomicBool,
    settings_dialog_hwnd: AtomicIsize,
    settings_thread: Mutex<Option<JoinHandle<()>>>,
    _instance: SingleInstance,
}

impl App {
    fn add_tray_icon(&self) -> AppResult<()> {
        let current = self.monitor.current();
        let mut data = self.tray_data(NIF_MESSAGE | NIF_ICON | NIF_TIP);
        data.uCallbackMessage = WM_TRAY;
        data.hIcon = self.icon_for_state(current.state);
        fill_wide_array(
            &mut data.szTip,
            &format!("{APP_TITLE}：{}", current.state.display_name()),
        );
        if unsafe { Shell_NotifyIconW(NIM_ADD, &data) }.as_bool() {
            Ok(())
        } else {
            Err("Windows 无法创建系统托盘图标。".to_owned())
        }
    }

    fn remove_tray_icon(&self) {
        let data = self.tray_data(Default::default());
        unsafe {
            let _ = Shell_NotifyIconW(NIM_DELETE, &data);
        }
    }

    fn update_tray_status(&self, state: NetworkState, message: &str) {
        let mut data = self.tray_data(NIF_ICON | NIF_TIP);
        data.hIcon = self.icon_for_state(state);
        fill_wide_array(
            &mut data.szTip,
            &format!("{APP_TITLE}：{}\n{message}", state.display_name()),
        );
        unsafe {
            let _ = Shell_NotifyIconW(NIM_MODIFY, &data);
        }
    }

    fn show_notification(&self, notification: &AppNotification) {
        if !self.settings.snapshot().notifications_enabled {
            return;
        }
        let mut data = self.tray_data(NIF_INFO);
        fill_wide_array(&mut data.szInfoTitle, &notification.title);
        fill_wide_array(&mut data.szInfo, &notification.message);
        data.dwInfoFlags = match notification.kind {
            NotificationKind::Error => NIIF_ERROR,
            NotificationKind::Warning => NIIF_WARNING,
            NotificationKind::Information | NotificationKind::Success => NIIF_INFO,
        };
        unsafe {
            let _ = Shell_NotifyIconW(NIM_MODIFY, &data);
        }
    }

    fn tray_data(
        &self,
        flags: windows::Win32::UI::Shell::NOTIFY_ICON_DATA_FLAGS,
    ) -> NOTIFYICONDATAW {
        NOTIFYICONDATAW {
            cbSize: size_of::<NOTIFYICONDATAW>() as u32,
            hWnd: self.hwnd,
            uID: TRAY_ID,
            uFlags: flags,
            ..Default::default()
        }
    }

    fn icon_for_state(&self, state: NetworkState) -> HICON {
        if state.uses_blue_icon() {
            self.blue_icon
        } else {
            self.red_icon
        }
    }

    fn handle_monitor_event(&self, event: MonitorEvent) {
        match event {
            MonitorEvent::Status(snapshot) => {
                self.update_tray_status(snapshot.state, &snapshot.message);
                self.update_open_dialog_status(snapshot.state, &snapshot.message);
                if matches!(
                    snapshot.state,
                    NetworkState::NotConfigured
                        | NetworkState::Online
                        | NetworkState::AuthServerUnavailable
                        | NetworkState::LoginFailed
                        | NetworkState::LogoutFailed
                        | NetworkState::Paused
                ) {
                    trim_idle_working_set();
                }
            }
            MonitorEvent::Notification(notification) => self.show_notification(&notification),
            MonitorEvent::SettingsChanged(settings) => {
                if let Some(dialog) = self.settings_dialog_handle() {
                    set_dialog_text(
                        dialog,
                        IDC_LAST_LOGIN,
                        &display_local_time(settings.last_successful_login_utc.as_deref()),
                    );
                }
            }
        }
    }

    fn settings_dialog_handle(&self) -> Option<HWND> {
        let value = self.settings_dialog_hwnd.load(Ordering::Acquire);
        (value != 0).then_some(HWND(value as *mut _))
    }

    fn update_open_dialog_status(&self, state: NetworkState, message: &str) {
        let Some(dialog) = self.settings_dialog_handle() else {
            return;
        };
        set_dialog_text(
            dialog,
            IDC_STATUS,
            &format!("{} — {message}", state.display_name()),
        );
        if let Ok(status) = unsafe { GetDlgItem(Some(dialog), IDC_STATUS) } {
            unsafe {
                let _ = InvalidateRect(Some(status), None, true);
            }
        }
    }

    fn show_context_menu(&self) {
        let Ok(menu) = (unsafe { CreatePopupMenu() }) else {
            return;
        };
        let current = self.monitor.current();
        let settings = self.settings.snapshot();
        append_menu_text(
            menu,
            MF_STRING | MF_GRAYED,
            0,
            &format!("状态：{}", current.state.display_name()),
        );
        append_menu_text(menu, MF_STRING | MF_GRAYED, 0, &current.message);
        append_menu_text(
            menu,
            MF_STRING | MF_GRAYED,
            0,
            &format!(
                "最近登录：{}",
                display_local_time(settings.last_successful_login_utc.as_deref())
            ),
        );
        append_menu_separator(menu);
        append_menu_text(
            menu,
            MF_STRING,
            IDM_OPEN_PORTAL as usize,
            "打开校园网登录页",
        );
        append_menu_text(menu, MF_STRING, IDM_DETECT as usize, "立即检测");
        append_menu_text(menu, MF_STRING, IDM_LOGIN as usize, "立即登录");
        let logout_flags = if current.state == NetworkState::LoggingOut {
            MF_STRING | MF_GRAYED
        } else {
            MF_STRING
        };
        append_menu_text(menu, logout_flags, IDM_LOGOUT as usize, "注销校园网");
        append_menu_text(
            menu,
            MF_STRING,
            IDM_PAUSE as usize,
            if self.monitor.is_paused() {
                "恢复自动登录"
            } else {
                "暂停自动登录"
            },
        );
        append_menu_separator(menu);
        append_menu_text(menu, MF_STRING, IDM_SETTINGS as usize, "设置…");
        let startup_flags = if self.startup.is_enabled() {
            MF_STRING | MF_CHECKED
        } else {
            MF_STRING
        };
        append_menu_text(menu, startup_flags, IDM_AUTOSTART as usize, "开机自动启动");
        append_menu_separator(menu);
        append_menu_text(menu, MF_STRING, IDM_EXIT as usize, "退出");

        let mut point = POINT::default();
        if unsafe { GetCursorPos(&mut point) }.is_ok() {
            unsafe {
                let _ = SetForegroundWindow(self.hwnd);
            }
            let command = unsafe {
                TrackPopupMenu(
                    menu,
                    TPM_LEFTALIGN | TPM_RIGHTBUTTON | TPM_RETURNCMD,
                    point.x,
                    point.y,
                    None,
                    self.hwnd,
                    None,
                )
            }
            .0 as u32;
            self.execute_menu_command(command);
        }
        unsafe {
            let _ = DestroyMenu(menu);
        }
    }

    fn execute_menu_command(&self, command: u32) {
        match command {
            IDM_OPEN_PORTAL => open_portal(self.hwnd),
            IDM_DETECT => self.monitor.request_detection(),
            IDM_LOGIN => self.monitor.request_manual_login(),
            IDM_LOGOUT => self.confirm_and_logout(),
            IDM_PAUSE => self.monitor.toggle_pause(),
            IDM_SETTINGS => self.show_settings(),
            IDM_AUTOSTART => self.toggle_startup(),
            IDM_EXIT => unsafe {
                let _ = PostMessageW(
                    Some(self.hwnd),
                    WM_CLOSE,
                    WPARAM::default(),
                    LPARAM::default(),
                );
            },
            _ => {}
        }
    }

    fn confirm_and_logout(&self) {
        let answer = show_message(
            Some(self.hwnd),
            "确定要注销当前校园网会话吗？\n\n注销成功后程序会暂停自动重连，避免刚注销又被自动登录。需要联网时，可从托盘选择“恢复自动登录”或“立即登录”。",
            MB_YESNO | MB_ICONWARNING | MB_DEFBUTTON2,
        );
        if answer == IDYES {
            self.monitor.request_logout();
        }
    }

    fn toggle_startup(&self) {
        let was_enabled = self.startup.is_enabled();
        let result = if was_enabled {
            self.startup.disable()
        } else {
            self.startup.enable()
        };
        if let Err(message) = result {
            show_message(Some(self.hwnd), &message, MB_OK | MB_ICONERROR);
            return;
        }

        if let Err(message) = self
            .settings
            .update(|settings| settings.auto_start = !was_enabled)
        {
            if was_enabled {
                let _ = self.startup.enable();
            } else {
                let _ = self.startup.disable();
            }
            show_message(Some(self.hwnd), &message, MB_OK | MB_ICONERROR);
        } else {
            self.monitor.notify_settings_changed();
        }
    }

    fn show_settings(&self) {
        self.reap_finished_settings_thread();
        if self.settings_dialog_open.swap(true, Ordering::AcqRel) {
            if let Some(dialog) = self.settings_dialog_handle() {
                unsafe {
                    let _ = SetForegroundWindow(dialog);
                }
            }
            return;
        }
        let app_address = self as *const App as isize;
        let instance = self.hinstance.0 as isize;
        let owner = self.hwnd.0 as isize;
        let worker = thread::Builder::new()
            .name("campus-settings".to_string())
            .stack_size(512 * 1024)
            .spawn(move || {
                initialize_common_controls();
                let app = unsafe { &*(app_address as *const App) };
                let result = unsafe {
                    DialogBoxParamW(
                        Some(HINSTANCE(instance as *mut _)),
                        resource(IDD_SETTINGS),
                        Some(HWND(owner as *mut _)),
                        Some(settings_dialog_proc),
                        LPARAM(app_address),
                    )
                };
                if result == -1 {
                    show_message(
                        Some(HWND(owner as *mut _)),
                        "Windows 无法打开设置窗口。",
                        MB_OK | MB_ICONERROR,
                    );
                }
                deactivate_text_services();
                app.settings_dialog_hwnd.store(0, Ordering::Release);
                app.settings_dialog_open.store(false, Ordering::Release);
                trim_idle_working_set();
            });
        match worker {
            Ok(worker) => {
                *self
                    .settings_thread
                    .lock()
                    .unwrap_or_else(|poisoned| poisoned.into_inner()) = Some(worker);
            }
            Err(error) => {
                self.settings_dialog_open.store(false, Ordering::Release);
                show_message(
                    Some(self.hwnd),
                    &format!("无法创建设置窗口：{error}"),
                    MB_OK | MB_ICONERROR,
                );
            }
        }
    }

    fn reap_finished_settings_thread(&self) {
        let mut slot = self
            .settings_thread
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        if slot.as_ref().is_some_and(JoinHandle::is_finished)
            && let Some(worker) = slot.take()
        {
            let _ = worker.join();
        }
    }

    fn request_close_settings_dialog(&self) {
        if let Some(dialog) = self.settings_dialog_handle() {
            unsafe {
                let _ = PostMessageW(Some(dialog), WM_CLOSE, WPARAM::default(), LPARAM::default());
            }
        }
    }

    fn close_settings_dialog_and_wait(&self) {
        self.request_close_settings_dialog();
        if let Some(worker) = self
            .settings_thread
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .take()
        {
            let _ = worker.join();
        }
    }

    fn initialize_settings_dialog(&self, dialog: HWND) {
        self.settings_dialog_hwnd
            .store(dialog.0 as isize, Ordering::Release);
        let settings = self.settings.snapshot();
        set_dialog_text(dialog, IDC_ACCOUNT, &settings.account);
        set_dialog_text(dialog, IDC_PASSWORD, "");
        set_edit_cue(
            dialog,
            IDC_PASSWORD,
            if settings.encrypted_password.is_empty() {
                "请输入密码"
            } else {
                "密码已保存，留空则不修改"
            },
        );
        configure_number_input(
            dialog,
            IDC_INTERVAL,
            IDC_INTERVAL_SPIN,
            10,
            3600,
            settings.check_interval_seconds,
        );
        configure_number_input(
            dialog,
            IDC_COOLDOWN,
            IDC_COOLDOWN_SPIN,
            -1,
            3600,
            settings.failure_cooldown_seconds,
        );
        set_dialog_text(dialog, IDC_FALLBACK, &settings.fallback_probe_url);
        set_dialog_text(dialog, IDC_PAUSE_PERIODS, &settings.pause_periods);
        unsafe {
            let _ = CheckDlgButton(
                dialog,
                IDC_LOGIN_ONCE,
                if settings.autostart_login_once {
                    BST_CHECKED
                } else {
                    BST_UNCHECKED
                },
            );
        }
        set_dialog_text(
            dialog,
            IDC_LAST_LOGIN,
            &display_local_time(settings.last_successful_login_utc.as_deref()),
        );
        let current = self.monitor.current();
        set_dialog_text(
            dialog,
            IDC_STATUS,
            &format!("{} — {}", current.state.display_name(), current.message),
        );

        unsafe {
            SendMessageW(
                dialog,
                WM_SETICON,
                Some(WPARAM(ICON_SMALL as usize)),
                Some(LPARAM(self.blue_icon.0 as isize)),
            );
            SendMessageW(
                dialog,
                WM_SETICON,
                Some(WPARAM(ICON_BIG as usize)),
                Some(LPARAM(self.blue_icon.0 as isize)),
            );
        }

        if let Ok(combo) = unsafe { GetDlgItem(Some(dialog), IDC_PROVIDER) } {
            for provider in Provider::ALL {
                let text = to_wide(provider.display_name());
                unsafe {
                    SendMessageW(
                        combo,
                        CB_ADDSTRING,
                        None,
                        Some(LPARAM(text.as_ptr() as isize)),
                    );
                }
            }
            unsafe {
                SendMessageW(
                    combo,
                    CB_SETCURSEL,
                    Some(WPARAM(settings.provider.index())),
                    None,
                );
            }
        }
        update_submitted_account_preview(dialog);
        unsafe {
            let _ = CheckDlgButton(
                dialog,
                IDC_AUTOSTART,
                if settings.auto_start {
                    BST_CHECKED
                } else {
                    BST_UNCHECKED
                },
            );
            let _ = CheckDlgButton(
                dialog,
                IDC_NOTIFY_SUCCESS,
                if settings.notifications_enabled {
                    BST_CHECKED
                } else {
                    BST_UNCHECKED
                },
            );
        }
    }

    fn save_settings_dialog(&self, dialog: HWND) -> AppResult<()> {
        let previous = self.settings.snapshot();
        let mut candidate = previous.clone();
        candidate.account = get_dialog_text(dialog, IDC_ACCOUNT, 512).trim().to_owned();
        candidate.fallback_probe_url = get_dialog_text(dialog, IDC_FALLBACK, 2048)
            .trim()
            .to_owned();
        candidate.check_interval_seconds = parse_dialog_i32(dialog, IDC_INTERVAL, "检测间隔")?;
        candidate.failure_cooldown_seconds = parse_dialog_i32(dialog, IDC_COOLDOWN, "失败冷却")?;
        candidate.provider = Provider::from_index(combo_selection(dialog, IDC_PROVIDER));
        candidate.auto_start = checkbox_checked(dialog, IDC_AUTOSTART);
        candidate.notifications_enabled = checkbox_checked(dialog, IDC_NOTIFY_SUCCESS);
        candidate.autostart_login_once = checkbox_checked(dialog, IDC_LOGIN_ONCE);
        candidate.pause_periods = get_dialog_text(dialog, IDC_PAUSE_PERIODS, 4096)
            .trim()
            .to_owned();

        let mut password = get_dialog_text(dialog, IDC_PASSWORD, 2048);
        if !password.is_empty() {
            let encrypted = encrypt_password(&password);
            password.zeroize();
            candidate.encrypted_password = encrypted?;
        } else {
            password.zeroize();
        }

        let errors = candidate.validate(candidate.encrypted_password.trim().is_empty());
        if !errors.is_empty() {
            return Err(errors.join("\n"));
        }

        let startup_was_enabled = self.startup.is_enabled();
        if candidate.auto_start {
            self.startup.enable()?;
        } else {
            self.startup.disable()?;
        }

        if let Err(message) = self.settings.replace(candidate) {
            if startup_was_enabled {
                let _ = self.startup.enable();
            } else {
                let _ = self.startup.disable();
            }
            return Err(message);
        }
        self.monitor.notify_settings_changed();
        Ok(())
    }

    fn clear_account_data(&self, dialog: HWND) {
        let answer = show_message(
            Some(dialog),
            "这会删除已保存的账号、加密密码和设置，并关闭程序。是否继续？",
            MB_YESNO | MB_ICONWARNING | MB_DEFBUTTON2,
        );
        if answer != IDYES {
            return;
        }
        if let Err(message) = self.startup.disable().and_then(|_| self.settings.clear()) {
            show_message(Some(dialog), &message, MB_OK | MB_ICONERROR);
            return;
        }
        unsafe {
            self.settings_dialog_hwnd.store(0, Ordering::Release);
            let _ = EndDialog(dialog, IDC_CLEAR_DATA as isize);
            let _ = PostMessageW(
                Some(self.hwnd),
                WM_CLOSE,
                WPARAM::default(),
                LPARAM::default(),
            );
        }
    }
}

unsafe extern "system" fn window_proc(
    hwnd: HWND,
    message: u32,
    wparam: WPARAM,
    lparam: LPARAM,
) -> LRESULT {
    let pointer =
        unsafe { windows::Win32::UI::WindowsAndMessaging::GetWindowLongPtrW(hwnd, GWLP_USERDATA) }
            as *const App;
    if !pointer.is_null() {
        let app = unsafe { &*pointer };
        if message == app.taskbar_created && message != 0 {
            let _ = app.add_tray_icon();
            return LRESULT::default();
        }
        match message {
            WM_TRAY => {
                match lparam.0 as u32 {
                    WM_LBUTTONDBLCLK => app.show_settings(),
                    WM_RBUTTONUP => app.show_context_menu(),
                    _ => {}
                }
                return LRESULT::default();
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
                app.request_close_settings_dialog();
                unsafe {
                    let _ = DestroyWindow(hwnd);
                }
                return LRESULT::default();
            }
            WM_DESTROY => {
                unsafe { PostQuitMessage(0) };
                return LRESULT::default();
            }
            _ => {}
        }
    }
    unsafe { DefWindowProcW(hwnd, message, wparam, lparam) }
}

unsafe extern "system" fn settings_dialog_proc(
    dialog: HWND,
    message: u32,
    wparam: WPARAM,
    lparam: LPARAM,
) -> isize {
    if message == WM_INITDIALOG {
        unsafe {
            SetWindowLongPtrW(dialog, GWLP_USERDATA, lparam.0);
        }
        let app = unsafe { &*(lparam.0 as *const App) };
        app.initialize_settings_dialog(dialog);
        center_dialog_in_work_area(dialog);
        return 1;
    }

    let pointer = unsafe {
        windows::Win32::UI::WindowsAndMessaging::GetWindowLongPtrW(dialog, GWLP_USERDATA)
    } as *const App;
    if pointer.is_null() {
        return 0;
    }
    let app = unsafe { &*pointer };
    match message {
        WM_COMMAND => {
            let command = (wparam.0 & 0xffff) as u16;
            let notification = ((wparam.0 >> 16) & 0xffff) as u16;
            if (command == IDC_ACCOUNT as u16 && notification == EN_CHANGE as u16)
                || (command == IDC_PROVIDER as u16 && notification == CBN_SELCHANGE as u16)
            {
                update_submitted_account_preview(dialog);
                return 1;
            }
            if command == IDOK.0 as u16 {
                match app.save_settings_dialog(dialog) {
                    Ok(()) => unsafe {
                        app.settings_dialog_hwnd.store(0, Ordering::Release);
                        let _ = EndDialog(dialog, IDOK.0 as isize);
                    },
                    Err(message) => {
                        show_message(Some(dialog), &message, MB_OK | MB_ICONERROR);
                    }
                }
                return 1;
            }
            if command == IDCANCEL.0 as u16 {
                unsafe {
                    app.settings_dialog_hwnd.store(0, Ordering::Release);
                    let _ = EndDialog(dialog, IDCANCEL.0 as isize);
                }
                return 1;
            }
            if command == IDC_GITHUB as u16 {
                open_url(dialog, crate::settings::PROJECT_URL);
                return 1;
            }
            if command == IDC_CLEAR_DATA {
                app.clear_account_data(dialog);
                return 1;
            }
        }
        WM_CTLCOLORSTATIC => {
            let control = HWND(lparam.0 as *mut _);
            let control_id = unsafe { GetDlgCtrlID(control) };
            let color = if control_id == IDC_SECURITY_NOTICE {
                Some(rgb(204, 122, 0))
            } else if control_id == IDC_STATUS {
                match app.monitor.current().state {
                    NetworkState::Online => Some(rgb(34, 139, 34)),
                    NetworkState::AuthServerUnavailable
                    | NetworkState::LoginFailed
                    | NetworkState::LogoutFailed => Some(rgb(178, 34, 34)),
                    NetworkState::Paused => Some(rgb(255, 140, 0)),
                    _ => None,
                }
            } else {
                None
            };
            if let Some(color) = color {
                let hdc = HDC(wparam.0 as *mut _);
                unsafe {
                    SetTextColor(hdc, color);
                    SetBkMode(hdc, TRANSPARENT);
                    return GetSysColorBrush(COLOR_3DFACE).0 as isize;
                }
            }
        }
        WM_DRAWITEM => {
            let item = unsafe { &*(lparam.0 as *const DRAWITEMSTRUCT) };
            if item.CtlID == IDC_CLEAR_DATA as u32 {
                draw_clear_data_button(item);
                return 1;
            }
        }
        WM_DISPLAYCHANGE => {
            center_dialog_in_work_area(dialog);
            return 1;
        }
        WM_CLOSE => {
            unsafe {
                app.settings_dialog_hwnd.store(0, Ordering::Release);
                let _ = EndDialog(dialog, IDCANCEL.0 as isize);
            }
            return 1;
        }
        _ => {}
    }
    0
}

struct SingleInstance {
    handle: HANDLE,
    already_exists: bool,
}

impl SingleInstance {
    fn acquire() -> AppResult<Self> {
        let name = to_wide(MUTEX_NAME);
        let handle = unsafe { CreateMutexW(None, false, pcwstr(&name)) }
            .map_err(|error| format!("无法创建单实例锁：{error}"))?;
        let already_exists = unsafe { GetLastError() } == ERROR_ALREADY_EXISTS;
        Ok(Self {
            handle,
            already_exists,
        })
    }
}

impl Drop for SingleInstance {
    fn drop(&mut self) {
        unsafe {
            let _ = CloseHandle(self.handle);
        }
    }
}

fn initialize_common_controls() {
    let controls = INITCOMMONCONTROLSEX {
        dwSize: size_of::<INITCOMMONCONTROLSEX>() as u32,
        dwICC: ICC_STANDARD_CLASSES,
    };
    unsafe {
        let _ = InitCommonControlsEx(&controls);
    }
}

fn center_dialog_in_work_area(dialog: HWND) {
    let monitor = unsafe { MonitorFromWindow(dialog, MONITOR_DEFAULTTONEAREST) };
    if monitor.0.is_null() {
        return;
    }
    let mut monitor_info = MONITORINFO {
        cbSize: size_of::<MONITORINFO>() as u32,
        ..Default::default()
    };
    if !unsafe { GetMonitorInfoW(monitor, &mut monitor_info) }.as_bool() {
        return;
    }
    let mut window = windows::Win32::Foundation::RECT::default();
    if unsafe { GetWindowRect(dialog, &mut window) }.is_err() {
        return;
    }
    let width = window.right - window.left;
    let height = window.bottom - window.top;
    let work_width = monitor_info.rcWork.right - monitor_info.rcWork.left;
    let work_height = monitor_info.rcWork.bottom - monitor_info.rcWork.top;
    let left = monitor_info.rcWork.left + (work_width - width).max(0) / 2;
    let top = monitor_info.rcWork.top + (work_height - height).max(0) / 2;
    unsafe {
        let _ = SetWindowPos(
            dialog,
            None,
            left,
            top,
            0,
            0,
            SWP_NOSIZE | SWP_NOZORDER | SWP_NOACTIVATE,
        );
    }
}

fn deactivate_text_services() {
    unsafe {
        let com_initialized = CoInitializeEx(None, COINIT_APARTMENTTHREADED).is_ok();
        if com_initialized {
            let manager: windows::core::Result<ITfThreadMgr> =
                CoCreateInstance(&CLSID_TF_ThreadMgr, None, CLSCTX_INPROC_SERVER);
            if let Ok(manager) = manager {
                let _ = manager.Deactivate();
            }
        }
        let _ = ImmDisableIME(u32::MAX);
        CoFreeUnusedLibrariesEx(0, None);
        if com_initialized {
            CoUninitialize();
        }
    }
}

fn register_window_class(instance: HINSTANCE) -> AppResult<()> {
    let class_name = to_wide(CLASS_NAME);
    let cursor = unsafe { LoadCursorW(None, IDC_ARROW) }
        .map_err(|error| format!("无法加载鼠标指针：{error}"))?;
    let class = WNDCLASSEXW {
        cbSize: size_of::<WNDCLASSEXW>() as u32,
        lpfnWndProc: Some(window_proc),
        hInstance: instance,
        hCursor: cursor,
        lpszClassName: pcwstr(&class_name),
        ..Default::default()
    };
    if unsafe { RegisterClassExW(&class) } == 0 {
        return Err(format!("无法注册程序窗口：{}", WindowsError::from_thread()));
    }
    Ok(())
}

fn load_icon(instance: HINSTANCE, id: usize) -> AppResult<HICON> {
    let handle = unsafe { LoadImageW(Some(instance), resource(id), IMAGE_ICON, 0, 0, LR_SHARED) }
        .map_err(|error| format!("无法加载程序图标：{error}"))?;
    Ok(HICON(handle.0))
}

fn notify_existing_instance() {
    let class_name = to_wide(CLASS_NAME);
    for _ in 0..20 {
        if let Ok(hwnd) = unsafe { FindWindowW(pcwstr(&class_name), PCWSTR::null()) } {
            unsafe {
                let _ = PostMessageW(
                    Some(hwnd),
                    WM_SHOW_SETTINGS,
                    WPARAM::default(),
                    LPARAM::default(),
                );
            }
            return;
        }
        unsafe { Sleep(50) };
    }
}

fn open_portal(hwnd: HWND) {
    open_url(hwnd, CAMPUS_LOGIN_PAGE_URL);
}

fn open_url(hwnd: HWND, address: &str) {
    let operation = to_wide("open");
    let url = to_wide(address);
    unsafe {
        let _ = ShellExecuteW(
            Some(hwnd),
            pcwstr(&operation),
            pcwstr(&url),
            PCWSTR::null(),
            PCWSTR::null(),
            SW_SHOWNORMAL,
        );
    }
}

fn append_menu_text(
    menu: windows::Win32::UI::WindowsAndMessaging::HMENU,
    flags: MENU_ITEM_FLAGS,
    id: usize,
    text: &str,
) {
    let value = to_wide(text);
    unsafe {
        let _ = AppendMenuW(menu, flags, id, pcwstr(&value));
    }
}

fn append_menu_separator(menu: windows::Win32::UI::WindowsAndMessaging::HMENU) {
    unsafe {
        let _ = AppendMenuW(menu, MF_SEPARATOR, 0, PCWSTR::null());
    }
}

fn set_dialog_text(dialog: HWND, id: i32, text: &str) {
    let value = to_wide(text);
    if let Ok(control) = unsafe { GetDlgItem(Some(dialog), id) } {
        unsafe {
            let _ = SetWindowTextW(control, pcwstr(&value));
        }
    }
}

fn set_edit_cue(dialog: HWND, id: i32, text: &str) {
    let value = to_wide(text);
    if let Ok(control) = unsafe { GetDlgItem(Some(dialog), id) } {
        unsafe {
            SendMessageW(
                control,
                EM_SETCUEBANNER,
                Some(WPARAM(1)),
                Some(LPARAM(value.as_ptr() as isize)),
            );
        }
    }
}

fn configure_number_input(
    dialog: HWND,
    edit_id: i32,
    spin_id: i32,
    minimum: i32,
    maximum: i32,
    value: i32,
) {
    let Ok(edit) = (unsafe { GetDlgItem(Some(dialog), edit_id) }) else {
        return;
    };
    let Ok(spin) = (unsafe { GetDlgItem(Some(dialog), spin_id) }) else {
        set_dialog_text(dialog, edit_id, &value.to_string());
        return;
    };
    unsafe {
        SendMessageW(spin, UDM_SETBUDDY, Some(WPARAM(edit.0 as usize)), None);
        SendMessageW(
            spin,
            UDM_SETRANGE32,
            Some(WPARAM(minimum as isize as usize)),
            Some(LPARAM(maximum as isize)),
        );
        SendMessageW(spin, UDM_SETPOS32, None, Some(LPARAM(value as isize)));
    }
}

fn update_submitted_account_preview(dialog: HWND) {
    let account = get_dialog_text(dialog, IDC_ACCOUNT, 512);
    let account = account.trim();
    let preview = if account.is_empty() {
        "—".to_string()
    } else {
        Provider::from_index(combo_selection(dialog, IDC_PROVIDER)).submitted_account(account)
    };
    set_dialog_text(dialog, IDC_SUBMITTED_ACCOUNT, &preview);
}

fn draw_clear_data_button(item: &DRAWITEMSTRUCT) {
    let selected = item.itemState.0 & ODS_SELECTED.0 != 0;
    let fill = unsafe {
        CreateSolidBrush(if selected {
            rgb(150, 28, 28)
        } else {
            rgb(190, 38, 38)
        })
    };
    let border = unsafe { CreateSolidBrush(rgb(145, 24, 24)) };
    unsafe {
        FillRect(item.hDC, &item.rcItem, fill);
        FrameRect(item.hDC, &item.rcItem, border);
    }

    let mut buffer = [0u16; 128];
    let length = unsafe { GetWindowTextW(item.hwndItem, &mut buffer) }.max(0) as usize;
    let mut text = buffer[..length].to_vec();
    let mut rectangle = item.rcItem;
    if selected {
        rectangle.left += 1;
        rectangle.top += 1;
    }
    let font = unsafe { SendMessageW(item.hwndItem, WM_GETFONT, None, None) }.0;
    let previous_font =
        (font != 0).then(|| unsafe { SelectObject(item.hDC, HGDIOBJ(font as *mut _)) });
    unsafe {
        SetBkMode(item.hDC, TRANSPARENT);
        SetTextColor(item.hDC, rgb(255, 255, 255));
        DrawTextW(
            item.hDC,
            &mut text,
            &mut rectangle,
            DT_CENTER | DT_VCENTER | DT_SINGLELINE,
        );
        if item.itemState.0 & ODS_FOCUS.0 != 0 {
            let mut focus = item.rcItem;
            focus.left += 3;
            focus.top += 3;
            focus.right -= 3;
            focus.bottom -= 3;
            let _ = DrawFocusRect(item.hDC, &focus);
        }
        if let Some(previous_font) = previous_font {
            SelectObject(item.hDC, previous_font);
        }
        let _ = DeleteObject(HGDIOBJ(fill.0));
        let _ = DeleteObject(HGDIOBJ(border.0));
    }
}

const fn rgb(red: u8, green: u8, blue: u8) -> COLORREF {
    COLORREF(red as u32 | ((green as u32) << 8) | ((blue as u32) << 16))
}

fn get_dialog_text(dialog: HWND, id: i32, capacity: usize) -> String {
    let mut buffer = vec![0u16; capacity.max(2)];
    let Ok(control) = (unsafe { GetDlgItem(Some(dialog), id) }) else {
        return String::new();
    };
    let length = unsafe { GetWindowTextW(control, &mut buffer) }.max(0) as usize;
    String::from_utf16_lossy(&buffer[..length])
}

fn parse_dialog_i32(dialog: HWND, id: i32, label: &str) -> AppResult<i32> {
    get_dialog_text(dialog, id, 64)
        .trim()
        .parse::<i32>()
        .map_err(|_| format!("{label}必须是整数。"))
}

fn combo_selection(dialog: HWND, id: i32) -> usize {
    let Ok(combo) = (unsafe { GetDlgItem(Some(dialog), id) }) else {
        return 0;
    };
    let value = unsafe { SendMessageW(combo, CB_GETCURSEL, None, None) }.0;
    if value < 0 { 0 } else { value as usize }
}

fn checkbox_checked(dialog: HWND, id: i32) -> bool {
    unsafe { IsDlgButtonChecked(dialog, id) == BST_CHECKED.0 }
}

fn show_message(
    hwnd: Option<HWND>,
    message: &str,
    style: windows::Win32::UI::WindowsAndMessaging::MESSAGEBOX_STYLE,
) -> windows::Win32::UI::WindowsAndMessaging::MESSAGEBOX_RESULT {
    let message = to_wide(message);
    let title = to_wide(APP_TITLE);
    unsafe { MessageBoxW(hwnd, pcwstr(&message), pcwstr(&title), style) }
}

fn fill_wide_array<const N: usize>(destination: &mut [u16; N], value: &str) {
    destination.fill(0);
    if N == 0 {
        return;
    }
    for (index, unit) in value.encode_utf16().take(N - 1).enumerate() {
        destination[index] = unit;
    }
}

fn resource(id: usize) -> PCWSTR {
    PCWSTR(id as *const u16)
}

fn trim_idle_working_set() {
    unsafe {
        let _ = SetProcessWorkingSetSize(GetCurrentProcess(), usize::MAX, usize::MAX);
    }
}
