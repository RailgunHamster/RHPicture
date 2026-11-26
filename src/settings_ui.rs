use crate::config::{AppConfig, PinDoubleClickAction, ProxyConfig, SaveFormat, UpdateProvider};
use crate::hotkey::{
    HotkeySpec, MOD_FLAG_ALT, MOD_FLAG_CTRL, MOD_FLAG_SHIFT, MOD_FLAG_WIN,
};
use crate::pin;
use crate::updater::{APP_VERSION, UpdateService};
use crate::util::{
    OwnedBrush, OwnedFont, browse_folder_dialog, create_ui_font, current_hinstance, from_wide,
    get_auto_start, load_app_icon, process_memory_mb, rgb_colorref, scale_dpi, set_auto_start,
    to_wide, window_dpi,
};
use std::cell::{Cell, RefCell};
use std::sync::{Arc, Once};
use windows::Win32::Foundation::{COLORREF, HWND, LPARAM, LRESULT, RECT, WPARAM};
use windows::Win32::Graphics::Gdi::{HDC, InvalidateRect, SetBkColor, SetBkMode, SetTextColor, TRANSPARENT};
use windows::Win32::UI::Controls::{
    EM_SETREADONLY, ICC_BAR_CLASSES, ICC_PROGRESS_CLASS, ICC_STANDARD_CLASSES, ICC_TAB_CLASSES,
    INITCOMMONCONTROLSEX, InitCommonControlsEx, NMHDR, PBM_SETPOS, PBM_SETRANGE32, PROGRESS_CLASSW,
    TCIF_TEXT, TCITEMW, TCM_GETCURSEL, TCM_INSERTITEMW, TCM_SETCURSEL, TCN_SELCHANGE,
    WC_TABCONTROLW,
};
use windows::Win32::UI::Input::KeyboardAndMouse::{
    EnableWindow, GetAsyncKeyState, VK_BACK, VK_CONTROL, VK_DELETE, VK_ESCAPE, VK_LWIN, VK_MENU,
    VK_RWIN, VK_SHIFT,
};
use windows::Win32::UI::WindowsAndMessaging::{
    BM_GETCHECK, BM_SETCHECK, BS_AUTOCHECKBOX, BS_DEFPUSHBUTTON, BS_PUSHBUTTON, CB_ADDSTRING,
    CB_GETCURSEL, CB_SETCURSEL, CBS_DROPDOWNLIST, CallWindowProcW, CreateWindowExW,
    DefWindowProcW, DestroyWindow, ES_AUTOHSCROLL, ES_MULTILINE, ES_READONLY, ES_WANTRETURN,
    GWLP_WNDPROC, GetClientRect, GetSystemMetrics, GetWindowTextLengthW, GetWindowTextW, HMENU,
    HWND_BOTTOM, IDC_ARROW, KillTimer, LoadCursorW, RegisterClassExW, SM_CXSCREEN, SM_CYSCREEN,
    SW_HIDE, SW_RESTORE, SW_SHOW, SWP_NOACTIVATE, SWP_NOMOVE, SWP_NOSIZE, SendMessageW,
    SetForegroundWindow, SetTimer, SetWindowLongPtrW, SetWindowPos, SetWindowTextW, ShowWindow,
    WM_CLOSE, WM_COMMAND, WM_CTLCOLORBTN, WM_CTLCOLORSTATIC, WM_DESTROY, WM_GETFONT, WM_KEYDOWN,
    WM_KILLFOCUS, WM_NCDESTROY, WM_NOTIFY, WM_SETFOCUS, WM_SETFONT, WM_SYSKEYDOWN, WM_TIMER,
    WNDCLASSEXW, WNDPROC, WS_BORDER, WS_CAPTION, WS_CHILD, WS_CLIPSIBLINGS, WS_EX_CLIENTEDGE,
    WS_MINIMIZEBOX, WS_OVERLAPPED, WS_SYSMENU, WS_TABSTOP, WS_VISIBLE, WS_VSCROLL,
};
use windows::core::PCWSTR;

const SETTINGS_CLASS: &str = "RHPictureSettingsClass";
const TIMER_STATS: usize = 10;

// Control IDs
const ID_TAB: usize = 100;
const ID_BTN_SAVE: usize = 101;
const ID_BTN_CLOSE: usize = 102;
const ID_LBL_FOOTER: usize = 103;

// Tab 0: General & Hotkeys
const ID_EDIT_HK_SHOT: usize = 201;
const ID_BTN_CLR_SHOT: usize = 202;
const ID_EDIT_HK_PIN: usize = 203;
const ID_BTN_CLR_PIN: usize = 204;
const ID_EDIT_HK_TOGGLE: usize = 205;
const ID_BTN_CLR_TOGGLE: usize = 206;
const ID_CHK_AUTOSTART: usize = 207;
const ID_CHK_SMART_WIN: usize = 208;
const ID_CHK_MAGNIFIER: usize = 209;

// Tab 1: Pin & Save
const ID_EDIT_SAVE_DIR: usize = 301;
const ID_BTN_BROWSE_DIR: usize = 302;
const ID_CMB_FORMAT: usize = 303;
const ID_CHK_QUICK_SAVE: usize = 304;
const ID_CMB_PIN_DBLCLK: usize = 305;
const ID_CHK_PIN_BORDER: usize = 306;
const ID_LBL_MEM_STATS: usize = 307;
const ID_BTN_HIDE_PINS: usize = 308;
const ID_BTN_SHOW_PINS: usize = 309;
const ID_BTN_CLOSE_PINS: usize = 310;

// Tab 2: Updates
const ID_LBL_VERSION: usize = 401;
const ID_CMB_UPDATE_SRC: usize = 402;
const ID_EDIT_CNB_URL: usize = 403;
const ID_EDIT_GITHUB_URL: usize = 404;
const ID_CHK_PROXY: usize = 405;
const ID_CMB_PROXY_PROTO: usize = 406;
const ID_EDIT_PROXY_HOST: usize = 407;
const ID_EDIT_PROXY_PORT: usize = 408;
const ID_CHK_AUTO_UPDATE: usize = 409;
const ID_BTN_UPDATE_PRIMARY: usize = 410;
const ID_BTN_UPDATE_CHECK: usize = 411;
const ID_BTN_UPDATE_CANCEL: usize = 412;
const ID_PROGRESS_UPDATE: usize = 413;
const ID_LBL_UPDATE_STATUS: usize = 414;
const ID_EDIT_UPDATE_NOTES: usize = 415;

pub struct SettingsCallbacks {
    pub on_config_saved: Box<dyn Fn(AppConfig) -> Vec<String>>,
    pub on_suspend_hotkeys: Box<dyn Fn(bool)>,
    pub on_exit_for_update: Arc<dyn Fn() + Send + Sync>,
}

