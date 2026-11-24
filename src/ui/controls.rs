use std::mem::size_of;
use std::sync::atomic::{AtomicBool, Ordering};

use windows::Win32::Foundation::{COLORREF, HWND, LPARAM, LRESULT, POINT, RECT, WPARAM};
use windows::Win32::Graphics::Gdi::{
    COLOR_3DFACE, COLOR_BTNFACE, COLOR_BTNSHADOW, COLOR_BTNTEXT, COLOR_GRAYTEXT, COLOR_HIGHLIGHT,
    COLOR_HIGHLIGHTTEXT, COLOR_WINDOW, COLOR_WINDOWTEXT, ClientToScreen, CreateSolidBrush,
    DT_CENTER, DT_SINGLELINE, DT_VCENTER, DeleteObject, DrawFocusRect, DrawTextW, FillRect,
    FrameRect, GetDC, GetSysColor, GetSysColorBrush, GetTextMetricsW, GetWindowDC, HDC, HGDIOBJ,
    InvalidateRect, ReleaseDC, RestoreDC, SaveDC, SelectObject, SetBkMode, SetTextColor,
    TEXTMETRICW, TRANSPARENT,
};
use windows::Win32::UI::Accessibility::{HCF_HIGHCONTRASTON, HIGHCONTRASTW};
use windows::Win32::UI::Controls::{
    DRAWITEMSTRUCT, ODS_DISABLED, ODS_FOCUS, ODS_SELECTED, WM_MOUSELEAVE,
};
use windows::Win32::UI::Input::KeyboardAndMouse::{
    IsWindowEnabled, TME_LEAVE, TRACKMOUSEEVENT, TrackMouseEvent,
};
use windows::Win32::UI::Shell::{DefSubclassProc, RemoveWindowSubclass};
use windows::Win32::UI::WindowsAndMessaging::{
    CB_SETITEMHEIGHT, GetClientRect, GetWindowRect, GetWindowTextW, NCCALCSIZE_PARAMS,
    SPI_GETHIGHCONTRAST, SWP_FRAMECHANGED, SWP_NOACTIVATE, SWP_NOMOVE, SWP_NOSIZE, SWP_NOZORDER,
    SendMessageW, SetWindowPos, SystemParametersInfoW, WM_ENABLE, WM_GETFONT, WM_MOUSEMOVE,
    WM_NCCALCSIZE, WM_NCDESTROY, WM_NCPAINT, WM_PRINT, WM_SETFONT,
};

use crate::settings::Provider;

static CLEAR_BUTTON_HOVERED: AtomicBool = AtomicBool::new(false);

pub(super) fn is_high_contrast() -> bool {
    let mut high_contrast = HIGHCONTRASTW {
        cbSize: size_of::<HIGHCONTRASTW>() as u32,
        ..Default::default()
    };
    let ok = unsafe {
        SystemParametersInfoW(
            SPI_GETHIGHCONTRAST,
            high_contrast.cbSize,
            Some((&mut high_contrast as *mut HIGHCONTRASTW).cast()),
            Default::default(),
        )
    }
    .is_ok();
    ok && (high_contrast.dwFlags.0 & HCF_HIGHCONTRASTON.0) != 0
}

pub(super) const fn rgb(red: u8, green: u8, blue: u8) -> COLORREF {
    COLORREF(red as u32 | ((green as u32) << 8) | ((blue as u32) << 16))
}

pub(super) fn refresh_edit_frame(hwnd: HWND) {
    unsafe {
        let _ = SetWindowPos(
            hwnd,
            None,
            0,
            0,
            0,
            0,
            SWP_NOMOVE | SWP_NOSIZE | SWP_NOZORDER | SWP_NOACTIVATE | SWP_FRAMECHANGED,
        );
    }
}

