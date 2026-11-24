use std::sync::atomic::Ordering;
use std::thread::{self, JoinHandle};

use windows::Win32::Foundation::{COLORREF, HINSTANCE, HWND, LPARAM, WPARAM};
use windows::Win32::Graphics::Gdi::{
    COLOR_3DFACE, COLOR_HIGHLIGHT, COLOR_WINDOWTEXT, GetSysColor, GetSysColorBrush, HDC,
    InvalidateRect, SetBkMode, SetTextColor, TRANSPARENT,
};
use windows::Win32::UI::Controls::{
    BST_CHECKED, BST_UNCHECKED, CheckDlgButton, DRAWITEMSTRUCT, EM_GETPASSWORDCHAR,
    EM_SETCUEBANNER, EM_SETPASSWORDCHAR, IsDlgButtonChecked, MEASUREITEMSTRUCT, UDM_SETBUDDY,
    UDM_SETPOS32, UDM_SETRANGE32,
};
use windows::Win32::UI::HiDpi::GetDpiForWindow;
use windows::Win32::UI::Input::KeyboardAndMouse::EnableWindow;
use windows::Win32::UI::Shell::SetWindowSubclass;
use windows::Win32::UI::WindowsAndMessaging::{
    BN_CLICKED, CB_ADDSTRING, CB_GETCURSEL, CB_SETCURSEL, DialogBoxParamW, EndDialog, GetDlgCtrlID,
    GetDlgItem, GetWindowLongPtrW, GetWindowTextW, HWND_BOTTOM, ICON_BIG, ICON_SMALL, IDCANCEL,
    IDOK, IDYES, MB_DEFBUTTON2, MB_ICONERROR, MB_ICONWARNING, MB_OK, MB_YESNO, PostMessageW,
    SWP_NOACTIVATE, SWP_NOMOVE, SWP_NOSIZE, SendMessageW, SetForegroundWindow, SetWindowLongPtrW,
    SetWindowPos, SetWindowTextW, WM_CLOSE, WM_COMMAND, WM_CTLCOLORSTATIC, WM_DISPLAYCHANGE,
    WM_DRAWITEM, WM_INITDIALOG, WM_MEASUREITEM, WM_SETICON, GWLP_USERDATA,
};
use zeroize::{Zeroize, Zeroizing};

use crate::AppResult;
use crate::credentials::encrypt_password;
use crate::monitor::NetworkState;
use crate::settings::Provider;
use crate::time_utils::display_local_time;
use crate::wide::{pcwstr, to_wide};

use super::controls::{
    centered_edit_proc, clear_button_proc, draw_clear_data_button, draw_cooldown_frame,
    draw_provider_item, is_high_contrast, provider_combo_proc, refresh_edit_frame, rgb,
    size_provider_items,
};
use super::system::{
    center_dialog_in_work_area, deactivate_text_services, initialize_common_controls, open_url,
    resource, show_message, trim_idle_working_set,
};
use super::{
    App, IDC_ACCOUNT, IDC_AUTOSTART, IDC_CLEAR_DATA, IDC_COOLDOWN, IDC_COOLDOWN_FRAME,
    IDC_COOLDOWN_SPIN, IDC_FALLBACK, IDC_GITHUB, IDC_INTERVAL, IDC_INTERVAL_SPIN, IDC_LAST_LOGIN,
    IDC_NOTIFY_FAILURE, IDC_NOTIFY_SUCCESS, IDC_PASSWORD, IDC_PASSWORD_TOGGLE, IDC_PROVIDER,
    IDC_SECURITY_NOTICE, IDC_STATUS, IDC_TRIGGER_AUTOSTART, IDC_TRIGGER_PERIODIC,
    IDC_TRIGGER_RESUME, IDC_TRIGGER_UNLOCK, IDD_SETTINGS, WM_DIALOG_REFRESH,
};

impl App {
    pub(super) fn settings_dialog_handle(&self) -> Option<HWND> {
        let value = self.settings_dialog_hwnd.load(Ordering::Acquire);
        (value != 0).then_some(HWND(value as *mut _))
    }