struct SettingsWindow {
    hwnd: HWND,
    tab_ctrl: HWND,
    active_tab: Cell<usize>,
    tab_controls: [Vec<HWND>; 3],
    controls: ControlHandles,
    font_ui: OwnedFont,
    font_bold: OwnedFont,
    bg_brush: OwnedBrush,
    updater: UpdateService,
    callbacks: SettingsCallbacks,
    orig_hotkey_procs: [WNDPROC; 3],
}

#[derive(Default)]
struct ControlHandles {
    footer_lbl: HWND,
    // Tab 0
    hk_shot: HWND,
    hk_pin: HWND,
    hk_toggle: HWND,
    chk_autostart: HWND,
    chk_smart_win: HWND,
    chk_magnifier: HWND,
    // Tab 1
    save_dir: HWND,
    cmb_format: HWND,
    chk_quick_save: HWND,
    cmb_pin_dblclk: HWND,
    chk_pin_border: HWND,
    mem_stats: HWND,
    // Tab 2
    lbl_version: HWND,
    cmb_update_src: HWND,
    edit_cnb_url: HWND,
    edit_github_url: HWND,
    chk_proxy: HWND,
    cmb_proxy_proto: HWND,
    edit_proxy_host: HWND,
    edit_proxy_port: HWND,
    chk_auto_update: HWND,
    btn_update_primary: HWND,
    btn_update_check: HWND,
    btn_update_cancel: HWND,
    progress_update: HWND,
    lbl_update_status: HWND,
    edit_update_notes: HWND,
}

thread_local! {
    static SETTINGS: RefCell<Option<Box<SettingsWindow>>> = const { RefCell::new(None) };
}

static REGISTER_CLASS: Once = Once::new();

fn ensure_registered() {
    REGISTER_CLASS.call_once(|| unsafe {
        let icc = INITCOMMONCONTROLSEX {
            dwSize: std::mem::size_of::<INITCOMMONCONTROLSEX>() as u32,
            dwICC: ICC_TAB_CLASSES | ICC_PROGRESS_CLASS | ICC_STANDARD_CLASSES | ICC_BAR_CLASSES,
        };
        let _ = InitCommonControlsEx(&icc);

        let class_w = to_wide(SETTINGS_CLASS);
        let wc = WNDCLASSEXW {
            cbSize: std::mem::size_of::<WNDCLASSEXW>() as u32,
            lpfnWndProc: Some(settings_wnd_proc),
            hInstance: current_hinstance(),
            hIcon: load_app_icon(32).unwrap_or_default(),
            hIconSm: load_app_icon(16).unwrap_or_default(),
            hCursor: LoadCursorW(None, IDC_ARROW).unwrap_or_default(),
            hbrBackground: windows::Win32::Graphics::Gdi::HBRUSH(
                (windows::Win32::Graphics::Gdi::COLOR_WINDOW.0 + 1) as *mut _,
            ),
            lpszClassName: PCWSTR(class_w.as_ptr()),
            ..Default::default()
        };
        let _ = RegisterClassExW(&wc);
    });
}

pub fn open_settings(
    config: &AppConfig,
    updater: UpdateService,
    initial_tab: Option<usize>,
    callbacks: SettingsCallbacks,
) {
    // If already open, bring to front and optionally switch tab (never hold borrow_mut across Win32 calls!)
    let existing = SETTINGS.with(|cell| {
        let borrow = cell.borrow();
        let win = borrow.as_ref()?;
        if let Some(t) = initial_tab {
            win.switch_tab(t.min(2));
        }
        win.refresh_memory_stats();
        win.refresh_update_ui();
        Some(win.hwnd)
    });
    if let Some(hwnd) = existing {
        unsafe {
            let _ = ShowWindow(hwnd, SW_RESTORE);
            let _ = ShowWindow(hwnd, SW_SHOW);
            let _ = SetForegroundWindow(hwnd);
        }
        return;
    }

    ensure_registered();

    let class_w = to_wide(SETTINGS_CLASS);
    let title_w = to_wide("RHPicture 设置");
    let win_w = 590;
    let win_h = 540;
    let sx = unsafe { GetSystemMetrics(SM_CXSCREEN) };
    let sy = unsafe { GetSystemMetrics(SM_CYSCREEN) };
    let x = ((sx - win_w) / 2).max(40);
    let y = ((sy - win_h) / 2).max(40);

    let Ok(hwnd) = (unsafe {
        CreateWindowExW(
            Default::default(),
            PCWSTR(class_w.as_ptr()),
            PCWSTR(title_w.as_ptr()),
            WS_OVERLAPPED | WS_CAPTION | WS_SYSMENU | WS_MINIMIZEBOX,
            x,
            y,
            win_w,
            win_h,
            None,
            None,
            Some(current_hinstance()),
            None,
        )
    }) else {
        return;
    };

    let dpi = window_dpi(hwnd);
    let scaled_w = scale_dpi(win_w, dpi);
    let scaled_h = scale_dpi(win_h, dpi);
    let sx_pos = ((sx - scaled_w) / 2).max(20);
    let sy_pos = ((sy - scaled_h) / 2).max(20);
    unsafe {
        let _ = SetWindowPos(hwnd, None, sx_pos, sy_pos, scaled_w, scaled_h, Default::default());
    }

    let font_ui = OwnedFont(create_ui_font(dpi, 9, false));
    let font_bold = OwnedFont(create_ui_font(dpi, 9, true));
    let bg_brush = OwnedBrush::solid(255, 255, 255);

    let mut win = Box::new(SettingsWindow {
        hwnd,
        tab_ctrl: HWND::default(),
        active_tab: Cell::new(initial_tab.unwrap_or(0).min(2)),
        tab_controls: [Vec::new(), Vec::new(), Vec::new()],
        controls: ControlHandles::default(),
        font_ui,
        font_bold,
        bg_brush,
        updater,
        callbacks,
        orig_hotkey_procs: [None, None, None],
    });

    win.build_ui(dpi, config);
    // Keep the tab control at the bottom of sibling Z-order so page controls always sit on top.
    unsafe {
        let _ = SetWindowPos(
            win.tab_ctrl,
            Some(HWND_BOTTOM),
            0,
            0,
            0,
            0,
            SWP_NOMOVE | SWP_NOSIZE | SWP_NOACTIVATE,
        );
    }
    let init_tab = win.active_tab.get();
    win.switch_tab(init_tab);
    win.refresh_memory_stats();
    win.refresh_update_ui();

    SETTINGS.with(|cell| {
        *cell.borrow_mut() = Some(win);
    });

    unsafe {
        let _ = SetTimer(Some(hwnd), TIMER_STATS, 1500, None);
        let _ = ShowWindow(hwnd, SW_SHOW);
        let _ = SetForegroundWindow(hwnd);
    }
}

