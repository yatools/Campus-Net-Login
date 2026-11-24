use std::mem::size_of;
use std::sync::atomic::{AtomicBool, Ordering};

use windows::Win32::Foundation::{LPARAM, POINT, WPARAM};
use windows::Win32::UI::Shell::{
    NIF_ICON, NIF_INFO, NIF_MESSAGE, NIF_TIP, NIIF_ERROR, NIIF_INFO, NIIF_WARNING, NIM_ADD,
    NIM_DELETE, NIM_MODIFY, NOTIFY_ICON_DATA_FLAGS, NOTIFYICONDATAW, Shell_NotifyIconW,
};
use windows::Win32::UI::WindowsAndMessaging::{
    AppendMenuW, CreatePopupMenu, DestroyMenu, GetCursorPos, HICON, HMENU, IDYES, MB_DEFBUTTON2,
    MB_ICONERROR, MB_ICONWARNING, MB_OK, MB_YESNO, MENU_ITEM_FLAGS, MF_CHECKED, MF_GRAYED,
    MF_SEPARATOR, MF_STRING, PostMessageW, SetForegroundWindow, TPM_LEFTALIGN, TPM_RETURNCMD,
    TPM_RIGHTBUTTON, TrackPopupMenu, WM_CLOSE, WM_NULL,
};
use windows::core::PCWSTR;

use crate::AppResult;
use crate::monitor::{AppNotification, MonitorEvent, NetworkState, NotificationKind};
use crate::time_utils::display_local_time;
use crate::wide::{pcwstr, to_wide};

use super::system::{fill_wide_array, open_portal, show_message, trim_idle_working_set};
use super::{
    APP_TITLE, App, IDM_AUTOSTART, IDM_DETECT, IDM_EXIT, IDM_LOGIN, IDM_LOGOUT, IDM_OPEN_PORTAL,
    IDM_PAUSE, IDM_SETTINGS, TRAY_ID, WM_DIALOG_REFRESH, WM_TRAY,
};

struct MenuOpenGuard<'a>(&'a AtomicBool);

impl Drop for MenuOpenGuard<'_> {
    fn drop(&mut self) {
        self.0.store(false, Ordering::Release);
    }
}

impl App {
    pub(super) fn add_tray_icon(&self) -> AppResult<()> {
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

    pub(super) fn remove_tray_icon(&self) {
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

    pub(super) fn show_notification(&self, notification: &AppNotification) {
        if !notification.kind.is_enabled(&self.settings.snapshot()) {
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

    fn tray_data(&self, flags: NOTIFY_ICON_DATA_FLAGS) -> NOTIFYICONDATAW {
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

    pub(super) fn handle_monitor_event(&self, event: MonitorEvent) {
        match event {
            MonitorEvent::Status(snapshot) => {
                self.update_tray_status(snapshot.state, &snapshot.message);
                self.request_dialog_refresh();
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
            MonitorEvent::SettingsChanged(_) => self.request_dialog_refresh(),
        }
    }

    fn request_dialog_refresh(&self) {
        if let Some(dialog) = self.settings_dialog_handle() {
            unsafe {
                let _ = PostMessageW(
                    Some(dialog),
                    WM_DIALOG_REFRESH,
                    WPARAM::default(),
                    LPARAM::default(),
                );
            }
        }
    }

    pub(super) fn show_context_menu(&self) {
        if self.menu_open.swap(true, Ordering::AcqRel) {
            return;
        }
        let menu_guard = MenuOpenGuard(&self.menu_open);

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

        let mut command = 0u32;
        let mut point = POINT::default();
        if unsafe { GetCursorPos(&mut point) }.is_ok() {
            unsafe {
                let _ = SetForegroundWindow(self.hwnd);
            }
            command = unsafe {
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
            unsafe {
                let _ = PostMessageW(Some(self.hwnd), WM_NULL, WPARAM::default(), LPARAM::default());
            }
        }
        unsafe {
            let _ = DestroyMenu(menu);
        }
        drop(menu_guard);
        self.execute_menu_command(command);
    }

    pub(super) fn execute_menu_command(&self, command: u32) {
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
}

fn append_menu_text(menu: HMENU, flags: MENU_ITEM_FLAGS, id: usize, text: &str) {
    let value = to_wide(text);
    unsafe {
        let _ = AppendMenuW(menu, flags, id, pcwstr(&value));
    }
}

fn append_menu_separator(menu: HMENU) {
    unsafe {
        let _ = AppendMenuW(menu, MF_SEPARATOR, 0, PCWSTR::null());
    }
}
