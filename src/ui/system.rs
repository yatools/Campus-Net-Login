use std::mem::size_of;

use windows::Win32::Foundation::{
    CloseHandle, ERROR_ALREADY_EXISTS, GetLastError, HANDLE, HINSTANCE, HWND, LPARAM, RECT, WPARAM,
};
use windows::Win32::Graphics::Gdi::{
    GetMonitorInfoW, MONITOR_DEFAULTTONEAREST, MONITORINFO, MonitorFromWindow,
};
use windows::Win32::System::Com::{
    CLSCTX_INPROC_SERVER, COINIT_APARTMENTTHREADED, CoCreateInstance, CoFreeUnusedLibrariesEx,
    CoInitializeEx, CoUninitialize,
};
use windows::Win32::System::Threading::{
    CreateMutexW, GetCurrentProcess, SetProcessWorkingSetSize, Sleep,
};
use windows::Win32::UI::Controls::{
    ICC_STANDARD_CLASSES, INITCOMMONCONTROLSEX, InitCommonControlsEx,
};
use windows::Win32::UI::Input::Ime::ImmDisableIME;
use windows::Win32::UI::Shell::ShellExecuteW;
use windows::Win32::UI::TextServices::{CLSID_TF_ThreadMgr, ITfThreadMgr};
use windows::Win32::UI::WindowsAndMessaging::{
    FindWindowW, GetWindowRect, HICON, IDC_ARROW, IMAGE_ICON, LR_SHARED, LoadCursorW, LoadImageW,
    MB_ICONERROR, MB_OK, MESSAGEBOX_RESULT, MESSAGEBOX_STYLE, MessageBoxW, PostMessageW,
    RegisterClassExW, SW_SHOWNORMAL, SWP_NOACTIVATE, SWP_NOSIZE, SWP_NOZORDER, SetWindowPos,
    WNDCLASSEXW,
};
use windows::core::{Error as WindowsError, PCWSTR};

use crate::AppResult;
use crate::network::CAMPUS_LOGIN_PAGE_URL;
use crate::wide::{pcwstr, to_wide};

use super::{APP_TITLE, CLASS_NAME, MUTEX_NAME, WM_SHOW_SETTINGS, window_proc};

pub(super) struct SingleInstance {
    handle: HANDLE,
    pub(super) already_exists: bool,
}

impl SingleInstance {
    pub(super) fn acquire() -> AppResult<Self> {
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

pub(super) fn initialize_common_controls() {
    let controls = INITCOMMONCONTROLSEX {
        dwSize: size_of::<INITCOMMONCONTROLSEX>() as u32,
        dwICC: ICC_STANDARD_CLASSES,
    };
    unsafe {
        let _ = InitCommonControlsEx(&controls);
    }
}

pub(super) fn center_dialog_in_work_area(dialog: HWND) {
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
    let mut window = RECT::default();
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

pub(super) fn deactivate_text_services() {
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

pub(super) fn register_window_class(instance: HINSTANCE) -> AppResult<()> {
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

pub(super) fn load_icon(instance: HINSTANCE, id: usize) -> AppResult<HICON> {
    let handle = unsafe { LoadImageW(Some(instance), resource(id), IMAGE_ICON, 0, 0, LR_SHARED) }
        .map_err(|error| format!("无法加载程序图标：{error}"))?;
    Ok(HICON(handle.0))
}

pub(super) fn notify_existing_instance() {
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

pub(super) fn open_portal(hwnd: HWND) {
    open_url(hwnd, CAMPUS_LOGIN_PAGE_URL);
}

pub(super) fn open_url(hwnd: HWND, address: &str) {
    let operation = to_wide("open");
    let url = to_wide(address);
    let result = unsafe {
        ShellExecuteW(
            Some(hwnd),
            pcwstr(&operation),
            pcwstr(&url),
            PCWSTR::null(),
            PCWSTR::null(),
            SW_SHOWNORMAL,
        )
    };
    if (result.0 as isize) <= 32 {
        show_message(
            Some(hwnd),
            &format!("无法在默认浏览器中打开链接：\n{address}"),
            MB_OK | MB_ICONERROR,
        );
    }
}

pub(super) fn show_message(
    hwnd: Option<HWND>,
    message: &str,
    style: MESSAGEBOX_STYLE,
) -> MESSAGEBOX_RESULT {
    let message = to_wide(message);
    let title = to_wide(APP_TITLE);
    unsafe { MessageBoxW(hwnd, pcwstr(&message), pcwstr(&title), style) }
}

pub(super) fn fill_wide_array<const N: usize>(destination: &mut [u16; N], value: &str) {
    destination.fill(0);
    if N <= 1 {
        return;
    }
    let max_units = N - 1;
    let mut offset = 0usize;
    let mut utf16 = [0u16; 2];
    for character in value.chars() {
        let units = character.encode_utf16(&mut utf16);
        if offset + units.len() > max_units {
            break;
        }
        destination[offset..offset + units.len()].copy_from_slice(units);
        offset += units.len();
    }
}

pub(super) fn resource(id: usize) -> PCWSTR {
    PCWSTR(id as *const u16)
}

pub(super) fn trim_idle_working_set() {
    unsafe {
        let _ = SetProcessWorkingSetSize(GetCurrentProcess(), usize::MAX, usize::MAX);
    }
}

#[cfg(test)]
mod tests {
    use super::fill_wide_array;

    #[test]
    fn fill_wide_array_never_splits_surrogate_pair_at_boundary() {
        let mut buffer = [0u16; 4];
        fill_wide_array(&mut buffer, "ab😀");
        assert_eq!(buffer, ['a' as u16, 'b' as u16, 0, 0]);

        let mut larger = [0u16; 5];
        fill_wide_array(&mut larger, "ab😀c");
        let text = String::from_utf16(&larger[..4]).unwrap();
        assert_eq!(text, "ab😀");
        assert_eq!(larger[4], 0);
    }
}