pub fn notify_update_changed() {
    SETTINGS.with(|cell| {
        if let Ok(borrow) = cell.try_borrow() {
            if let Some(win) = borrow.as_ref() {
                win.refresh_update_ui();
            }
        }
    });
}

pub fn close_settings_if_open() {
    let hwnd = SETTINGS.with(|cell| cell.borrow().as_ref().map(|w| w.hwnd));
    if let Some(hwnd) = hwnd {
        unsafe {
            let _ = DestroyWindow(hwnd);
        }
    }
}

fn set_font(hwnd: HWND, font: windows::Win32::Graphics::Gdi::HFONT) {
    unsafe {
        let _ = SendMessageW(hwnd, WM_SETFONT, Some(WPARAM(font.0 as usize)), Some(LPARAM(1)));
    }
}

fn get_text(hwnd: HWND) -> String {
    unsafe {
        let len = GetWindowTextLengthW(hwnd);
        if len <= 0 {
            return String::new();
        }
        let mut buf = vec![0u16; (len + 1) as usize];
        GetWindowTextW(hwnd, &mut buf);
        from_wide(&buf)
    }
}

fn set_text(hwnd: HWND, text: &str) {
    let w = to_wide(text);
    unsafe {
        let _ = SetWindowTextW(hwnd, PCWSTR(w.as_ptr()));
    }
}

fn is_checked(hwnd: HWND) -> bool {
    unsafe { SendMessageW(hwnd, BM_GETCHECK, None, None).0 == 1 }
}

fn set_checked(hwnd: HWND, checked: bool) {
    unsafe {
        let _ = SendMessageW(hwnd, BM_SETCHECK, Some(WPARAM(if checked { 1 } else { 0 })), None);
    }
}

fn combo_add(hwnd: HWND, items: &[&str], selected: usize) {
    for item in items {
        let w = to_wide(item);
        unsafe {
            let _ = SendMessageW(hwnd, CB_ADDSTRING, None, Some(LPARAM(w.as_ptr() as isize)));
        }
    }
    unsafe {
        let _ = SendMessageW(hwnd, CB_SETCURSEL, Some(WPARAM(selected)), None);
    }
}

fn combo_sel(hwnd: HWND) -> usize {
    let idx = unsafe { SendMessageW(hwnd, CB_GETCURSEL, None, None).0 };
    if idx < 0 { 0 } else { idx as usize }
}

impl SettingsWindow {
    fn create_child(
        &self,
        class: &str,
        text: &str,
        style: u32,
        ex_style: u32,
        x: i32,
        y: i32,
        w: i32,
        h: i32,
        id: usize,
        bold: bool,
    ) -> HWND {
        let class_w = to_wide(class);
        let text_w = to_wide(text);
        let hwnd = unsafe {
            CreateWindowExW(
                windows::Win32::UI::WindowsAndMessaging::WINDOW_EX_STYLE(ex_style),
                PCWSTR(class_w.as_ptr()),
                PCWSTR(text_w.as_ptr()),
                windows::Win32::UI::WindowsAndMessaging::WINDOW_STYLE(style),
                x,
                y,
                w,
                h,
                Some(self.hwnd),
                Some(HMENU(id as *mut _)),
                Some(current_hinstance()),
                None,
            )
            .unwrap_or_default()
        };
        set_font(hwnd, if bold { self.font_bold.0 } else { self.font_ui.0 });
        hwnd
    }