    pub(super) fn show_settings(&self) {
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

    pub(super) fn request_close_settings_dialog(&self) {
        if let Some(dialog) = self.settings_dialog_handle() {
            unsafe {
                let _ = PostMessageW(Some(dialog), WM_CLOSE, WPARAM::default(), LPARAM::default());
            }
        }
    }

    pub(super) fn close_settings_dialog_and_wait(&self) {
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
        // Decorative frame stays behind all inputs and never receives keyboard focus.
        if let Ok(frame) = unsafe { GetDlgItem(Some(dialog), IDC_COOLDOWN_FRAME) } {
            unsafe {
                let _ = SetWindowPos(
                    frame,
                    Some(HWND_BOTTOM),
                    0,
                    0,
                    0,
                    0,
                    SWP_NOMOVE | SWP_NOSIZE | SWP_NOACTIVATE,
                );
            }
        }
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
        self.password_visible.store(false, Ordering::Release);
        let password_char = unsafe { GetDlgItem(Some(dialog), IDC_PASSWORD) }
            .ok()
            .map(|edit| unsafe { SendMessageW(edit, EM_GETPASSWORDCHAR, None, None) }.0 as u32)
            .filter(|&character| character != 0)
            .unwrap_or(0x25CF);
        self.default_password_char
            .store(password_char, Ordering::Release);
        set_dialog_text(dialog, IDC_PASSWORD_TOGGLE, "显示");

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
        for id in [
            IDC_ACCOUNT,
            IDC_PASSWORD,
            IDC_INTERVAL,
            IDC_COOLDOWN,
            IDC_FALLBACK,
        ] {
            if let Ok(control) = unsafe { GetDlgItem(Some(dialog), id) } {
                unsafe {
                    let _ = SetWindowSubclass(control, Some(centered_edit_proc), 1, 0);
                }
                refresh_edit_frame(control);
            }
        }
        if let Ok(clear_button) = unsafe { GetDlgItem(Some(dialog), IDC_CLEAR_DATA as i32) } {
            unsafe {
                let _ = SetWindowSubclass(clear_button, Some(clear_button_proc), 2, 0);
            }
        }

        set_checkbox(dialog, IDC_AUTOSTART, settings.auto_start);
        set_checkbox(dialog, IDC_TRIGGER_PERIODIC, settings.trigger_periodic);
        set_checkbox(
            dialog,
            IDC_TRIGGER_AUTOSTART,
            settings.trigger_on_autostart,
        );
        set_checkbox(dialog, IDC_TRIGGER_RESUME, settings.trigger_on_resume);
        set_checkbox(dialog, IDC_TRIGGER_UNLOCK, settings.trigger_on_unlock);
        set_checkbox(
            dialog,
            IDC_NOTIFY_FAILURE,
            settings.failure_notifications_enabled,
        );
        set_checkbox(
            dialog,
            IDC_NOTIFY_SUCCESS,
            settings.success_notifications_enabled,
        );
        sync_periodic_controls_enabled(dialog);
        self.refresh_dialog_status(dialog);

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
            unsafe {
                let _ = SetWindowSubclass(combo, Some(provider_combo_proc), 3, 0);
            }
            size_provider_items(combo);
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
    }

    fn refresh_dialog_status(&self, dialog: HWND) {
        let settings = self.settings.snapshot();
        let current = self.monitor.current();
        set_dialog_text(
            dialog,
            IDC_LAST_LOGIN,
            &display_local_time(settings.last_successful_login_utc.as_deref()),
        );
        set_dialog_text(
            dialog,
            IDC_STATUS,
            &format!("{} — {}", current.state.display_name(), current.message),
        );
        if let Ok(status) = unsafe { GetDlgItem(Some(dialog), IDC_STATUS) } {
            unsafe {
                let _ = InvalidateRect(Some(status), None, true);
            }
        }
    }

    fn toggle_password_visibility(&self, dialog: HWND) {
        let next_visible = !self.password_visible.load(Ordering::Acquire);
        self.password_visible.store(next_visible, Ordering::Release);
        let mask_char = if next_visible {
            0usize
        } else {
            self.default_password_char.load(Ordering::Acquire) as usize
        };
        if let Ok(edit) = unsafe { GetDlgItem(Some(dialog), IDC_PASSWORD) } {
            unsafe {
                SendMessageW(edit, EM_SETPASSWORDCHAR, Some(WPARAM(mask_char)), None);
                let _ = InvalidateRect(Some(edit), None, true);
            }
        }
        set_dialog_text(
            dialog,
            IDC_PASSWORD_TOGGLE,
            if next_visible { "隐藏" } else { "显示" },
        );
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
        candidate.trigger_periodic = checkbox_checked(dialog, IDC_TRIGGER_PERIODIC);
        candidate.trigger_on_autostart = checkbox_checked(dialog, IDC_TRIGGER_AUTOSTART);
        candidate.trigger_on_resume = checkbox_checked(dialog, IDC_TRIGGER_RESUME);
        candidate.trigger_on_unlock = checkbox_checked(dialog, IDC_TRIGGER_UNLOCK);
        candidate.success_notifications_enabled = checkbox_checked(dialog, IDC_NOTIFY_SUCCESS);
        candidate.failure_notifications_enabled = checkbox_checked(dialog, IDC_NOTIFY_FAILURE);

        let password = get_dialog_secret(dialog, IDC_PASSWORD, 2048);
        if !password.is_empty() {
            candidate.encrypted_password = encrypt_password(&password)?;
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

        if let Err(message) = self.settings.update(|current| {
            // A login can finish while the dialog is saving preferences.
            candidate.last_successful_login_utc = current.last_successful_login_utc.clone();
            *current = candidate;
        }) {
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

fn sync_periodic_controls_enabled(dialog: HWND) {
    let enabled = checkbox_checked(dialog, IDC_TRIGGER_PERIODIC);
    for id in [
        IDC_INTERVAL,
        IDC_INTERVAL_SPIN,
        IDC_COOLDOWN,
        IDC_COOLDOWN_SPIN,
    ] {
        if let Ok(control) = unsafe { GetDlgItem(Some(dialog), id) } {
            unsafe {
                let _ = EnableWindow(control, enabled);
                let _ = InvalidateRect(Some(control), None, true);
            }
        }
    }
}

unsafe extern "system" fn settings_dialog_proc(
    dialog: HWND,
    message: u32,
    wparam: WPARAM,
    lparam: LPARAM,
) -> isize {
    // Fixed owner-draw controls are measured before WM_INITDIALOG.
    if message == WM_MEASUREITEM {
        let item = unsafe { &mut *(lparam.0 as *mut MEASUREITEMSTRUCT) };
        if item.CtlID == IDC_PROVIDER as u32 {
            let dpi = unsafe { GetDpiForWindow(dialog) }.max(96);
            item.itemHeight = (20 * dpi + 48) / 96;
            return 1;
        }
    }
    if message == WM_INITDIALOG {
        unsafe {
            SetWindowLongPtrW(dialog, GWLP_USERDATA, lparam.0);
        }
        let app = unsafe { &*(lparam.0 as *const App) };
        app.initialize_settings_dialog(dialog);
        center_dialog_in_work_area(dialog);
        return 1;
    }

    let pointer = unsafe { GetWindowLongPtrW(dialog, GWLP_USERDATA) } as *const App;
    if pointer.is_null() {
        return 0;
    }
    let app = unsafe { &*pointer };
    match message {
        WM_DIALOG_REFRESH => {
            app.refresh_dialog_status(dialog);
            return 1;
        }
        WM_COMMAND => {
            let command = (wparam.0 & 0xffff) as u16;
            let notification = ((wparam.0 >> 16) & 0xffff) as u32;
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
            if command == IDC_PASSWORD_TOGGLE as u16 && notification == BN_CLICKED {
                app.toggle_password_visibility(dialog);
                return 1;
            }
            if command == IDC_TRIGGER_PERIODIC as u16 && notification == BN_CLICKED {
                sync_periodic_controls_enabled(dialog);
                return 1;
            }
        }
        WM_CTLCOLORSTATIC => {
            let control = HWND(lparam.0 as *mut _);
            let control_id = unsafe { GetDlgCtrlID(control) };
            let high_contrast = is_high_contrast();
            let color = if control_id == IDC_SECURITY_NOTICE {
                Some(if high_contrast {
                    COLORREF(unsafe { GetSysColor(COLOR_HIGHLIGHT) })
                } else {
                    rgb(204, 122, 0)
                })
            } else if control_id == IDC_STATUS {
                if high_contrast {
                    match app.monitor.current().state {
                        NetworkState::Online => {
                            Some(COLORREF(unsafe { GetSysColor(COLOR_HIGHLIGHT) }))
                        }
                        NetworkState::AuthServerUnavailable
                        | NetworkState::LoginFailed
                        | NetworkState::LogoutFailed
                        | NetworkState::Paused => {
                            Some(COLORREF(unsafe { GetSysColor(COLOR_WINDOWTEXT) }))
                        }
                        _ => None,
                    }
                } else {
                    match app.monitor.current().state {
                        NetworkState::Online => Some(rgb(34, 139, 34)),
                        NetworkState::AuthServerUnavailable
                        | NetworkState::LoginFailed
                        | NetworkState::LogoutFailed => Some(rgb(178, 34, 34)),
                        NetworkState::Paused => Some(rgb(255, 140, 0)),
                        _ => None,
                    }
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
            if item.CtlID == IDC_COOLDOWN_FRAME as u32 {
                draw_cooldown_frame(item);
                return 1;
            }
            if item.CtlID == IDC_PROVIDER as u32 {
                draw_provider_item(item);
                return 1;
            }
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

fn set_checkbox(dialog: HWND, id: i32, checked: bool) {
    unsafe {
        let _ = CheckDlgButton(
            dialog,
            id,
            if checked { BST_CHECKED } else { BST_UNCHECKED },
        );
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

fn get_dialog_text(dialog: HWND, id: i32, capacity: usize) -> String {
    let mut buffer = vec![0u16; capacity.max(2)];
    let Ok(control) = (unsafe { GetDlgItem(Some(dialog), id) }) else {
        return String::new();
    };
    let length = unsafe { GetWindowTextW(control, &mut buffer) }.max(0) as usize;
    String::from_utf16_lossy(&buffer[..length])
}

fn get_dialog_secret(dialog: HWND, id: i32, capacity: usize) -> Zeroizing<String> {
    let mut buffer = Zeroizing::new(vec![0u16; capacity.max(2)]);
    let Ok(control) = (unsafe { GetDlgItem(Some(dialog), id) }) else {
        return Zeroizing::new(String::new());
    };
    let length = unsafe { GetWindowTextW(control, &mut buffer) }.max(0) as usize;
    let secret = Zeroizing::new(String::from_utf16_lossy(&buffer[..length]));
    buffer.zeroize();
    secret
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