fn paint_edit_padding(hwnd: HWND, dc: HDC) {
    let mut window = RECT::default();
    let mut client = RECT::default();
    let mut origin = POINT::default();
    unsafe {
        if GetWindowRect(hwnd, &mut window).is_err()
            || GetClientRect(hwnd, &mut client).is_err()
            || !ClientToScreen(hwnd, &mut origin).as_bool()
        {
            return;
        }
        let x = origin.x - window.left;
        let y = origin.y - window.top;
        let border = x.max(1);
        let top = RECT {
            left: x,
            top: border,
            right: x + client.right,
            bottom: y,
        };
        let bottom = RECT {
            left: x,
            top: y + client.bottom,
            right: x + client.right,
            bottom: window.bottom - window.top - border.max(2),
        };
        let brush = GetSysColorBrush(if IsWindowEnabled(hwnd).as_bool() {
            COLOR_WINDOW
        } else {
            COLOR_3DFACE
        });
        if top.bottom > top.top {
            FillRect(dc, &top, brush);
        }
        if bottom.bottom > bottom.top {
            FillRect(dc, &bottom, brush);
        }
    }
}

// Keep native single-line/password/cue behavior; center the client area on the font's actual height.
pub(super) unsafe extern "system" fn centered_edit_proc(
    hwnd: HWND,
    message: u32,
    wparam: WPARAM,
    lparam: LPARAM,
    id: usize,
    _data: usize,
) -> LRESULT {
    let result = unsafe { DefSubclassProc(hwnd, message, wparam, lparam) };
    match message {
        WM_NCCALCSIZE => {
            let rect = unsafe {
                if wparam.0 != 0 {
                    &mut (*(lparam.0 as *mut NCCALCSIZE_PARAMS)).rgrc[0]
                } else {
                    &mut *(lparam.0 as *mut RECT)
                }
            };
            let dc = unsafe { GetDC(Some(hwnd)) };
            if !dc.0.is_null() {
                let font = unsafe { SendMessageW(hwnd, WM_GETFONT, None, None) }.0;
                let previous =
                    (font != 0).then(|| unsafe { SelectObject(dc, HGDIOBJ(font as *mut _)) });
                let mut metrics = TEXTMETRICW::default();
                if unsafe { GetTextMetricsW(dc, &mut metrics) }.as_bool() {
                    let available = rect.bottom - rect.top;
                    let height = metrics.tmHeight.min(available).max(1);
                    rect.top += (available - height).max(0) / 2;
                    rect.bottom = rect.top + height;
                }
                unsafe {
                    if let Some(previous) = previous {
                        SelectObject(dc, previous);
                    }
                    ReleaseDC(Some(hwnd), dc);
                }
            }
            return LRESULT(0);
        }
        WM_NCPAINT => {
            let dc = unsafe { GetWindowDC(Some(hwnd)) };
            if !dc.0.is_null() {
                paint_edit_padding(hwnd, dc);
                unsafe {
                    ReleaseDC(Some(hwnd), dc);
                }
            }
        }
        WM_PRINT if lparam.0 & 2 != 0 => paint_edit_padding(hwnd, HDC(wparam.0 as *mut _)),
        WM_ENABLE => {
            refresh_edit_frame(hwnd);
            unsafe {
                let _ = InvalidateRect(Some(hwnd), None, true);
            }
        }
        WM_SETFONT => refresh_edit_frame(hwnd),
        WM_NCDESTROY => unsafe {
            let _ = RemoveWindowSubclass(hwnd, Some(centered_edit_proc), id);
        },
        _ => {}
    }
    result
}

pub(super) fn size_provider_items(hwnd: HWND) {
    unsafe {
        let dc = GetDC(Some(hwnd));
        if dc.0.is_null() {
            return;
        }
        let saved = SaveDC(dc);
        let font = SendMessageW(hwnd, WM_GETFONT, None, None).0;
        if font != 0 {
            SelectObject(dc, HGDIOBJ(font as *mut _));
        }
        let mut metrics = TEXTMETRICW::default();
        if GetTextMetricsW(dc, &mut metrics).as_bool() {
            let height = metrics.tmHeight + (metrics.tmHeight / 4).max(2);
            SendMessageW(
                hwnd,
                CB_SETITEMHEIGHT,
                Some(WPARAM(0)),
                Some(LPARAM(height as isize)),
            );
            SendMessageW(
                hwnd,
                CB_SETITEMHEIGHT,
                Some(WPARAM(usize::MAX)),
                Some(LPARAM(height as isize)),
            );
        }
        if saved != 0 {
            let _ = RestoreDC(dc, saved);
        }
        ReleaseDC(Some(hwnd), dc);
    }
}