    fn build_ui(&mut self, dpi: u32, cfg: &AppConfig) {
        let s = |v: i32| scale_dpi(v, dpi);
        let mut client = RECT::default();
        unsafe {
            let _ = GetClientRect(self.hwnd, &mut client);
        }
        let cw = client.right - client.left;
        let ch = client.bottom - client.top;

        let pad = s(14);
        let footer_h = s(44);

        // Tab control
        let tab_w = cw - pad * 2;
        let tab_h = ch - pad * 2 - footer_h;
        let tab_class = from_wide(unsafe { WC_TABCONTROLW.as_wide() });
        self.tab_ctrl = self.create_child(
            &tab_class,
            "",
            (WS_CHILD | WS_VISIBLE | WS_CLIPSIBLINGS | WS_TABSTOP).0,
            0,
            pad,
            pad,
            tab_w,
            tab_h,
            ID_TAB,
            false,
        );
        for (i, title) in ["快捷键与常规", "贴图与保存", "更新设置"].iter().enumerate() {
            let mut title_w = to_wide(title);
            let item = TCITEMW {
                mask: TCIF_TEXT,
                pszText: windows::core::PWSTR(title_w.as_mut_ptr()),
                ..Default::default()
            };
            unsafe {
                let _ = SendMessageW(
                    self.tab_ctrl,
                    TCM_INSERTITEMW,
                    Some(WPARAM(i)),
                    Some(LPARAM(&item as *const TCITEMW as isize)),
                );
            }
        }

        // Footer bar
        let fy = ch - footer_h;
        self.controls.footer_lbl = self.create_child(
            "STATIC",
            "修改设置后点击「保存设置」生效",
            (WS_CHILD | WS_VISIBLE).0,
            0,
            pad,
            fy + s(11),
            cw - pad * 2 - s(195),
            s(22),
            ID_LBL_FOOTER,
            false,
        );
        self.create_child(
            "BUTTON",
            "保存设置",
            (WS_CHILD | WS_VISIBLE | WS_TABSTOP).0 | BS_DEFPUSHBUTTON as u32,
            0,
            cw - pad - s(185),
            fy + s(6),
            s(88),
            s(30),
            ID_BTN_SAVE,
            true,
        );
        self.create_child(
            "BUTTON",
            "关闭",
            (WS_CHILD | WS_VISIBLE | WS_TABSTOP).0 | BS_PUSHBUTTON as u32,
            0,
            cw - pad - s(88),
            fy + s(6),
            s(88),
            s(30),
            ID_BTN_CLOSE,
            false,
        );

        let left = pad + s(18);
        let top = pad + s(40);
        let inner_w = tab_w - s(36);

        // ==================== TAB 0: 快捷键与常规 ====================
        {
            let mut list = Vec::new();
            let mut y = top;

            list.push(self.create_child(
                "STATIC",
                "全局快捷键（点击输入框后直接按下目标组合键，Backspace 清空）",
                (WS_CHILD).0,
                0,
                left,
                y,
                inner_w,
                s(20),
                0,
                true,
            ));
            y += s(28);

            let rows = [
                ("开始截图快捷键：", &cfg.screenshot_hotkey, ID_EDIT_HK_SHOT, ID_BTN_CLR_SHOT, 0usize),
                ("剪贴板贴图快捷键：", &cfg.pin_clipboard_hotkey, ID_EDIT_HK_PIN, ID_BTN_CLR_PIN, 1usize),
                ("隐藏/显示全部贴图：", &cfg.toggle_pins_hotkey, ID_EDIT_HK_TOGGLE, ID_BTN_CLR_TOGGLE, 2usize),
            ];
            for (label, val, edit_id, clr_id, slot_idx) in rows {
                list.push(self.create_child(
                    "STATIC",
                    label,
                    (WS_CHILD).0,
                    0,
                    left,
                    y + s(4),
                    s(145),
                    s(22),
                    0,
                    false,
                ));
                let display = if val.trim().is_empty() { "无" } else { val.as_str() };
                let edit = self.create_child(
                    "EDIT",
                    display,
                    (WS_CHILD | WS_TABSTOP | WS_BORDER).0 | ES_READONLY as u32,
                    WS_EX_CLIENTEDGE.0,
                    left + s(150),
                    y,
                    s(210),
                    s(26),
                    edit_id,
                    true,
                );
                let prev = unsafe {
                    let prev_ptr = SetWindowLongPtrW(edit, GWLP_WNDPROC, hotkey_edit_proc as *const () as isize);
                    if prev_ptr != 0 {
                        Some(std::mem::transmute::<isize, unsafe extern "system" fn(HWND, u32, WPARAM, LPARAM) -> LRESULT>(prev_ptr))
                    } else {
                        None
                    }
                };
                self.orig_hotkey_procs[slot_idx] = prev;
                match slot_idx {
                    0 => self.controls.hk_shot = edit,
                    1 => self.controls.hk_pin = edit,
                    _ => self.controls.hk_toggle = edit,
                }
                list.push(edit);

                let clr = self.create_child(
                    "BUTTON",
                    "清空",
                    (WS_CHILD | WS_TABSTOP).0 | BS_PUSHBUTTON as u32,
                    0,
                    left + s(370),
                    y,
                    s(60),
                    s(26),
                    clr_id,
                    false,
                );
                list.push(clr);
                y += s(36);
            }

            y += s(10);
            list.push(self.create_child(
                "STATIC",
                "常规选项",
                (WS_CHILD).0,
                0,
                left,
                y,
                inner_w,
                s(20),
                0,
                true,
            ));
            y += s(26);

            self.controls.chk_autostart = self.create_child(
                "BUTTON",
                "开机自动启动 RHPicture（后台常驻托盘，零显存占用）",
                (WS_CHILD | WS_TABSTOP).0 | BS_AUTOCHECKBOX as u32,
                0,
                left,
                y,
                inner_w,
                s(24),
                ID_CHK_AUTOSTART,
                false,
            );
            set_checked(self.controls.chk_autostart, get_auto_start() || cfg.auto_start);
            list.push(self.controls.chk_autostart);
            y += s(28);

            self.controls.chk_smart_win = self.create_child(
                "BUTTON",
                "截图时自动识别窗口边界（鼠标悬停自动框选，单击即可截取）",
                (WS_CHILD | WS_TABSTOP).0 | BS_AUTOCHECKBOX as u32,
                0,
                left,
                y,
                inner_w,
                s(24),
                ID_CHK_SMART_WIN,
                false,
            );
            set_checked(self.controls.chk_smart_win, cfg.smart_window_detect);
            list.push(self.controls.chk_smart_win);
            y += s(28);

            self.controls.chk_magnifier = self.create_child(
                "BUTTON",
                "截图时显示像素放大镜与取色器（悬停按 C 复制 #HEX 色值）",
                (WS_CHILD | WS_TABSTOP).0 | BS_AUTOCHECKBOX as u32,
                0,
                left,
                y,
                inner_w,
                s(24),
                ID_CHK_MAGNIFIER,
                false,
            );
            set_checked(self.controls.chk_magnifier, cfg.show_magnifier);
            list.push(self.controls.chk_magnifier);
            y += s(36);

            list.push(self.create_child(
                "STATIC",
                "快捷操作说明：\r\n• 截图时：双击选区/Enter/Ctrl+C 复制，F3/T 贴图，Ctrl+S 保存，右键重选，方向键 1px 微调\r\n• 贴图上：左键拖动，滚轮缩放，Ctrl+滚轮调透明度，双击隐藏，H 隐藏，R 旋转，Esc 关闭",
                (WS_CHILD).0,
                0,
                left,
                y,
                inner_w,
                s(64),
                0,
                false,
            ));

            self.tab_controls[0] = list;
        }

        // ==================== TAB 1: 贴图与保存 ====================
        {
            let mut list = Vec::new();
            let mut y = top;

            list.push(self.create_child(
                "STATIC",
                "本地保存设置",
                (WS_CHILD).0,
                0,
                left,
                y,
                inner_w,
                s(20),
                0,
                true,
            ));
            y += s(26);

            list.push(self.create_child(
                "STATIC",
                "默认保存目录：",
                (WS_CHILD).0,
                0,
                left,
                y + s(4),
                s(108),
                s(22),
                0,
                false,
            ));
            self.controls.save_dir = self.create_child(
                "EDIT",
                &cfg.save_dir,
                (WS_CHILD | WS_TABSTOP | WS_BORDER).0 | ES_AUTOHSCROLL as u32,
                WS_EX_CLIENTEDGE.0,
                left + s(112),
                y,
                inner_w - s(190),
                s(26),
                ID_EDIT_SAVE_DIR,
                false,
            );
            list.push(self.controls.save_dir);
            list.push(self.create_child(
                "BUTTON",
                "浏览…",
                (WS_CHILD | WS_TABSTOP).0 | BS_PUSHBUTTON as u32,
                0,
                left + inner_w - s(70),
                y,
                s(70),
                s(26),
                ID_BTN_BROWSE_DIR,
                false,
            ));
            y += s(36);

            list.push(self.create_child(
                "STATIC",
                "默认图像格式：",
                (WS_CHILD).0,
                0,
                left,
                y + s(4),
                s(108),
                s(22),
                0,
                false,
            ));
            self.controls.cmb_format = self.create_child(
                "COMBOBOX",
                "",
                (WS_CHILD | WS_TABSTOP | WS_VSCROLL).0 | CBS_DROPDOWNLIST as u32,
                0,
                left + s(112),
                y,
                s(200),
                s(120),
                ID_CMB_FORMAT,
                false,
            );
            combo_add(
                self.controls.cmb_format,
                &["PNG（无损清晰，推荐）", "JPEG（体积小）", "BMP（原始位图）"],
                cfg.save_format.index(),
            );
            list.push(self.controls.cmb_format);
            y += s(34);

            self.controls.chk_quick_save = self.create_child(
                "BUTTON",
                "点击「保存」时直接存入默认目录（跳过另存为弹窗）",
                (WS_CHILD | WS_TABSTOP).0 | BS_AUTOCHECKBOX as u32,
                0,
                left,
                y,
                inner_w,
                s(24),
                ID_CHK_QUICK_SAVE,
                false,
            );
            set_checked(self.controls.chk_quick_save, cfg.quick_save_no_dialog);
            list.push(self.controls.chk_quick_save);
            y += s(36);

            list.push(self.create_child(
                "STATIC",
                "贴图窗口行为",
                (WS_CHILD).0,
                0,
                left,
                y,
                inner_w,
                s(20),
                0,
                true,
            ));
            y += s(26);

            list.push(self.create_child(
                "STATIC",
                "双击贴图动作：",
                (WS_CHILD).0,
                0,
                left,
                y + s(4),
                s(108),
                s(22),
                0,
                false,
            ));
            self.controls.cmb_pin_dblclk = self.create_child(
                "COMBOBOX",
                "",
                (WS_CHILD | WS_TABSTOP | WS_VSCROLL).0 | CBS_DROPDOWNLIST as u32,
                0,
                left + s(112),
                y,
                s(280),
                s(120),
                ID_CMB_PIN_DBLCLK,
                false,
            );
            combo_add(
                self.controls.cmb_pin_dblclk,
                &[
                    "隐藏贴图（自动压缩省内存，随时恢复）",
                    "重置为 100% 原始尺寸",
                    "直接关闭贴图",
                ],
                cfg.pin_double_click.index(),
            );
            list.push(self.controls.cmb_pin_dblclk);
            y += s(34);

            self.controls.chk_pin_border = self.create_child(
                "BUTTON",
                "为贴图绘制 1px 细边框（防止白底截图融入背景）",
                (WS_CHILD | WS_TABSTOP).0 | BS_AUTOCHECKBOX as u32,
                0,
                left,
                y,
                inner_w,
                s(24),
                ID_CHK_PIN_BORDER,
                false,
            );
            set_checked(self.controls.chk_pin_border, cfg.pin_show_border);
            list.push(self.controls.chk_pin_border);
            y += s(38);

            list.push(self.create_child(
                "STATIC",
                "实时资源与贴图管理",
                (WS_CHILD).0,
                0,
                left,
                y,
                inner_w,
                s(20),
                0,
                true,
            ));
            y += s(24);

            self.controls.mem_stats = self.create_child(
                "STATIC",
                "内存统计加载中…",
                (WS_CHILD).0,
                0,
                left,
                y,
                inner_w,
                s(22),
                ID_LBL_MEM_STATS,
                false,
            );
            list.push(self.controls.mem_stats);
            y += s(30);

            list.push(self.create_child(
                "BUTTON",
                "隐藏全部贴图",
                (WS_CHILD | WS_TABSTOP).0 | BS_PUSHBUTTON as u32,
                0,
                left,
                y,
                s(110),
                s(28),
                ID_BTN_HIDE_PINS,
                false,
            ));
            list.push(self.create_child(
                "BUTTON",
                "显示全部贴图",
                (WS_CHILD | WS_TABSTOP).0 | BS_PUSHBUTTON as u32,
                0,
                left + s(120),
                y,
                s(110),
                s(28),
                ID_BTN_SHOW_PINS,
                false,
            ));
            list.push(self.create_child(
                "BUTTON",
                "关闭全部贴图",
                (WS_CHILD | WS_TABSTOP).0 | BS_PUSHBUTTON as u32,
                0,
                left + s(240),
                y,
                s(110),
                s(28),
                ID_BTN_CLOSE_PINS,
                false,
            ));

            self.tab_controls[1] = list;
        }

        // ==================== TAB 2: 更新设置 ====================
        {
            let mut list = Vec::new();
            let mut y = top;

            self.controls.lbl_version = self.create_child(
                "STATIC",
                &format!("当前版本：v{APP_VERSION}"),
                (WS_CHILD).0,
                0,
                left,
                y,
                inner_w,
                s(22),
                ID_LBL_VERSION,
                true,
            );
            list.push(self.controls.lbl_version);
            y += s(28);

            list.push(self.create_child(
                "STATIC",
                "首选更新源：",
                (WS_CHILD).0,
                0,
                left,
                y + s(4),
                s(98),
                s(22),
                0,
                false,
            ));
            self.controls.cmb_update_src = self.create_child(
                "COMBOBOX",
                "",
                (WS_CHILD | WS_TABSTOP | WS_VSCROLL).0 | CBS_DROPDOWNLIST as u32,
                0,
                left + s(102),
                y,
                s(290),
                s(100),
                ID_CMB_UPDATE_SRC,
                false,
            );
            combo_add(
                self.controls.cmb_update_src,
                &[
                    "优先 CNB（失败自动回退 GitHub）",
                    "优先 GitHub（失败自动回退 CNB）",
                ],
                cfg.update.preferred_source.index(),
            );
            list.push(self.controls.cmb_update_src);
            y += s(32);

            list.push(self.create_child(
                "STATIC",
                "CNB 地址：",
                (WS_CHILD).0,
                0,
                left,
                y + s(4),
                s(98),
                s(22),
                0,
                false,
            ));
            self.controls.edit_cnb_url = self.create_child(
                "EDIT",
                &cfg.update.cnb_url,
                (WS_CHILD | WS_TABSTOP | WS_BORDER).0 | ES_AUTOHSCROLL as u32,
                WS_EX_CLIENTEDGE.0,
                left + s(102),
                y,
                inner_w - s(102),
                s(24),
                ID_EDIT_CNB_URL,
                false,
            );
            list.push(self.controls.edit_cnb_url);
            y += s(30);

            list.push(self.create_child(
                "STATIC",
                "GitHub 地址：",
                (WS_CHILD).0,
                0,
                left,
                y + s(4),
                s(98),
                s(22),
                0,
                false,
            ));
            self.controls.edit_github_url = self.create_child(
                "EDIT",
                &cfg.update.github_url,
                (WS_CHILD | WS_TABSTOP | WS_BORDER).0 | ES_AUTOHSCROLL as u32,
                WS_EX_CLIENTEDGE.0,
                left + s(102),
                y,
                inner_w - s(102),
                s(24),
                ID_EDIT_GITHUB_URL,
                false,
            );
            list.push(self.controls.edit_github_url);
            y += s(32);

            // Proxy row
            self.controls.chk_proxy = self.create_child(
                "BUTTON",
                "更新代理",
                (WS_CHILD | WS_TABSTOP).0 | BS_AUTOCHECKBOX as u32,
                0,
                left,
                y + s(2),
                s(88),
                s(22),
                ID_CHK_PROXY,
                false,
            );
            set_checked(self.controls.chk_proxy, cfg.update.proxy.enabled);
            list.push(self.controls.chk_proxy);

            self.controls.cmb_proxy_proto = self.create_child(
                "COMBOBOX",
                "",
                (WS_CHILD | WS_TABSTOP | WS_VSCROLL).0 | CBS_DROPDOWNLIST as u32,
                0,
                left + s(102),
                y,
                s(76),
                s(90),
                ID_CMB_PROXY_PROTO,
                false,
            );
            combo_add(
                self.controls.cmb_proxy_proto,
                &["http", "https"],
                if cfg.update.proxy.protocol == "https" { 1 } else { 0 },
            );
            list.push(self.controls.cmb_proxy_proto);

            self.controls.edit_proxy_host = self.create_child(
                "EDIT",
                &cfg.update.proxy.host,
                (WS_CHILD | WS_TABSTOP | WS_BORDER).0 | ES_AUTOHSCROLL as u32,
                WS_EX_CLIENTEDGE.0,
                left + s(186),
                y,
                s(180),
                s(24),
                ID_EDIT_PROXY_HOST,
                false,
            );
            list.push(self.controls.edit_proxy_host);

            self.controls.edit_proxy_port = self.create_child(
                "EDIT",
                &cfg.update.proxy.port.to_string(),
                (WS_CHILD | WS_TABSTOP | WS_BORDER).0 | ES_AUTOHSCROLL as u32,
                WS_EX_CLIENTEDGE.0,
                left + s(374),
                y,
                s(72),
                s(24),
                ID_EDIT_PROXY_PORT,
                false,
            );
            list.push(self.controls.edit_proxy_port);
            y += s(30);

            self.controls.chk_auto_update = self.create_child(
                "BUTTON",
                "启动时自动检查更新",
                (WS_CHILD | WS_TABSTOP).0 | BS_AUTOCHECKBOX as u32,
                0,
                left,
                y,
                s(220),
                s(22),
                ID_CHK_AUTO_UPDATE,
                false,
            );
            set_checked(self.controls.chk_auto_update, cfg.update.auto_check_on_start);
            list.push(self.controls.chk_auto_update);
            y += s(30);

            // Action buttons
            self.controls.btn_update_primary = self.create_child(
                "BUTTON",
                "检查更新",
                (WS_CHILD | WS_TABSTOP).0 | BS_PUSHBUTTON as u32,
                0,
                left,
                y,
                s(160),
                s(28),
                ID_BTN_UPDATE_PRIMARY,
                true,
            );
            list.push(self.controls.btn_update_primary);

            self.controls.btn_update_check = self.create_child(
                "BUTTON",
                "重新检查",
                (WS_CHILD | WS_TABSTOP).0 | BS_PUSHBUTTON as u32,
                0,
                left + s(170),
                y,
                s(90),
                s(28),
                ID_BTN_UPDATE_CHECK,
                false,
            );
            list.push(self.controls.btn_update_check);

            self.controls.btn_update_cancel = self.create_child(
                "BUTTON",
                "取消",
                (WS_CHILD | WS_TABSTOP).0 | BS_PUSHBUTTON as u32,
                0,
                left + s(270),
                y,
                s(76),
                s(28),
                ID_BTN_UPDATE_CANCEL,
                false,
            );
            list.push(self.controls.btn_update_cancel);
            y += s(34);

            // Progress bar
            let prog_class = from_wide(unsafe { PROGRESS_CLASSW.as_wide() });
            self.controls.progress_update = self.create_child(
                &prog_class,
                "",
                (WS_CHILD).0,
                0,
                left,
                y,
                inner_w,
                s(16),
                ID_PROGRESS_UPDATE,
                false,
            );
            unsafe {
                let _ = SendMessageW(
                    self.controls.progress_update,
                    PBM_SETRANGE32,
                    Some(WPARAM(0)),
                    Some(LPARAM(100)),
                );
            }
            list.push(self.controls.progress_update);
            y += s(22);

            self.controls.lbl_update_status = self.create_child(
                "STATIC",
                "尚未检查更新",
                (WS_CHILD).0,
                0,
                left,
                y,
                inner_w,
                s(34),
                ID_LBL_UPDATE_STATUS,
                false,
            );
            list.push(self.controls.lbl_update_status);
            y += s(36);

            let notes_h = (pad + tab_h - s(14) - y).max(s(65));
            self.controls.edit_update_notes = self.create_child(
                "EDIT",
                "",
                (WS_CHILD | WS_BORDER | WS_VSCROLL).0
                    | ES_MULTILINE as u32
                    | ES_READONLY as u32
                    | ES_WANTRETURN as u32,
                WS_EX_CLIENTEDGE.0,
                left,
                y,
                inner_w,
                notes_h,
                ID_EDIT_UPDATE_NOTES,
                false,
            );
            unsafe {
                let _ = SendMessageW(self.controls.edit_update_notes, EM_SETREADONLY, Some(WPARAM(1)), None);
            }
            list.push(self.controls.edit_update_notes);

            self.tab_controls[2] = list;
        }
    }

    fn switch_tab(&self, index: usize) {
        let target = index.min(2);
        self.active_tab.set(target);
        unsafe {
            let _ = SendMessageW(
                self.tab_ctrl,
                TCM_SETCURSEL,
                Some(WPARAM(target)),
                None,
            );
        }
        for (i, group) in self.tab_controls.iter().enumerate() {
            let cmd = if i == target { SW_SHOW } else { SW_HIDE };
            for &ctrl in group {
                unsafe {
                    let _ = ShowWindow(ctrl, cmd);
                }
            }
        }
        unsafe {
            let _ = InvalidateRect(Some(self.hwnd), None, true);
        }
    }

    fn refresh_memory_stats(&self) {
        let mem = process_memory_mb();
        let (vis, hid) = pin::pin_counts();
        let text = format!(
            "当前内存：{mem:.1} MB  |  显存占用：0 MB（纯 CPU）  |  显示中贴图：{vis}  |  已隐藏：{hid}"
        );
        set_text(self.controls.mem_stats, &text);
    }

    fn refresh_update_ui(&self) {
        let snap = self.updater.snapshot();
        let mode_desc = if snap.managed {
            "Velopack 增量更新已启用"
        } else {
            "独立运行模式（支持检查更新；使用 Velopack 安装包可自动增量更新）"
        };
        set_text(
            self.controls.lbl_version,
            &format!("当前版本：v{} — {mode_desc}", snap.current_version),
        );
        set_text(self.controls.btn_update_primary, &snap.primary_button_label());
        set_text(self.controls.lbl_update_status, &snap.status);

        let notes_display = if snap.notes.trim().is_empty() {
            if let Some(ver) = &snap.available_version {
                format!("版本 v{ver} 未附带更新日志。")
            } else {
                "检查更新后将在此显示版本更新日志。\r\n\r\n提示：支持直接填写 CNB（https://cnb.cool/用户名/仓库）或 GitHub（https://github.com/用户名/仓库）地址，自动读取 releases.win.json 并优先走增量差分包更新。".into()
            }
        } else {
            snap.notes.replace("\r\n", "\n").replace('\n', "\r\n")
        };
        set_text(self.controls.edit_update_notes, &notes_display);

        unsafe {
            let _ = EnableWindow(self.controls.btn_update_primary, !snap.busy);
            let _ = EnableWindow(self.controls.btn_update_check, !snap.busy);
            let _ = EnableWindow(self.controls.btn_update_cancel, snap.busy && snap.cancellable);
            let _ = SendMessageW(
                self.controls.progress_update,
                PBM_SETPOS,
                Some(WPARAM(snap.progress.clamp(0, 100) as usize)),
                None,
            );
        }
    }