pub(super) unsafe extern "system" fn provider_combo_proc(
    hwnd: HWND,
    message: u32,
    wparam: WPARAM,
    lparam: LPARAM,
    id: usize,
    _data: usize,
) -> LRESULT {
    let result = unsafe { DefSubclassProc(hwnd, message, wparam, lparam) };
    if message == WM_SETFONT {
        size_provider_items(hwnd);
    }
    if message == WM_NCDESTROY {
        unsafe {
            let _ = RemoveWindowSubclass(hwnd, Some(provider_combo_proc), id);
        }
    }
    result
}

pub(super) fn draw_provider_item(item: &DRAWITEMSTRUCT) {
    let selected = item.itemState.0 & ODS_SELECTED.0 != 0;
    let disabled = item.itemState.0 & ODS_DISABLED.0 != 0;
    unsafe {
        let saved = SaveDC(item.hDC);
        FillRect(
            item.hDC,
            &item.rcItem,
            GetSysColorBrush(if selected {
                COLOR_HIGHLIGHT
            } else {
                COLOR_WINDOW
            }),
        );
        let font = SendMessageW(item.hwndItem, WM_GETFONT, None, None).0;
        if font != 0 {
            SelectObject(item.hDC, HGDIOBJ(font as *mut _));
        }
        SetBkMode(item.hDC, TRANSPARENT);
        SetTextColor(
            item.hDC,
            COLORREF(GetSysColor(if disabled {
                COLOR_GRAYTEXT
            } else if selected {
                COLOR_HIGHLIGHTTEXT
            } else {
                COLOR_WINDOWTEXT
            })),
        );
        if let Some(provider) = Provider::ALL.get(item.itemID as usize) {
            let mut text: Vec<u16> = provider.display_name().encode_utf16().collect();
            let mut rect = item.rcItem;
            DrawTextW(
                item.hDC,
                &mut text,
                &mut rect,
                DT_CENTER | DT_VCENTER | DT_SINGLELINE,
            );
        }
        if item.itemState.0 & ODS_FOCUS.0 != 0 {
            let _ = DrawFocusRect(item.hDC, &item.rcItem);
        }
        if saved != 0 {
            let _ = RestoreDC(item.hDC, saved);
        }
    }
}

pub(super) unsafe extern "system" fn clear_button_proc(
    hwnd: HWND,
    message: u32,
    wparam: WPARAM,
    lparam: LPARAM,
    id: usize,
    _data: usize,
) -> LRESULT {
    match message {
        WM_MOUSEMOVE => {
            if !CLEAR_BUTTON_HOVERED.swap(true, Ordering::AcqRel) {
                let mut event = TRACKMOUSEEVENT {
                    cbSize: size_of::<TRACKMOUSEEVENT>() as u32,
                    dwFlags: TME_LEAVE,
                    hwndTrack: hwnd,
                    dwHoverTime: 0,
                };
                unsafe {
                    let _ = TrackMouseEvent(&mut event);
                    let _ = InvalidateRect(Some(hwnd), None, false);
                }
            }
        }
        WM_MOUSELEAVE => {
            if CLEAR_BUTTON_HOVERED.swap(false, Ordering::AcqRel) {
                unsafe {
                    let _ = InvalidateRect(Some(hwnd), None, false);
                }
            }
        }
        WM_NCDESTROY => {
            CLEAR_BUTTON_HOVERED.store(false, Ordering::Release);
            unsafe {
                let _ = RemoveWindowSubclass(hwnd, Some(clear_button_proc), id);
            }
        }
        _ => {}
    }
    unsafe { DefSubclassProc(hwnd, message, wparam, lparam) }
}