    fn collect_config(&self) -> Result<AppConfig, String> {
        let normalize_hk = |hwnd: HWND| -> Result<String, String> {
            let raw = get_text(hwnd);
            match HotkeySpec::parse(&raw)? {
                Some(spec) => Ok(spec.format()),
                None => Ok(String::new()),
            }
        };

        let proxy_port_text = get_text(self.controls.edit_proxy_port);
        let proxy_enabled = is_checked(self.controls.chk_proxy);
        let proxy_port = if proxy_enabled {
            proxy_port_text
                .trim()
                .parse::<u16>()
                .map_err(|_| "代理端口应为 1–65535".to_string())?
        } else {
            proxy_port_text.trim().parse::<u16>().unwrap_or(7890)
        };

        let proxy = ProxyConfig {
            enabled: proxy_enabled,
            protocol: if combo_sel(self.controls.cmb_proxy_proto) == 1 {
                "https".into()
            } else {
                "http".into()
            },
            host: get_text(self.controls.edit_proxy_host).trim().into(),
            port: proxy_port,
        };
        proxy.validate()?;

        let cnb_url = get_text(self.controls.edit_cnb_url).trim().to_string();
        let github_url = get_text(self.controls.edit_github_url).trim().to_string();
        if !cnb_url.is_empty() {
            crate::updater::normalize_mirror_url(&cnb_url, UpdateProvider::Cnb, false)?;
        }
        if !github_url.is_empty() {
            crate::updater::normalize_mirror_url(&github_url, UpdateProvider::Github, false)?;
        }

        let mut cfg = AppConfig::default();
        cfg.screenshot_hotkey = normalize_hk(self.controls.hk_shot)?;
        cfg.pin_clipboard_hotkey = normalize_hk(self.controls.hk_pin)?;
        cfg.toggle_pins_hotkey = normalize_hk(self.controls.hk_toggle)?;
        cfg.auto_start = is_checked(self.controls.chk_autostart);
        cfg.smart_window_detect = is_checked(self.controls.chk_smart_win);
        cfg.show_magnifier = is_checked(self.controls.chk_magnifier);
        cfg.save_dir = get_text(self.controls.save_dir).trim().into();
        cfg.save_format = SaveFormat::from_index(combo_sel(self.controls.cmb_format));
        cfg.quick_save_no_dialog = is_checked(self.controls.chk_quick_save);
        cfg.pin_double_click =
            PinDoubleClickAction::from_index(combo_sel(self.controls.cmb_pin_dblclk));
        cfg.pin_show_border = is_checked(self.controls.chk_pin_border);
        cfg.update.preferred_source =
            UpdateProvider::from_index(combo_sel(self.controls.cmb_update_src));
        cfg.update.cnb_url = cnb_url;
        cfg.update.github_url = github_url;
        cfg.update.auto_check_on_start = is_checked(self.controls.chk_auto_update);
        cfg.update.proxy = proxy;
        Ok(cfg)
    }

    fn save_config(&self) -> bool {
        let cfg = match self.collect_config() {
            Ok(c) => c,
            Err(err) => {
                set_text(self.controls.footer_lbl, &format!("错误：{err}"));
                return false;
            }
        };
        if let Err(err) = cfg.save() {
            set_text(self.controls.footer_lbl, &format!("保存失败：{err}"));
            return false;
        }
        if let Err(err) = set_auto_start(cfg.auto_start) {
            set_text(self.controls.footer_lbl, &format!("配置已保存，但自启设置失败：{err}"));
            return false;
        }
        self.updater.update_config(cfg.update.clone());
        let warnings = (self.callbacks.on_config_saved)(cfg);
        if warnings.is_empty() {
            set_text(self.controls.footer_lbl, "设置已保存并立即生效");
        } else {
            set_text(self.controls.footer_lbl, &warnings.join("；"));
        }
        true
    }
}

fn key_down(vk: u16) -> bool {
    unsafe { (GetAsyncKeyState(vk as i32) as u16 & 0x8000) != 0 }
}

unsafe extern "system" fn hotkey_edit_proc(
    hwnd: HWND,
    msg: u32,
    wparam: WPARAM,
    lparam: LPARAM,
) -> LRESULT {
    match msg {
        WM_SETFOCUS => {
            SETTINGS.with(|cell| {
                if let Some(win) = cell.borrow().as_ref() {
                    (win.callbacks.on_suspend_hotkeys)(true);
                    set_text(win.controls.footer_lbl, "请直接按下要绑定的组合键（Backspace 清空）");
                }
            });
        }
        WM_KILLFOCUS => {
            SETTINGS.with(|cell| {
                if let Some(win) = cell.borrow().as_ref() {
                    (win.callbacks.on_suspend_hotkeys)(false);
                }
            });
        }
        WM_KEYDOWN | WM_SYSKEYDOWN => {
            let vk = wparam.0 as u32;
            if vk == VK_ESCAPE.0 as u32 {
                return LRESULT(0);
            }
            if vk == VK_BACK.0 as u32 || vk == VK_DELETE.0 as u32 {
                set_text(hwnd, "无");
                return LRESULT(0);
            }
            if HotkeySpec::is_modifier_only_vk(vk) {
                return LRESULT(0);
            }
            if crate::hotkey::vk_to_key_name(vk).is_some() {
                let mut mods = 0u32;
                if key_down(VK_CONTROL.0) {
                    mods |= MOD_FLAG_CTRL;
                }
                if key_down(VK_MENU.0) {
                    mods |= MOD_FLAG_ALT;
                }
                if key_down(VK_SHIFT.0) {
                    mods |= MOD_FLAG_SHIFT;
                }
                if key_down(VK_LWIN.0) || key_down(VK_RWIN.0) {
                    mods |= MOD_FLAG_WIN;
                }
                let spec = HotkeySpec { modifiers: mods, vk };
                set_text(hwnd, &spec.format());
            }
            return LRESULT(0);
        }
        _ => {}
    }

    let orig = SETTINGS.with(|cell| {
        let borrow = cell.borrow();
        let win = borrow.as_ref()?;
        if hwnd == win.controls.hk_shot {
            win.orig_hotkey_procs[0]
        } else if hwnd == win.controls.hk_pin {
            win.orig_hotkey_procs[1]
        } else if hwnd == win.controls.hk_toggle {
            win.orig_hotkey_procs[2]
        } else {
            None
        }
    });
    if orig.is_some() {
        unsafe { CallWindowProcW(orig, hwnd, msg, wparam, lparam) }
    } else {
        unsafe { DefWindowProcW(hwnd, msg, wparam, lparam) }
    }
}