pub(super) fn draw_cooldown_frame(item: &DRAWITEMSTRUCT) {
    let rect = item.rcItem;
    let width = rect.right - rect.left;
    let height = rect.bottom - rect.top;
    // Resource geometry is 232 x 44 dialog units; keep the notch between the controls.
    let notch_x = rect.left + width * 90 / 232;
    let notch_y = rect.top + height / 2;
    let stroke = ((height + 44) / 88).max(1);
    let edges = [
        RECT {
            left: notch_x,
            top: rect.top,
            right: rect.right,
            bottom: rect.top + stroke,
        },
        RECT {
            left: rect.right - stroke,
            top: rect.top,
            right: rect.right,
            bottom: rect.bottom,
        },
        RECT {
            left: rect.left,
            top: rect.bottom - stroke,
            right: rect.right,
            bottom: rect.bottom,
        },
        RECT {
            left: rect.left,
            top: notch_y,
            right: rect.left + stroke,
            bottom: rect.bottom,
        },
        RECT {
            left: rect.left,
            top: notch_y,
            right: notch_x + stroke,
            bottom: notch_y + stroke,
        },
        RECT {
            left: notch_x,
            top: rect.top,
            right: notch_x + stroke,
            bottom: notch_y + stroke,
        },
    ];
    let color = COLORREF(unsafe {
        GetSysColor(if is_high_contrast() {
            COLOR_WINDOWTEXT
        } else {
            COLOR_BTNSHADOW
        })
    });
    unsafe {
        let brush = CreateSolidBrush(color);
        for edge in edges {
            FillRect(item.hDC, &edge, brush);
        }
        let _ = DeleteObject(HGDIOBJ(brush.0));
    }
}

pub(super) fn draw_clear_data_button(item: &DRAWITEMSTRUCT) {
    let pressed = item.itemState.0 & ODS_SELECTED.0 != 0;
    let hovered = CLEAR_BUTTON_HOVERED.load(Ordering::Acquire);
    let (fill_color, border_color, text_color) = if is_high_contrast() {
        let active = pressed || hovered;
        unsafe {
            (
                COLORREF(GetSysColor(if active {
                    COLOR_HIGHLIGHT
                } else {
                    COLOR_BTNFACE
                })),
                COLORREF(GetSysColor(if active {
                    COLOR_HIGHLIGHTTEXT
                } else {
                    COLOR_BTNTEXT
                })),
                COLORREF(GetSysColor(if active {
                    COLOR_HIGHLIGHTTEXT
                } else {
                    COLOR_BTNTEXT
                })),
            )
        }
    } else if pressed {
        (rgb(140, 20, 20), rgb(110, 16, 16), rgb(255, 255, 255))
    } else if hovered {
        (rgb(168, 28, 28), rgb(130, 20, 20), rgb(255, 255, 255))
    } else {
        (rgb(190, 38, 38), rgb(145, 24, 24), rgb(255, 255, 255))
    };

    let fill = unsafe { CreateSolidBrush(fill_color) };
    let border = unsafe { CreateSolidBrush(border_color) };
    unsafe {
        FillRect(item.hDC, &item.rcItem, fill);
        FrameRect(item.hDC, &item.rcItem, border);
    }

    let mut buffer = [0u16; 128];
    let length = unsafe { GetWindowTextW(item.hwndItem, &mut buffer) }.max(0) as usize;
    let mut text = buffer[..length].to_vec();
    let mut rectangle = item.rcItem;
    let font = unsafe { SendMessageW(item.hwndItem, WM_GETFONT, None, None) }.0;
    let previous_font =
        (font != 0).then(|| unsafe { SelectObject(item.hDC, HGDIOBJ(font as *mut _)) });
    unsafe {
        SetBkMode(item.hDC, TRANSPARENT);
        SetTextColor(item.hDC, text_color);
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