unsafe extern "system" fn settings_wnd_proc(
    hwnd: HWND,
    msg: u32,
    wparam: WPARAM,
    lparam: LPARAM,
) -> LRESULT {
    match msg {
        WM_CTLCOLORSTATIC | WM_CTLCOLORBTN => {
            let hdc = HDC(wparam.0 as *mut _);
            unsafe {
                SetBkMode(hdc, TRANSPARENT);
                SetBkColor(hdc, COLORREF(0x00FFFFFF));
                SetTextColor(hdc, rgb_colorref(30, 41, 59));
            }
            let brush = SETTINGS.with(|cell| {
                cell.try_borrow()
                    .ok()
                    .and_then(|b| b.as_ref().map(|w| w.bg_brush.0.0 as isize))
                    .unwrap_or(0)
            });
            if brush != 0 {
                return LRESULT(brush);
            }
        }
        WM_NOTIFY => {
            let nmhdr = unsafe { &*(lparam.0 as *const NMHDR) };
            if nmhdr.idFrom == ID_TAB && nmhdr.code == TCN_SELCHANGE {
                SETTINGS.with(|cell| {
                    if let Some(win) = cell.borrow().as_ref() {
                        let sel = unsafe { SendMessageW(win.tab_ctrl, TCM_GETCURSEL, None, None).0 };
                        if sel >= 0 {
                            win.switch_tab(sel as usize);
                            if sel == 1 {
                                win.refresh_memory_stats();
                            } else if sel == 2 {
                                win.refresh_update_ui();
                            }
                        }
                    }
                });
                return LRESULT(0);
            }
        }
        WM_COMMAND => {
            let id = (wparam.0 & 0xFFFF) as usize;
            match id {
                ID_BTN_SAVE => {
                    SETTINGS.with(|cell| {
                        if let Some(win) = cell.borrow().as_ref() {
                            win.save_config();
                        }
                    });
                    return LRESULT(0);
                }
                ID_BTN_CLOSE => {
                    unsafe {
                        let _ = DestroyWindow(hwnd);
                    }
                    return LRESULT(0);
                }
                ID_BTN_CLR_SHOT => {
                    SETTINGS.with(|cell| {
                        if let Some(win) = cell.borrow().as_ref() {
                            set_text(win.controls.hk_shot, "无");
                        }
                    });
                    return LRESULT(0);
                }
                ID_BTN_CLR_PIN => {
                    SETTINGS.with(|cell| {
                        if let Some(win) = cell.borrow().as_ref() {
                            set_text(win.controls.hk_pin, "无");
                        }
                    });
                    return LRESULT(0);
                }
                ID_BTN_CLR_TOGGLE => {
                    SETTINGS.with(|cell| {
                        if let Some(win) = cell.borrow().as_ref() {
                            set_text(win.controls.hk_toggle, "无");
                        }
                    });
                    return LRESULT(0);
                }
                ID_BTN_BROWSE_DIR => {
                    if let Some(dir) = browse_folder_dialog(Some(hwnd)) {
                        SETTINGS.with(|cell| {
                            if let Some(win) = cell.borrow().as_ref() {
                                set_text(win.controls.save_dir, &dir.to_string_lossy());
                            }
                        });
                    }
                    return LRESULT(0);
                }
                ID_BTN_HIDE_PINS => {
                    pin::hide_all_pins();
                    SETTINGS.with(|cell| {
                        if let Some(win) = cell.borrow().as_ref() {
                            win.refresh_memory_stats();
                        }
                    });
                    return LRESULT(0);
                }
                ID_BTN_SHOW_PINS => {
                    pin::show_all_pins();
                    SETTINGS.with(|cell| {
                        if let Some(win) = cell.borrow().as_ref() {
                            win.refresh_memory_stats();
                        }
                    });
                    return LRESULT(0);
                }
                ID_BTN_CLOSE_PINS => {
                    pin::close_all_pins();
                    SETTINGS.with(|cell| {
                        if let Some(win) = cell.borrow().as_ref() {
                            win.refresh_memory_stats();
                        }
                    });
                    return LRESULT(0);
                }
                ID_BTN_UPDATE_PRIMARY => {
                    let action = SETTINGS.with(|cell| {
                        let borrow = cell.borrow();
                        let win = borrow.as_ref()?;
                        if !win.save_config() {
                            return None;
                        }
                        Some((win.updater.clone(), win.callbacks.on_exit_for_update.clone()))
                    });
                    if let Some((updater, on_exit)) = action {
                        updater.start_download_or_install_async(on_exit);
                    }
                    return LRESULT(0);
                }
                ID_BTN_UPDATE_CHECK => {
                    let updater = SETTINGS.with(|cell| {
                        let borrow = cell.borrow();
                        let win = borrow.as_ref()?;
                        if !win.save_config() {
                            return None;
                        }
                        Some(win.updater.clone())
                    });
                    if let Some(updater) = updater {
                        updater.start_check_async();
                    }
                    return LRESULT(0);
                }
                ID_BTN_UPDATE_CANCEL => {
                    SETTINGS.with(|cell| {
                        if let Some(win) = cell.borrow().as_ref() {
                            win.updater.cancel();
                        }
                    });
                    return LRESULT(0);
                }
                _ => {}
            }
        }
        WM_TIMER => {
            if wparam.0 == TIMER_STATS {
                SETTINGS.with(|cell| {
                    if let Some(win) = cell.borrow().as_ref() {
                        win.refresh_memory_stats();
                    }
                });
                return LRESULT(0);
            }
        }
        WM_GETFONT => {
            let f = SETTINGS.with(|cell| {
                cell.borrow()
                    .as_ref()
                    .map(|w| w.font_ui.0.0 as isize)
                    .unwrap_or(0)
            });
            if f != 0 {
                return LRESULT(f);
            }
        }
        WM_CLOSE => {
            unsafe {
                let _ = DestroyWindow(hwnd);
            }
            return LRESULT(0);
        }
        WM_DESTROY => {
            unsafe {
                let _ = KillTimer(Some(hwnd), TIMER_STATS);
            }
            SETTINGS.with(|cell| {
                if let Some(win) = cell.borrow().as_ref() {
                    (win.callbacks.on_suspend_hotkeys)(false);
                }
            });
        }
        WM_NCDESTROY => {
            SETTINGS.with(|cell| {
                *cell.borrow_mut() = None;
            });
        }
        _ => {}
    }
    unsafe { DefWindowProcW(hwnd, msg, wparam, lparam) }
}
