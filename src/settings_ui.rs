//! Modern Windows 11 dark-slate Fluent Settings window rendered purely on CPU (0 MB VRAM)
//! via tiny-skia vector cards/toggles/segmented pills + GDI ClearType typography, with borderless
//! native Win32 EDIT controls embedded cleanly inside rounded input frames.

use crate::capture::RectI;
use crate::config::{
    AppConfig, PinDoubleClickAction, ProxyConfig, SaveFormat, ThemeMode, UpdateProvider,
};
use crate::hotkey::{HotkeySpec, MOD_FLAG_ALT, MOD_FLAG_CTRL, MOD_FLAG_SHIFT, MOD_FLAG_WIN};
use crate::pin;
use crate::update_splash::draw_rhpicture_emblem;
use crate::updater::{APP_VERSION, UpdatePhase, UpdateService};
use crate::util::{
    DibSurface, OwnedBrush, OwnedFont, browse_folder_dialog, create_ui_font, current_hinstance,
    from_wide, get_auto_start, load_app_icon, process_memory_mb, rgb_colorref, scale_dpi,
    set_auto_start, to_wide, window_dpi,
};
use std::cell::{Cell, RefCell};
use std::sync::{Arc, Once};
use tiny_skia::{
    Color, FillRule, GradientStop, LineCap, LineJoin, LinearGradient, Paint, PathBuilder, Pixmap,
    Point, SpreadMode, Stroke, Transform,
};
use windows::Win32::Foundation::{COLORREF, HWND, LPARAM, LRESULT, RECT, WPARAM};
use windows::Win32::Graphics::Dwm::{
    DWMWA_CAPTION_COLOR, DWMWA_TEXT_COLOR, DWMWA_USE_IMMERSIVE_DARK_MODE,
    DWMWA_WINDOW_CORNER_PREFERENCE, DWMWCP_ROUND, DwmSetWindowAttribute,
};
use windows::Win32::Graphics::Gdi::{
    BeginPaint, BitBlt, DT_CALCRECT, DT_CENTER, DT_END_ELLIPSIS, DT_LEFT, DT_NOCLIP, DT_RIGHT,
    DT_SINGLELINE, DT_VCENTER, DT_WORDBREAK, DrawTextW, EndPaint, HDC, IntersectClipRect,
    InvalidateRect, PAINTSTRUCT, RestoreDC, SRCCOPY, SaveDC, SelectObject, SetBkColor, SetBkMode,
    SetTextColor, TRANSPARENT,
};
use windows::Win32::UI::Controls::WM_MOUSELEAVE;
use windows::Win32::UI::Input::KeyboardAndMouse::{
    GetAsyncKeyState, GetFocus, SetFocus, TME_LEAVE, TRACKMOUSEEVENT, TrackMouseEvent, VK_BACK,
    VK_CONTROL, VK_DELETE, VK_ESCAPE, VK_LWIN, VK_MENU, VK_RWIN, VK_SHIFT,
};
use windows::Win32::UI::WindowsAndMessaging::{
    CreateWindowExW, DefWindowProcW, DestroyWindow, EN_CHANGE, EN_KILLFOCUS, EN_SETFOCUS,
    ES_AUTOHSCROLL, GetClientRect, GetSystemMetrics, GetWindowTextLengthW, GetWindowTextW, HMENU,
    IDC_ARROW, IDC_HAND, KillTimer, LoadCursorW, RegisterClassExW, SM_CXSCREEN, SM_CYSCREEN,
    SW_HIDE, SW_RESTORE, SW_SHOW, SendMessageW, SetCursor, SetForegroundWindow, SetTimer,
    SetWindowPos, SetWindowTextW, ShowWindow, WM_CLOSE, WM_COMMAND, WM_CTLCOLOREDIT,
    WM_CTLCOLORSTATIC, WM_DESTROY, WM_ERASEBKGND, WM_KEYDOWN, WM_LBUTTONDOWN, WM_MOUSEMOVE,
    WM_MOUSEWHEEL, WM_NCDESTROY, WM_PAINT, WM_SETCURSOR, WM_SETFONT, WM_SYSKEYDOWN, WM_TIMER,
    WNDCLASSEXW, WS_CAPTION, WS_CHILD, WS_CLIPCHILDREN, WS_MINIMIZEBOX, WS_OVERLAPPED, WS_SYSMENU,
    WS_TABSTOP,
};
use windows::core::PCWSTR;

const SETTINGS_CLASS: &str = "RHPictureSettingsClass";
const TIMER_STATS: usize = 10;
const TIMER_AUTOSAVE: usize = 11;

const ID_EDIT_SAVE_DIR: usize = 301;
const ID_EDIT_CNB_URL: usize = 403;
const ID_EDIT_GITHUB_URL: usize = 404;
const ID_EDIT_PROXY_HOST: usize = 407;
const ID_EDIT_PROXY_PORT: usize = 408;

pub struct SettingsCallbacks {
    pub on_config_saved: Box<dyn Fn(AppConfig) -> Vec<String>>,
    pub on_suspend_hotkeys: Box<dyn Fn(bool)>,
    pub on_exit_for_update: Arc<dyn Fn() + Send + Sync>,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum HitKind {
    NavTab(usize),
    HotkeyRecord(usize),
    HotkeyClear(usize),
    ThemeSeg(usize),
    ToggleAutoStart,
    ToggleSmartWin,
    ToggleMagnifier,
    BrowseSaveDir,
    FormatSeg(usize),
    ToggleQuickSave,
    PinDblClkSeg(usize),
    TogglePinBorder,
    HideAllPins,
    ShowAllPins,
    CloseAllPins,
    UpdatePrimary,
    UpdateRecheck,
    UpdateCancel,
    UpdateSourceSeg(usize),
    ToggleAutoCheck,
    ToggleProxy,
    ProxyProtoSeg(usize),
}

#[derive(Clone, Copy)]
struct HitZone {
    rect: RectI,
    kind: HitKind,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum ToastTone {
    Muted,
    Success,
    Warning,
    Error,
}

#[derive(Default)]
struct EditHandles {
    save_dir: HWND,
    cnb_url: HWND,
    github_url: HWND,
    proxy_host: HWND,
    proxy_port: HWND,
}

struct SettingsWindow {
    hwnd: HWND,
    dpi: u32,
    client_w: i32,
    client_h: i32,
    active_tab: Cell<usize>,
    ready: Cell<bool>,
    pending_autosave: Cell<bool>,
    recording_slot: Cell<Option<usize>>,
    hovered: Cell<Option<HitKind>>,
    tracking_leave: Cell<bool>,
    focused_edit: Cell<Option<usize>>,
    notes_scroll: Cell<i32>,
    notes_max_scroll: Cell<i32>,

    cfg: RefCell<AppConfig>,
    toast: RefCell<(String, ToastTone)>,
    hits: RefCell<Vec<HitZone>>,
    edits: EditHandles,

    font_brand: OwnedFont,
    font_h1: OwnedFont,
    font_sec: OwnedFont,
    font_body: OwnedFont,
    font_body_bold: OwnedFont,
    font_small: OwnedFont,
    font_input: OwnedFont,
    edit_bg_light: OwnedBrush,
    edit_bg_dark: OwnedBrush,

    updater: UpdateService,
    callbacks: SettingsCallbacks,
}

#[derive(Clone, Copy)]
struct Palette {
    dark: bool,
    dwm_caption: (u8, u8, u8),
    dwm_text: (u8, u8, u8),
    sidebar_bg: Color,
    canvas_bg: Color,
    hairline: Color,
    card_bg: Color,
    card_sub_bg: Color,
    card_border: Color,
    divider: Color,
    row_hover: Color,
    nav_active_bg: Color,
    nav_active_bar: Color,
    nav_hover_bg: Color,
    nav_icon_active: Color,
    nav_icon_idle: Color,
    nav_txt_active: (u8, u8, u8),
    nav_txt_hover: (u8, u8, u8),
    nav_txt_idle: (u8, u8, u8),
    text_primary: (u8, u8, u8),
    text_body: (u8, u8, u8),
    text_secondary: (u8, u8, u8),
    text_muted: (u8, u8, u8),
    text_accent: (u8, u8, u8),
    text_success: (u8, u8, u8),
    text_warning: (u8, u8, u8),
    text_danger: (u8, u8, u8),
    text_danger_soft: (u8, u8, u8),
    input_bg: Color,
    input_bg_rgb: (u8, u8, u8),
    input_text_rgb: (u8, u8, u8),
    input_border: Color,
    input_border_focus: Color,
    keycap_bg: Color,
    keycap_hover_bg: Color,
    keycap_rec_bg: Color,
    keycap_border: Color,
    keycap_hover_border: Color,
    toggle_off_track: Color,
    toggle_off_border: Color,
    seg_bg: Color,
    seg_border: Color,
    seg_active_bg: Color,
    seg_hover_bg: Color,
    ghost_bg: Color,
    ghost_hover_bg: Color,
    ghost_border: Color,
    ghost_hover_border: Color,
    ghost_danger_bg: Color,
    ghost_danger_hover_bg: Color,
    ghost_danger_border: Color,
    ghost_danger_hover_border: Color,
    progress_track: Color,
    toast_muted_bg: Color,
    toast_muted_border: Color,
    badge_ok_bg: Color,
    badge_warn_bg: Color,
    badge_err_bg: Color,
}

impl Palette {
    fn for_dark(dark: bool) -> Self {
        if dark {
            Self::dark()
        } else {
            Self::light()
        }
    }

    fn light() -> Self {
        Self {
            dark: false,
            dwm_caption: (241, 245, 249),
            dwm_text: (15, 23, 42),
            sidebar_bg: Color::from_rgba8(241, 245, 249, 255),
            canvas_bg: Color::from_rgba8(248, 250, 252, 255),
            hairline: Color::from_rgba8(15, 23, 42, 22),
            card_bg: Color::from_rgba8(255, 255, 255, 255),
            card_sub_bg: Color::from_rgba8(255, 255, 255, 255),
            card_border: Color::from_rgba8(15, 23, 42, 24),
            divider: Color::from_rgba8(15, 23, 42, 16),
            row_hover: Color::from_rgba8(15, 23, 42, 8),
            nav_active_bg: Color::from_rgba8(14, 165, 233, 28),
            nav_active_bar: Color::from_rgba8(2, 132, 199, 255),
            nav_hover_bg: Color::from_rgba8(15, 23, 42, 10),
            nav_icon_active: Color::from_rgba8(2, 132, 199, 255),
            nav_icon_idle: Color::from_rgba8(100, 116, 139, 255),
            nav_txt_active: (3, 105, 161),
            nav_txt_hover: (15, 23, 42),
            nav_txt_idle: (71, 85, 105),
            text_primary: (15, 23, 42),
            text_body: (51, 65, 85),
            text_secondary: (100, 116, 139),
            text_muted: (100, 116, 139),
            text_accent: (2, 132, 199),
            text_success: (5, 150, 105),
            text_warning: (180, 83, 9),
            text_danger: (225, 29, 72),
            text_danger_soft: (190, 18, 60),
            input_bg: Color::from_rgba8(248, 250, 252, 255),
            input_bg_rgb: (248, 250, 252),
            input_text_rgb: (15, 23, 42),
            input_border: Color::from_rgba8(203, 213, 225, 255),
            input_border_focus: Color::from_rgba8(2, 132, 199, 255),
            keycap_bg: Color::from_rgba8(248, 250, 252, 255),
            keycap_hover_bg: Color::from_rgba8(241, 245, 249, 255),
            keycap_rec_bg: Color::from_rgba8(224, 242, 254, 255),
            keycap_border: Color::from_rgba8(203, 213, 225, 255),
            keycap_hover_border: Color::from_rgba8(148, 163, 184, 255),
            toggle_off_track: Color::from_rgba8(203, 213, 225, 255),
            toggle_off_border: Color::from_rgba8(15, 23, 42, 20),
            seg_bg: Color::from_rgba8(241, 245, 249, 255),
            seg_border: Color::from_rgba8(203, 213, 225, 255),
            seg_active_bg: Color::from_rgba8(14, 165, 233, 255),
            seg_hover_bg: Color::from_rgba8(15, 23, 42, 12),
            ghost_bg: Color::from_rgba8(255, 255, 255, 255),
            ghost_hover_bg: Color::from_rgba8(241, 245, 249, 255),
            ghost_border: Color::from_rgba8(203, 213, 225, 255),
            ghost_hover_border: Color::from_rgba8(148, 163, 184, 255),
            ghost_danger_bg: Color::from_rgba8(255, 241, 242, 255),
            ghost_danger_hover_bg: Color::from_rgba8(255, 228, 230, 255),
            ghost_danger_border: Color::from_rgba8(254, 205, 211, 255),
            ghost_danger_hover_border: Color::from_rgba8(251, 113, 133, 255),
            progress_track: Color::from_rgba8(226, 232, 240, 255),
            toast_muted_bg: Color::from_rgba8(255, 255, 255, 255),
            toast_muted_border: Color::from_rgba8(203, 213, 225, 255),
            badge_ok_bg: Color::from_rgba8(236, 253, 245, 255),
            badge_warn_bg: Color::from_rgba8(254, 243, 199, 255),
            badge_err_bg: Color::from_rgba8(255, 228, 230, 255),
        }
    }

    fn dark() -> Self {
        Self {
            dark: true,
            dwm_caption: (11, 17, 32),
            dwm_text: (226, 232, 240),
            sidebar_bg: Color::from_rgba8(11, 17, 32, 255),
            canvas_bg: Color::from_rgba8(15, 23, 42, 255),
            hairline: Color::from_rgba8(255, 255, 255, 18),
            card_bg: Color::from_rgba8(22, 32, 50, 255),
            card_sub_bg: Color::from_rgba8(18, 27, 45, 255),
            card_border: Color::from_rgba8(255, 255, 255, 18),
            divider: Color::from_rgba8(255, 255, 255, 14),
            row_hover: Color::from_rgba8(255, 255, 255, 9),
            nav_active_bg: Color::from_rgba8(56, 189, 248, 32),
            nav_active_bar: Color::from_rgba8(56, 189, 248, 255),
            nav_hover_bg: Color::from_rgba8(255, 255, 255, 12),
            nav_icon_active: Color::from_rgba8(56, 189, 248, 255),
            nav_icon_idle: Color::from_rgba8(148, 163, 184, 255),
            nav_txt_active: (248, 250, 252),
            nav_txt_hover: (226, 232, 240),
            nav_txt_idle: (148, 163, 184),
            text_primary: (248, 250, 252),
            text_body: (203, 213, 225),
            text_secondary: (148, 163, 184),
            text_muted: (100, 116, 139),
            text_accent: (125, 211, 252),
            text_success: (52, 211, 153),
            text_warning: (251, 191, 36),
            text_danger: (251, 113, 133),
            text_danger_soft: (248, 165, 178),
            input_bg: Color::from_rgba8(14, 22, 39, 255),
            input_bg_rgb: (14, 22, 39),
            input_text_rgb: (241, 245, 249),
            input_border: Color::from_rgba8(42, 56, 82, 255),
            input_border_focus: Color::from_rgba8(56, 189, 248, 255),
            keycap_bg: Color::from_rgba8(14, 22, 39, 255),
            keycap_hover_bg: Color::from_rgba8(30, 41, 59, 255),
            keycap_rec_bg: Color::from_rgba8(14, 165, 233, 42),
            keycap_border: Color::from_rgba8(42, 56, 82, 255),
            keycap_hover_border: Color::from_rgba8(71, 85, 105, 255),
            toggle_off_track: Color::from_rgba8(51, 65, 85, 255),
            toggle_off_border: Color::from_rgba8(255, 255, 255, 22),
            seg_bg: Color::from_rgba8(14, 22, 39, 255),
            seg_border: Color::from_rgba8(42, 56, 82, 255),
            seg_active_bg: Color::from_rgba8(14, 165, 233, 220),
            seg_hover_bg: Color::from_rgba8(255, 255, 255, 16),
            ghost_bg: Color::from_rgba8(28, 40, 62, 255),
            ghost_hover_bg: Color::from_rgba8(42, 56, 82, 255),
            ghost_border: Color::from_rgba8(255, 255, 255, 24),
            ghost_hover_border: Color::from_rgba8(100, 116, 139, 140),
            ghost_danger_bg: Color::from_rgba8(244, 63, 94, 18),
            ghost_danger_hover_bg: Color::from_rgba8(244, 63, 94, 46),
            ghost_danger_border: Color::from_rgba8(244, 63, 94, 60),
            ghost_danger_hover_border: Color::from_rgba8(244, 63, 94, 140),
            progress_track: Color::from_rgba8(15, 23, 42, 255),
            toast_muted_bg: Color::from_rgba8(255, 255, 255, 12),
            toast_muted_border: Color::from_rgba8(255, 255, 255, 20),
            badge_ok_bg: Color::from_rgba8(16, 185, 129, 30),
            badge_warn_bg: Color::from_rgba8(245, 158, 11, 28),
            badge_err_bg: Color::from_rgba8(244, 63, 94, 28),
        }
    }
}

fn apply_dwm_theme(hwnd: HWND, dark: bool) {
    let p = Palette::for_dark(dark);
    unsafe {
        let dark_flag: i32 = if dark { 1 } else { 0 };
        let _ = DwmSetWindowAttribute(
            hwnd,
            DWMWA_USE_IMMERSIVE_DARK_MODE,
            &dark_flag as *const i32 as *const _,
            std::mem::size_of::<i32>() as u32,
        );
        let cap_color = rgb_colorref(p.dwm_caption.0, p.dwm_caption.1, p.dwm_caption.2);
        let _ = DwmSetWindowAttribute(
            hwnd,
            DWMWA_CAPTION_COLOR,
            &cap_color as *const COLORREF as *const _,
            std::mem::size_of::<COLORREF>() as u32,
        );
        let txt_color = rgb_colorref(p.dwm_text.0, p.dwm_text.1, p.dwm_text.2);
        let _ = DwmSetWindowAttribute(
            hwnd,
            DWMWA_TEXT_COLOR,
            &txt_color as *const COLORREF as *const _,
            std::mem::size_of::<COLORREF>() as u32,
        );
        let corner = DWMWCP_ROUND;
        let _ = DwmSetWindowAttribute(
            hwnd,
            DWMWA_WINDOW_CORNER_PREFERENCE,
            &corner as *const _ as *const _,
            std::mem::size_of_val(&corner) as u32,
        );
    }
}

thread_local! {
    static SETTINGS: RefCell<Option<Box<SettingsWindow>>> = const { RefCell::new(None) };
}

static REGISTER_CLASS: Once = Once::new();

fn ensure_registered() {
    REGISTER_CLASS.call_once(|| unsafe {
        let class_w = to_wide(SETTINGS_CLASS);
        let wc = WNDCLASSEXW {
            cbSize: std::mem::size_of::<WNDCLASSEXW>() as u32,
            lpfnWndProc: Some(settings_wnd_proc),
            hInstance: current_hinstance(),
            hIcon: load_app_icon(32).unwrap_or_default(),
            hIconSm: load_app_icon(16).unwrap_or_default(),
            hCursor: LoadCursorW(None, IDC_ARROW).unwrap_or_default(),
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
    let existing = SETTINGS.with(|cell| {
        let borrow = cell.borrow();
        let win = borrow.as_ref()?;
        if let Some(t) = initial_tab {
            win.switch_tab(t.min(2));
        }
        win.sync_update_notes_edit();
        win.request_repaint();
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
    let logical_w = 760;
    let logical_h = 590;
    let sx = unsafe { GetSystemMetrics(SM_CXSCREEN) };
    let sy = unsafe { GetSystemMetrics(SM_CYSCREEN) };

    let win_style = WS_OVERLAPPED | WS_CAPTION | WS_SYSMENU | WS_MINIMIZEBOX | WS_CLIPCHILDREN;
    let Ok(hwnd) = (unsafe {
        CreateWindowExW(
            Default::default(),
            PCWSTR(class_w.as_ptr()),
            PCWSTR(title_w.as_ptr()),
            win_style,
            (sx - logical_w) / 2,
            (sy - logical_h) / 2,
            logical_w,
            logical_h,
            None,
            None,
            Some(current_hinstance()),
            None,
        )
    }) else {
        return;
    };

    // Apply Windows 11 native light/dark titlebar + rounded corners matching active theme
    apply_dwm_theme(hwnd, config.theme.is_dark());

    let dpi = window_dpi(hwnd);
    let mut adj = RECT {
        left: 0,
        top: 0,
        right: scale_dpi(logical_w, dpi),
        bottom: scale_dpi(logical_h, dpi),
    };
    unsafe {
        let _ = windows::Win32::UI::HiDpi::AdjustWindowRectExForDpi(
            &mut adj,
            win_style,
            false,
            Default::default(),
            dpi,
        );
    }
    let win_w = adj.right - adj.left;
    let win_h = adj.bottom - adj.top;
    let x = ((sx - win_w) / 2).max(20);
    let y = ((sy - win_h) / 2).max(20);
    unsafe {
        let _ = SetWindowPos(hwnd, None, x, y, win_w, win_h, Default::default());
    }

    let mut client = RECT::default();
    unsafe {
        let _ = GetClientRect(hwnd, &mut client);
    }

    let mut cfg = config.clone();
    cfg.auto_start = get_auto_start() || cfg.auto_start;

    let mut win = Box::new(SettingsWindow {
        hwnd,
        dpi,
        client_w: (client.right - client.left).max(1),
        client_h: (client.bottom - client.top).max(1),
        active_tab: Cell::new(initial_tab.unwrap_or(0).min(2)),
        ready: Cell::new(false),
        pending_autosave: Cell::new(false),
        recording_slot: Cell::new(None),
        hovered: Cell::new(None),
        tracking_leave: Cell::new(false),
        focused_edit: Cell::new(None),
        notes_scroll: Cell::new(0),
        notes_max_scroll: Cell::new(0),
        cfg: RefCell::new(cfg.clone()),
        toast: RefCell::new(("修改后自动保存".into(), ToastTone::Muted)),
        hits: RefCell::new(Vec::new()),
        edits: EditHandles::default(),
        font_brand: OwnedFont(create_ui_font(dpi, 12, true)),
        font_h1: OwnedFont(create_ui_font(dpi, 14, true)),
        font_sec: OwnedFont(create_ui_font(dpi, 8, true)),
        font_body: OwnedFont(create_ui_font(dpi, 9, false)),
        font_body_bold: OwnedFont(create_ui_font(dpi, 9, true)),
        font_small: OwnedFont(create_ui_font(dpi, 8, false)),
        font_input: OwnedFont(create_ui_font(dpi, 9, false)),
        edit_bg_light: OwnedBrush::solid(248, 250, 252),
        edit_bg_dark: OwnedBrush::solid(14, 22, 39),
        updater,
        callbacks,
    });

    win.create_embedded_edits(&cfg);
    win.sync_update_notes_edit();
    win.update_edit_visibility();
    win.ready.set(true);

    SETTINGS.with(|cell| {
        *cell.borrow_mut() = Some(win);
    });

    unsafe {
        let _ = SetTimer(Some(hwnd), TIMER_STATS, 1500, None);
        let _ = ShowWindow(hwnd, SW_SHOW);
        let _ = SetForegroundWindow(hwnd);
        let _ = InvalidateRect(Some(hwnd), None, false);
    }
}

pub fn notify_update_changed() {
    SETTINGS.with(|cell| {
        if let Ok(borrow) = cell.try_borrow() {
            if let Some(win) = borrow.as_ref() {
                win.sync_update_notes_edit();
                win.request_repaint();
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

/// Helper used for visual verification snapshots of each tab (including embedded Win32 EDIT children).
pub fn capture_settings_snapshot(tab: usize, output_path: &std::path::Path) -> bool {
    SETTINGS.with(|cell| {
        let borrow = cell.borrow();
        let Some(win) = borrow.as_ref() else {
            return false;
        };
        win.switch_tab(tab.min(2));
        let Some(mut surface) = DibSurface::new(win.client_w, win.client_h) else {
            return false;
        };
        win.render_to_surface(&mut surface);

        // Composite visible embedded EDIT controls on top so snapshots reflect exact live appearance.
        const WM_PRINTCLIENT: u32 = 0x0318;
        const PRF_CLIENT: usize = 0x04;
        const PRF_ERASEBKGND: usize = 0x08;
        let tab_edits: Vec<(HWND, RectI)> = match tab.min(2) {
            1 => vec![(win.edits.save_dir, win.inset_edit(win.save_dir_box()))],
            2 => vec![
                (win.edits.cnb_url, win.inset_edit(win.cnb_url_box())),
                (win.edits.github_url, win.inset_edit(win.github_url_box())),
                (win.edits.proxy_host, win.inset_edit(win.proxy_host_box())),
                (win.edits.proxy_port, win.inset_edit(win.proxy_port_box())),
            ],
            _ => Vec::new(),
        };
        for (ehwnd, r) in tab_edits {
            if !ehwnd.is_invalid() && r.width() > 0 && r.height() > 0 {
                if let Some(child_surf) = DibSurface::new(r.width(), r.height()) {
                    unsafe {
                        let _ = SendMessageW(
                            ehwnd,
                            WM_PRINTCLIENT,
                            Some(WPARAM(child_surf.dc.0 as usize)),
                            Some(LPARAM((PRF_CLIENT | PRF_ERASEBKGND) as isize)),
                        );
                        let _ = BitBlt(
                            surface.dc,
                            r.left,
                            r.top,
                            r.width(),
                            r.height(),
                            Some(child_surf.dc),
                            0,
                            0,
                            SRCCOPY,
                        );
                    }
                }
            }
        }

        let mut pixels = surface.pixels_mut().to_vec();
        for px in pixels.chunks_exact_mut(4) {
            px[3] = 255;
        }
        if let Ok(img) = crate::image_ops::BgraImage::from_bgra(
            win.client_w as u32,
            win.client_h as u32,
            pixels,
        ) {
            img.save_to_file(output_path, SaveFormat::Png).is_ok()
        } else {
            false
        }
    })
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

impl SettingsWindow {
    fn s(&self, logical: i32) -> i32 {
        scale_dpi(logical, self.dpi)
    }

    fn sf(&self) -> f32 {
        self.dpi as f32 / 96.0
    }

    fn request_repaint(&self) {
        unsafe {
            let _ = InvalidateRect(Some(self.hwnd), None, false);
        }
    }

    fn request_stats_repaint(&self) {
        let r = RECT {
            left: self.s(12),
            top: self.client_h - self.s(106),
            right: self.s(190) - self.s(12),
            bottom: self.client_h - self.s(16),
        };
        unsafe {
            let _ = InvalidateRect(Some(self.hwnd), Some(&r), false);
        }
    }

    fn is_dark(&self) -> bool {
        self.cfg.borrow().theme.is_dark()
    }

    fn palette(&self) -> Palette {
        Palette::for_dark(self.is_dark())
    }

    fn refresh_theme_surfaces(&self) {
        apply_dwm_theme(self.hwnd, self.is_dark());
        for eh in [
            self.edits.save_dir,
            self.edits.cnb_url,
            self.edits.github_url,
            self.edits.proxy_host,
            self.edits.proxy_port,
        ] {
            if !eh.is_invalid() {
                unsafe {
                    let _ = InvalidateRect(Some(eh), None, true);
                }
            }
        }
        self.request_repaint();
    }

    fn create_borderless_edit(&self, text: &str, id: usize, rect: RectI) -> HWND {
        let class_w = to_wide("EDIT");
        let text_w = to_wide(text);
        let style = (WS_CHILD | WS_TABSTOP).0 | ES_AUTOHSCROLL as u32;
        let hwnd = unsafe {
            CreateWindowExW(
                Default::default(),
                PCWSTR(class_w.as_ptr()),
                PCWSTR(text_w.as_ptr()),
                windows::Win32::UI::WindowsAndMessaging::WINDOW_STYLE(style),
                rect.left,
                rect.top,
                rect.width().max(10),
                rect.height().max(10),
                Some(self.hwnd),
                Some(HMENU(id as *mut _)),
                Some(current_hinstance()),
                None,
            )
            .unwrap_or_default()
        };
        set_font(hwnd, self.font_input.0);
        hwnd
    }

    // Shared layout geometry for embedded EDIT controls (so paint frames and HWND bounds match 1:1!)
    fn save_dir_box(&self) -> RectI {
        let content_x = self.s(190) + self.s(26);
        let content_r = self.client_w - self.s(26);
        let card_l = content_x + self.s(16);
        let browse_w = self.s(74);
        let box_r = content_r - self.s(16) - browse_w - self.s(8);
        RectI::new(card_l, self.s(118), box_r, self.s(150))
    }

    fn cnb_url_box(&self) -> RectI {
        let content_x = self.s(190) + self.s(26);
        let content_r = self.client_w - self.s(26);
        RectI::new(
            content_x + self.s(96),
            self.s(272),
            content_r - self.s(16),
            self.s(302),
        )
    }

    fn github_url_box(&self) -> RectI {
        let content_x = self.s(190) + self.s(26);
        let content_r = self.client_w - self.s(26);
        RectI::new(
            content_x + self.s(96),
            self.s(312),
            content_r - self.s(16),
            self.s(342),
        )
    }

    fn proxy_host_box(&self) -> RectI {
        let content_x = self.s(190) + self.s(26);
        let content_r = self.client_w - self.s(26);
        let port_w = self.s(68);
        let host_l = content_x + self.s(246);
        let host_r = content_r - self.s(16) - port_w - self.s(8);
        RectI::new(host_l, self.s(354), host_r, self.s(384))
    }

    fn proxy_port_box(&self) -> RectI {
        let content_r = self.client_w - self.s(26);
        let port_w = self.s(68);
        RectI::new(
            content_r - self.s(16) - port_w,
            self.s(354),
            content_r - self.s(16),
            self.s(384),
        )
    }

    fn notes_box(&self) -> RectI {
        let content_x = self.s(190) + self.s(26);
        let content_r = self.client_w - self.s(26);
        RectI::new(
            content_x + self.s(14),
            self.s(438),
            content_r - self.s(14),
            self.client_h - self.s(26),
        )
    }

    fn inset_edit(&self, outer: RectI) -> RectI {
        let px = self.s(10);
        let py = self.s(6);
        RectI::new(outer.left + px, outer.top + py, outer.right - px, outer.bottom - py)
    }

    fn create_embedded_edits(&mut self, cfg: &AppConfig) {
        self.edits.save_dir = self.create_borderless_edit(
            &cfg.save_dir,
            ID_EDIT_SAVE_DIR,
            self.inset_edit(self.save_dir_box()),
        );
        self.edits.cnb_url = self.create_borderless_edit(
            &cfg.update.cnb_url,
            ID_EDIT_CNB_URL,
            self.inset_edit(self.cnb_url_box()),
        );
        self.edits.github_url = self.create_borderless_edit(
            &cfg.update.github_url,
            ID_EDIT_GITHUB_URL,
            self.inset_edit(self.github_url_box()),
        );
        self.edits.proxy_host = self.create_borderless_edit(
            &cfg.update.proxy.host,
            ID_EDIT_PROXY_HOST,
            self.inset_edit(self.proxy_host_box()),
        );
        self.edits.proxy_port = self.create_borderless_edit(
            &cfg.update.proxy.port.to_string(),
            ID_EDIT_PROXY_PORT,
            self.inset_edit(self.proxy_port_box()),
        );
        // Set subtle cue banners when fields are empty
        const EM_SETCUEBANNER: u32 = 0x1501;
        let host_hint = to_wide("127.0.0.1");
        let port_hint = to_wide("7890");
        unsafe {
            let _ = SendMessageW(
                self.edits.proxy_host,
                EM_SETCUEBANNER,
                Some(WPARAM(1)),
                Some(LPARAM(host_hint.as_ptr() as isize)),
            );
            let _ = SendMessageW(
                self.edits.proxy_port,
                EM_SETCUEBANNER,
                Some(WPARAM(1)),
                Some(LPARAM(port_hint.as_ptr() as isize)),
            );
        }
    }

    fn update_edit_visibility(&self) {
        let tab = self.active_tab.get();
        let tab1_cmd = if tab == 1 { SW_SHOW } else { SW_HIDE };
        let tab2_cmd = if tab == 2 { SW_SHOW } else { SW_HIDE };
        unsafe {
            let _ = ShowWindow(self.edits.save_dir, tab1_cmd);
            let _ = ShowWindow(self.edits.cnb_url, tab2_cmd);
            let _ = ShowWindow(self.edits.github_url, tab2_cmd);
            let _ = ShowWindow(self.edits.proxy_host, tab2_cmd);
            let _ = ShowWindow(self.edits.proxy_port, tab2_cmd);
        }
    }

    fn switch_tab(&self, tab: usize) {
        self.cancel_recording();
        self.flush_pending_autosave();
        self.active_tab.set(tab.min(2));
        self.hovered.set(None);
        self.update_edit_visibility();
        if tab == 2 {
            self.sync_update_notes_edit();
        }
        self.request_repaint();
    }

    fn sync_update_notes_edit(&self) {
        self.notes_scroll.set(0);
    }

    fn current_release_notes_text(&self) -> String {
        let snap = self.updater.snapshot();
        if snap.notes.trim().is_empty() {
            if let Some(ver) = &snap.available_version {
                format!("版本 v{ver} 未附带更新日志。")
            } else {
                "点击「检查更新」后将在此展示版本更新日志。\n\n• 支持自定义 CNB / GitHub 仓库地址，自动读取 releases.win.json\n• 安装版自动走 Velopack 增量差分包（*-delta.nupkg）并弹出独立过渡更新界面".into()
            }
        } else {
            snap.notes.replace("\r\n", "\n")
        }
    }

    fn set_toast(&self, text: impl Into<String>, tone: ToastTone) {
        *self.toast.borrow_mut() = (text.into(), tone);
        self.request_repaint();
    }

    fn begin_recording(&self, slot: usize) {
        self.recording_slot.set(Some(slot));
        (self.callbacks.on_suspend_hotkeys)(true);
        unsafe {
            let _ = SetFocus(Some(self.hwnd));
        }
        self.set_toast("请直接按下目标组合键（Backspace 清空 / Esc 取消）", ToastTone::Warning);
    }

    fn cancel_recording(&self) {
        if self.recording_slot.replace(None).is_some() {
            (self.callbacks.on_suspend_hotkeys)(false);
            self.request_repaint();
        }
    }

    fn schedule_autosave(&self) {
        if !self.ready.get() {
            return;
        }
        self.pending_autosave.set(true);
        unsafe {
            let _ = SetTimer(Some(self.hwnd), TIMER_AUTOSAVE, 450, None);
        }
    }

    fn flush_pending_autosave(&self) {
        if self.pending_autosave.get() {
            let _ = self.save_config(true);
        }
    }

    fn collect_and_sync_from_edits(&self) -> Result<AppConfig, String> {
        let mut cfg = self.cfg.borrow().clone();
        cfg.save_dir = get_text(self.edits.save_dir).trim().into();
        let cnb_url = get_text(self.edits.cnb_url).trim().to_string();
        let github_url = get_text(self.edits.github_url).trim().to_string();
        if !cnb_url.is_empty() {
            crate::updater::normalize_mirror_url(&cnb_url, UpdateProvider::Cnb, false)?;
        }
        if !github_url.is_empty() {
            crate::updater::normalize_mirror_url(&github_url, UpdateProvider::Github, false)?;
        }
        cfg.update.cnb_url = cnb_url;
        cfg.update.github_url = github_url;

        let port_text = get_text(self.edits.proxy_port);
        let port = if cfg.update.proxy.enabled {
            port_text
                .trim()
                .parse::<u16>()
                .map_err(|_| "代理端口应为 1–65535".to_string())?
        } else {
            port_text.trim().parse::<u16>().unwrap_or(7890)
        };
        let proxy = ProxyConfig {
            enabled: cfg.update.proxy.enabled,
            protocol: cfg.update.proxy.protocol.clone(),
            host: get_text(self.edits.proxy_host).trim().into(),
            port,
        };
        proxy.validate()?;
        cfg.update.proxy = proxy;
        Ok(cfg)
    }

    fn save_config(&self, quiet_invalid: bool) -> bool {
        self.pending_autosave.set(false);
        unsafe {
            let _ = KillTimer(Some(self.hwnd), TIMER_AUTOSAVE);
        }
        let cfg = match self.collect_and_sync_from_edits() {
            Ok(c) => c,
            Err(err) => {
                if quiet_invalid {
                    self.set_toast(format!("等待完善：{err}"), ToastTone::Warning);
                } else {
                    self.set_toast(format!("错误：{err}"), ToastTone::Error);
                }
                return false;
            }
        };
        *self.cfg.borrow_mut() = cfg.clone();

        if let Err(err) = cfg.save() {
            self.set_toast(format!("保存失败：{err}"), ToastTone::Error);
            return false;
        }
        if let Err(err) = set_auto_start(cfg.auto_start) {
            self.set_toast(format!("自启设置失败：{err}"), ToastTone::Error);
            return false;
        }
        self.updater.update_config(cfg.update.clone());
        let warnings = (self.callbacks.on_config_saved)(cfg);
        if warnings.is_empty() {
            self.set_toast("✓ 已自动保存", ToastTone::Success);
        } else {
            self.set_toast(warnings.join("；"), ToastTone::Warning);
        }
        true
    }

    fn handle_click(&self, kind: HitKind) {
        match kind {
            HitKind::NavTab(t) => self.switch_tab(t),
            HitKind::HotkeyRecord(slot) => self.begin_recording(slot),
            HitKind::HotkeyClear(slot) => {
                self.cancel_recording();
                {
                    let mut cfg = self.cfg.borrow_mut();
                    match slot {
                        0 => cfg.screenshot_hotkey.clear(),
                        1 => cfg.pin_clipboard_hotkey.clear(),
                        _ => cfg.toggle_pins_hotkey.clear(),
                    }
                }
                self.save_config(false);
            }
            HitKind::ThemeSeg(idx) => {
                self.cfg.borrow_mut().theme = ThemeMode::from_index(idx);
                self.refresh_theme_surfaces();
                self.save_config(false);
            }
            HitKind::ToggleAutoStart => {
                self.cfg.borrow_mut().auto_start ^= true;
                self.save_config(false);
            }
            HitKind::ToggleSmartWin => {
                self.cfg.borrow_mut().smart_window_detect ^= true;
                self.save_config(false);
            }
            HitKind::ToggleMagnifier => {
                self.cfg.borrow_mut().show_magnifier ^= true;
                self.save_config(false);
            }
            HitKind::BrowseSaveDir => {
                if let Some(dir) = browse_folder_dialog(Some(self.hwnd)) {
                    let text = dir.to_string_lossy().into_owned();
                    set_text(self.edits.save_dir, &text);
                    self.cfg.borrow_mut().save_dir = text;
                    self.save_config(false);
                }
            }
            HitKind::FormatSeg(idx) => {
                self.cfg.borrow_mut().save_format = SaveFormat::from_index(idx);
                self.save_config(false);
            }
            HitKind::ToggleQuickSave => {
                self.cfg.borrow_mut().quick_save_no_dialog ^= true;
                self.save_config(false);
            }
            HitKind::PinDblClkSeg(idx) => {
                self.cfg.borrow_mut().pin_double_click = PinDoubleClickAction::from_index(idx);
                self.save_config(false);
            }
            HitKind::TogglePinBorder => {
                self.cfg.borrow_mut().pin_show_border ^= true;
                self.save_config(false);
            }
            HitKind::HideAllPins => {
                pin::hide_all_pins();
                self.request_repaint();
            }
            HitKind::ShowAllPins => {
                pin::show_all_pins();
                self.request_repaint();
            }
            HitKind::CloseAllPins => {
                pin::close_all_pins();
                self.request_repaint();
            }
            HitKind::UpdatePrimary => {
                if self.save_config(false) {
                    let updater = self.updater.clone();
                    let on_exit = self.callbacks.on_exit_for_update.clone();
                    updater.start_download_or_install_async(on_exit);
                }
            }
            HitKind::UpdateRecheck => {
                if self.save_config(false) {
                    self.updater.start_check_async();
                }
            }
            HitKind::UpdateCancel => {
                self.updater.cancel();
            }
            HitKind::UpdateSourceSeg(idx) => {
                self.cfg.borrow_mut().update.preferred_source = UpdateProvider::from_index(idx);
                self.save_config(false);
            }
            HitKind::ToggleAutoCheck => {
                self.cfg.borrow_mut().update.auto_check_on_start ^= true;
                self.save_config(false);
            }
            HitKind::ToggleProxy => {
                self.cfg.borrow_mut().update.proxy.enabled ^= true;
                self.save_config(false);
            }
            HitKind::ProxyProtoSeg(idx) => {
                self.cfg.borrow_mut().update.proxy.protocol =
                    if idx == 1 { "https".into() } else { "http".into() };
                self.save_config(false);
            }
        }
    }

    fn render_to_surface(&self, surface: &mut DibSurface) {
        let w = surface.width;
        let h = surface.height;
        let sf = self.sf();
        let s = |v: i32| scale_dpi(v, self.dpi);
        let sidebar_w = s(190);
        let content_x = sidebar_w + self.s(26);
        let content_r = w - self.s(26);
        let content_w = content_r - content_x;

        let mut hits = Vec::new();
        let mut texts: Vec<(String, RectI, (u8, u8, u8), FontSlot, u32)> = Vec::new();

        let cfg = self.cfg.borrow().clone();
        let p = Palette::for_dark(cfg.theme.is_dark());
        let active_tab = self.active_tab.get();
        let hovered = self.hovered.get();
        let recording = self.recording_slot.get();
        let focused_edit = self.focused_edit.get();

        if let Some(mut pm) = Pixmap::new(w as u32, h as u32) {
            // 1. Canvas backgrounds: sidebar + content + subtle divider
            fill_rect(&mut pm, 0.0, 0.0, sidebar_w as f32, h as f32, p.sidebar_bg);
            fill_rect(
                &mut pm,
                sidebar_w as f32,
                0.0,
                (w - sidebar_w) as f32,
                h as f32,
                p.canvas_bg,
            );
            fill_rect(
                &mut pm,
                sidebar_w as f32,
                0.0,
                (1.0 * sf).max(1.0),
                h as f32,
                p.hairline,
            );

            // 2. Sidebar Brand Header
            draw_rhpicture_emblem(&mut pm, 18.0 * sf, 20.0 * sf, 34.0 * sf);
            texts.push((
                "RHPicture".into(),
                RectI::new(s(62), s(18), sidebar_w - s(10), s(40)),
                p.text_primary,
                FontSlot::Brand,
                DT_LEFT.0,
            ));
            texts.push((
                format!("v{APP_VERSION} · 极简截图"),
                RectI::new(s(62), s(38), sidebar_w - s(10), s(56)),
                p.text_muted,
                FontSlot::Small,
                DT_LEFT.0,
            ));

            // 3. Sidebar Navigation Pills
            let nav_items = [(0, "快捷键与常规"), (1, "贴图与保存"), (2, "检查与更新")];
            for (idx, label) in nav_items {
                let ny = s(84) + (idx as i32) * s(44);
                let rect = RectI::new(s(12), ny, sidebar_w - s(12), ny + s(38));
                let kind = HitKind::NavTab(idx);
                hits.push(HitZone { rect, kind });
                let is_active = active_tab == idx;
                let is_hover = hovered == Some(kind);

                if is_active {
                    fill_rrect(
                        &mut pm,
                        rect.left as f32,
                        rect.top as f32,
                        rect.width() as f32,
                        rect.height() as f32,
                        9.0 * sf,
                        p.nav_active_bg,
                    );
                    fill_rrect(
                        &mut pm,
                        rect.left as f32 + 2.0 * sf,
                        rect.top as f32 + 9.0 * sf,
                        3.0 * sf,
                        rect.height() as f32 - 18.0 * sf,
                        1.5 * sf,
                        p.nav_active_bar,
                    );
                } else if is_hover {
                    fill_rrect(
                        &mut pm,
                        rect.left as f32,
                        rect.top as f32,
                        rect.width() as f32,
                        rect.height() as f32,
                        9.0 * sf,
                        p.nav_hover_bg,
                    );
                }

                let icon_col = if is_active {
                    p.nav_icon_active
                } else {
                    p.nav_icon_idle
                };
                draw_nav_icon(
                    &mut pm,
                    idx,
                    rect.left as f32 + 14.0 * sf,
                    rect.top as f32 + 11.0 * sf,
                    sf,
                    icon_col,
                );
                let txt_col = if is_active {
                    p.nav_txt_active
                } else if is_hover {
                    p.nav_txt_hover
                } else {
                    p.nav_txt_idle
                };
                texts.push((
                    label.into(),
                    RectI::new(rect.left + s(38), rect.top, rect.right - s(8), rect.bottom),
                    txt_col,
                    if is_active { FontSlot::BodyBold } else { FontSlot::Body },
                    DT_LEFT.0,
                ));
            }

            // 4. Sidebar Bottom Live Zero-VRAM Resource Card
            let res_rect = RectI::new(s(12), h - s(106), sidebar_w - s(12), h - s(16));
            draw_card(&mut pm, res_rect, 10.0 * sf, sf, p.card_sub_bg, &p);
            if let Some(dot) = PathBuilder::from_circle(
                (res_rect.left + s(16)) as f32,
                (res_rect.top + s(18)) as f32,
                3.5 * sf,
            ) {
                let mut dot_p = Paint::default();
                dot_p.set_color(Color::from_rgba8(16, 185, 129, 255));
                dot_p.anti_alias = true;
                pm.fill_path(&dot, &dot_p, FillRule::Winding, Transform::identity(), None);
            }
            let mem = process_memory_mb();
            let (vis, hid) = pin::pin_counts();
            texts.push((
                "零显存引擎".into(),
                RectI::new(res_rect.left + s(26), res_rect.top + s(8), res_rect.right - s(8), res_rect.top + s(28)),
                p.text_success,
                FontSlot::Small,
                DT_LEFT.0,
            ));
            texts.push((
                format!("内存 {mem:.1} MB · 显存 0 MB"),
                RectI::new(res_rect.left + s(12), res_rect.top + s(34), res_rect.right - s(8), res_rect.top + s(54)),
                p.text_body,
                FontSlot::Small,
                DT_LEFT.0,
            ));
            texts.push((
                format!("贴图：{vis} 显示 / {hid} 隐藏"),
                RectI::new(res_rect.left + s(12), res_rect.top + s(56), res_rect.right - s(8), res_rect.top + s(78)),
                p.text_secondary,
                FontSlot::Small,
                DT_LEFT.0,
            ));

            // 5. Right Content Header + Auto-Save Status Pill
            let page_title = match active_tab {
                0 => "快捷键与常规",
                1 => "贴图与保存",
                _ => "检查与更新",
            };
            texts.push((
                page_title.into(),
                RectI::new(content_x, s(18), content_x + s(220), s(52)),
                p.text_primary,
                FontSlot::H1,
                DT_LEFT.0,
            ));

            let (toast_txt, toast_tone) = self.toast.borrow().clone();
            if !toast_txt.is_empty() {
                let (bg, fg, border) = match toast_tone {
                    ToastTone::Muted => (p.toast_muted_bg, p.text_secondary, p.toast_muted_border),
                    ToastTone::Success => (
                        p.badge_ok_bg,
                        p.text_success,
                        Color::from_rgba8(16, 185, 129, if p.dark { 85 } else { 95 }),
                    ),
                    ToastTone::Warning => (
                        p.badge_warn_bg,
                        p.text_warning,
                        Color::from_rgba8(245, 158, 11, if p.dark { 90 } else { 110 }),
                    ),
                    ToastTone::Error => (
                        p.badge_err_bg,
                        p.text_danger,
                        Color::from_rgba8(244, 63, 94, if p.dark { 95 } else { 110 }),
                    ),
                };
                let pill_w = ((toast_txt.chars().count() as i32) * s(11) + s(24))
                    .clamp(s(100), content_w - s(170));
                let pill = RectI::new(content_r - pill_w, s(22), content_r, s(48));
                fill_rrect(
                    &mut pm,
                    pill.left as f32,
                    pill.top as f32,
                    pill.width() as f32,
                    pill.height() as f32,
                    13.0 * sf,
                    bg,
                );
                stroke_rrect(
                    &mut pm,
                    pill.left as f32 + 0.5 * sf,
                    pill.top as f32 + 0.5 * sf,
                    pill.width() as f32 - sf,
                    pill.height() as f32 - sf,
                    12.5 * sf,
                    1.0 * sf,
                    border,
                );
                texts.push((
                    toast_txt,
                    RectI::new(pill.left + s(10), pill.top, pill.right - s(10), pill.bottom),
                    fg,
                    FontSlot::Small,
                    DT_CENTER.0 | DT_END_ELLIPSIS.0,
                ));
            }

            // 6. Active Page Body
            match active_tab {
                0 => {
                    // --- Section: Global Hotkeys ---
                    texts.push((
                        "全局快捷键".into(),
                        RectI::new(content_x + s(2), s(60), content_r, s(78)),
                        p.text_secondary,
                        FontSlot::Section,
                        DT_LEFT.0,
                    ));
                    let hk_card = RectI::new(content_x, s(80), content_r, s(224));
                    draw_card(&mut pm, hk_card, 11.0 * sf, sf, p.card_bg, &p);

                    let hk_rows = [
                        (0usize, "开始截图", "全局唤起多屏截图选区", cfg.screenshot_hotkey.as_str()),
                        (1usize, "剪贴板贴图", "将剪贴板图片直接钉到屏幕上", cfg.pin_clipboard_hotkey.as_str()),
                        (2usize, "隐藏 / 显示全部贴图", "一键切换所有贴图的显示与隐藏", cfg.toggle_pins_hotkey.as_str()),
                    ];
                    for (idx, title, desc, val) in hk_rows {
                        let ry = hk_card.top + (idx as i32) * s(48);
                        if idx > 0 {
                            draw_divider(&mut pm, hk_card.left + s(16), ry, hk_card.right - s(16), sf, &p);
                        }
                        texts.push((
                            title.into(),
                            RectI::new(hk_card.left + s(16), ry + s(6), hk_card.left + s(260), ry + s(26)),
                            p.text_primary,
                            FontSlot::BodyBold,
                            DT_LEFT.0,
                        ));
                        texts.push((
                            desc.into(),
                            RectI::new(hk_card.left + s(16), ry + s(25), hk_card.left + s(270), ry + s(42)),
                            p.text_secondary,
                            FontSlot::Small,
                            DT_LEFT.0,
                        ));

                        let clr_rect = RectI::new(hk_card.right - s(40), ry + s(10), hk_card.right - s(14), ry + s(38));
                        let key_rect = RectI::new(clr_rect.left - s(142), ry + s(10), clr_rect.left - s(8), ry + s(38));
                        hits.push(HitZone { rect: key_rect, kind: HitKind::HotkeyRecord(idx) });
                        hits.push(HitZone { rect: clr_rect, kind: HitKind::HotkeyClear(idx) });

                        let is_rec = recording == Some(idx);
                        let is_hov = hovered == Some(HitKind::HotkeyRecord(idx));
                        let kbg = if is_rec {
                            p.keycap_rec_bg
                        } else if is_hov {
                            p.keycap_hover_bg
                        } else {
                            p.keycap_bg
                        };
                        let kborder = if is_rec {
                            p.input_border_focus
                        } else if is_hov {
                            p.keycap_hover_border
                        } else {
                            p.keycap_border
                        };
                        fill_rrect(&mut pm, key_rect.left as f32, key_rect.top as f32, key_rect.width() as f32, key_rect.height() as f32, 7.0 * sf, kbg);
                        stroke_rrect(&mut pm, key_rect.left as f32 + 0.5 * sf, key_rect.top as f32 + 0.5 * sf, key_rect.width() as f32 - sf, key_rect.height() as f32 - sf, 6.5 * sf, if is_rec { 1.5 * sf } else { 1.0 * sf }, kborder);

                        let display = if is_rec {
                            "请按键…".to_string()
                        } else if val.trim().is_empty() {
                            "未设置".to_string()
                        } else {
                            val.to_string()
                        };
                        let txt_c = if is_rec {
                            p.text_accent
                        } else if val.trim().is_empty() {
                            p.text_muted
                        } else {
                            p.text_primary
                        };
                        texts.push((display, key_rect, txt_c, FontSlot::BodyBold, DT_CENTER.0));

                        let clr_hov = hovered == Some(HitKind::HotkeyClear(idx));
                        if clr_hov {
                            fill_rrect(&mut pm, clr_rect.left as f32, clr_rect.top as f32, clr_rect.width() as f32, clr_rect.height() as f32, 6.0 * sf, p.ghost_danger_hover_bg);
                        }
                        texts.push((
                            "×".into(),
                            clr_rect,
                            if clr_hov { p.text_danger } else { p.text_muted },
                            FontSlot::BodyBold,
                            DT_CENTER.0,
                        ));
                    }

                    // --- Section: Appearance & General ---
                    texts.push((
                        "外观与常规".into(),
                        RectI::new(content_x + s(2), s(236), content_r, s(254)),
                        p.text_secondary,
                        FontSlot::Section,
                        DT_LEFT.0,
                    ));
                    let gen_card = RectI::new(content_x, s(256), content_r, s(444));
                    draw_card(&mut pm, gen_card, 11.0 * sf, sf, p.card_bg, &p);

                    // Row 0: Theme Mode (Light / System / Dark)
                    let row_h = s(47);
                    texts.push((
                        "外观主题".into(),
                        RectI::new(gen_card.left + s(16), gen_card.top + s(6), gen_card.left + s(210), gen_card.top + s(25)),
                        p.text_primary,
                        FontSlot::BodyBold,
                        DT_LEFT.0,
                    ));
                    texts.push((
                        "默认浅色，可随 Windows 系统自动切换".into(),
                        RectI::new(gen_card.left + s(16), gen_card.top + s(24), gen_card.left + s(260), gen_card.top + s(41)),
                        p.text_secondary,
                        FontSlot::Small,
                        DT_LEFT.0,
                    ));
                    let theme_seg = RectI::new(
                        gen_card.right - s(228),
                        gen_card.top + s(9),
                        gen_card.right - s(16),
                        gen_card.top + s(38),
                    );
                    draw_segmented_bar(
                        &mut pm,
                        &mut hits,
                        &mut texts,
                        theme_seg,
                        sf,
                        &["浅色", "跟随系统", "深色"],
                        cfg.theme.index(),
                        hovered,
                        HitKind::ThemeSeg,
                        &p,
                    );

                    let toggles = [
                        (HitKind::ToggleAutoStart, "开机自动启动", "后台静默常驻托盘，零显存占用", cfg.auto_start),
                        (HitKind::ToggleSmartWin, "智能窗口边界识别", "截图悬停自动高亮目标窗口，单击即选", cfg.smart_window_detect),
                        (HitKind::ToggleMagnifier, "像素放大镜与取色器", "悬停显示放大像素格，按 C 直接复制 #HEX", cfg.show_magnifier),
                    ];
                    for (i, (kind, title, desc, on)) in toggles.into_iter().enumerate() {
                        let ry = gen_card.top + ((i + 1) as i32) * row_h;
                        draw_divider(&mut pm, gen_card.left + s(16), ry, gen_card.right - s(16), sf, &p);
                        let row_rect = RectI::new(gen_card.left, ry, gen_card.right, ry + row_h);
                        hits.push(HitZone { rect: row_rect, kind });
                        if hovered == Some(kind) {
                            fill_rrect(
                                &mut pm,
                                (row_rect.left + s(4)) as f32,
                                (row_rect.top + s(4)) as f32,
                                (row_rect.width() - s(8)) as f32,
                                (row_rect.height() - s(8)) as f32,
                                8.0 * sf,
                                p.row_hover,
                            );
                        }
                        texts.push((
                            title.into(),
                            RectI::new(gen_card.left + s(16), ry + s(6), gen_card.right - s(80), ry + s(25)),
                            p.text_primary,
                            FontSlot::BodyBold,
                            DT_LEFT.0,
                        ));
                        texts.push((
                            desc.into(),
                            RectI::new(gen_card.left + s(16), ry + s(24), gen_card.right - s(80), ry + s(41)),
                            p.text_secondary,
                            FontSlot::Small,
                            DT_LEFT.0,
                        ));
                        draw_toggle_switch(
                            &mut pm,
                            (gen_card.right - s(58)) as f32,
                            (ry + s(12)) as f32,
                            sf,
                            on,
                            &p,
                        );
                    }

                    // --- Compact Tips Card ---
                    let tip_card = RectI::new(content_x, s(458), content_r, h - s(16));
                    draw_card(&mut pm, tip_card, 11.0 * sf, sf, p.card_sub_bg, &p);
                    texts.push((
                        "快捷操作速查".into(),
                        RectI::new(tip_card.left + s(16), tip_card.top + s(10), tip_card.right - s(16), tip_card.top + s(28)),
                        p.text_accent,
                        FontSlot::Small,
                        DT_LEFT.0,
                    ));
                    texts.push((
                        "截图时：双击选区 / Enter 复制 · F3 / T 贴图 · Ctrl+S 保存 · 右键重选 · 方向键 1px 微调".into(),
                        RectI::new(tip_card.left + s(16), tip_card.top + s(32), tip_card.right - s(14), tip_card.top + s(53)),
                        p.text_secondary,
                        FontSlot::Small,
                        DT_LEFT.0 | DT_END_ELLIPSIS.0,
                    ));
                    texts.push((
                        "贴图上：左键拖动 · 滚轮缩放 · Ctrl+滚轮透明度 · 双击/H 隐藏 · R 旋转 · Esc 关闭".into(),
                        RectI::new(tip_card.left + s(16), tip_card.top + s(54), tip_card.right - s(14), tip_card.top + s(75)),
                        p.text_secondary,
                        FontSlot::Small,
                        DT_LEFT.0 | DT_END_ELLIPSIS.0,
                    ));
                    texts.push((
                        "取色器：悬停像素放大镜按 C 复制 #HEX 色值 · 快捷键直接将剪贴板图片贴图".into(),
                        RectI::new(tip_card.left + s(16), tip_card.top + s(76), tip_card.right - s(14), tip_card.top + s(97)),
                        p.text_secondary,
                        FontSlot::Small,
                        DT_LEFT.0 | DT_END_ELLIPSIS.0,
                    ));
                }
                1 => {
                    // --- Section: Local Saving ---
                    texts.push((
                        "本地保存".into(),
                        RectI::new(content_x + s(2), s(66), content_r, s(86)),
                        p.text_secondary,
                        FontSlot::Section,
                        DT_LEFT.0,
                    ));
                    let save_card = RectI::new(content_x, s(88), content_r, s(260));
                    draw_card(&mut pm, save_card, 11.0 * sf, sf, p.card_bg, &p);

                    texts.push((
                        "默认保存目录".into(),
                        RectI::new(save_card.left + s(16), s(96), save_card.right - s(16), s(116)),
                        p.text_body,
                        FontSlot::Small,
                        DT_LEFT.0,
                    ));
                    let dir_box = self.save_dir_box();
                    draw_input_frame(&mut pm, dir_box, sf, focused_edit == Some(ID_EDIT_SAVE_DIR), &p);
                    let browse_rect = RectI::new(dir_box.right + s(8), dir_box.top, save_card.right - s(16), dir_box.bottom);
                    hits.push(HitZone { rect: browse_rect, kind: HitKind::BrowseSaveDir });
                    draw_ghost_button(&mut pm, browse_rect, sf, hovered == Some(HitKind::BrowseSaveDir), false, &p);
                    texts.push(("浏览…".into(), browse_rect, p.text_body, FontSlot::Body, DT_CENTER.0));

                    draw_divider(&mut pm, save_card.left + s(16), s(162), save_card.right - s(16), sf, &p);
                    texts.push((
                        "默认保存格式".into(),
                        RectI::new(save_card.left + s(16), s(166), save_card.left + s(150), s(208)),
                        p.text_primary,
                        FontSlot::BodyBold,
                        DT_LEFT.0,
                    ));
                    let fmt_rect = RectI::new(save_card.right - s(246), s(172), save_card.right - s(16), s(202));
                    draw_segmented_bar(
                        &mut pm,
                        &mut hits,
                        &mut texts,
                        fmt_rect,
                        sf,
                        &["PNG 无损", "JPEG", "BMP"],
                        cfg.save_format.index(),
                        hovered,
                        HitKind::FormatSeg,
                        &p,
                    );

                    draw_divider(&mut pm, save_card.left + s(16), s(212), save_card.right - s(16), sf, &p);
                    let qs_row = RectI::new(save_card.left, s(212), save_card.right, s(260));
                    hits.push(HitZone { rect: qs_row, kind: HitKind::ToggleQuickSave });
                    if hovered == Some(HitKind::ToggleQuickSave) {
                        fill_rrect(&mut pm, (qs_row.left + s(4)) as f32, (qs_row.top + s(4)) as f32, (qs_row.width() - s(8)) as f32, (qs_row.height() - s(8)) as f32, 8.0 * sf, p.row_hover);
                    }
                    texts.push((
                        "保存时直接存入默认目录".into(),
                        RectI::new(save_card.left + s(16), s(218), save_card.right - s(80), s(238)),
                        p.text_primary,
                        FontSlot::BodyBold,
                        DT_LEFT.0,
                    ));
                    texts.push((
                        "跳过系统「另存为」弹窗，按时间戳极速保存".into(),
                        RectI::new(save_card.left + s(16), s(237), save_card.right - s(80), s(254)),
                        p.text_secondary,
                        FontSlot::Small,
                        DT_LEFT.0,
                    ));
                    draw_toggle_switch(&mut pm, (save_card.right - s(58)) as f32, s(225) as f32, sf, cfg.quick_save_no_dialog, &p);

                    // --- Section: Pin Behavior ---
                    texts.push((
                        "贴图窗口行为".into(),
                        RectI::new(content_x + s(2), s(276), content_r, s(296)),
                        p.text_secondary,
                        FontSlot::Section,
                        DT_LEFT.0,
                    ));
                    let pin_card = RectI::new(content_x, s(298), content_r, s(404));
                    draw_card(&mut pm, pin_card, 11.0 * sf, sf, p.card_bg, &p);

                    texts.push((
                        "双击贴图动作".into(),
                        RectI::new(pin_card.left + s(16), s(304), pin_card.left + s(150), s(348)),
                        p.text_primary,
                        FontSlot::BodyBold,
                        DT_LEFT.0,
                    ));
                    let dbl_rect = RectI::new(pin_card.right - s(276), s(311), pin_card.right - s(16), s(341));
                    draw_segmented_bar(
                        &mut pm,
                        &mut hits,
                        &mut texts,
                        dbl_rect,
                        sf,
                        &["隐藏省内存", "重置 100%", "直接关闭"],
                        cfg.pin_double_click.index(),
                        hovered,
                        HitKind::PinDblClkSeg,
                        &p,
                    );

                    draw_divider(&mut pm, pin_card.left + s(16), s(352), pin_card.right - s(16), sf, &p);
                    let bd_row = RectI::new(pin_card.left, s(352), pin_card.right, s(404));
                    hits.push(HitZone { rect: bd_row, kind: HitKind::TogglePinBorder });
                    if hovered == Some(HitKind::TogglePinBorder) {
                        fill_rrect(&mut pm, (bd_row.left + s(4)) as f32, (bd_row.top + s(4)) as f32, (bd_row.width() - s(8)) as f32, (bd_row.height() - s(8)) as f32, 8.0 * sf, p.row_hover);
                    }
                    texts.push((
                        "贴图细边框保护".into(),
                        RectI::new(pin_card.left + s(16), s(359), pin_card.right - s(80), s(379)),
                        p.text_primary,
                        FontSlot::BodyBold,
                        DT_LEFT.0,
                    ));
                    texts.push((
                        "绘制极细外轮廓，防止白底截图融入浅色窗口".into(),
                        RectI::new(pin_card.left + s(16), s(378), pin_card.right - s(80), s(396)),
                        p.text_secondary,
                        FontSlot::Small,
                        DT_LEFT.0,
                    ));
                    draw_toggle_switch(&mut pm, (pin_card.right - s(58)) as f32, s(367) as f32, sf, cfg.pin_show_border, &p);

                    // --- Section: Pin Quick Management ---
                    texts.push((
                        "当前贴图快捷管理".into(),
                        RectI::new(content_x + s(2), s(420), content_r, s(440)),
                        p.text_secondary,
                        FontSlot::Section,
                        DT_LEFT.0,
                    ));
                    let mg_card = RectI::new(content_x, s(442), content_r, s(506));
                    draw_card(&mut pm, mg_card, 11.0 * sf, sf, p.card_bg, &p);
                    let gap = s(10);
                    let bw = (mg_card.width() - s(32) - gap * 2) / 3;
                    let actions = [
                        (HitKind::HideAllPins, "隐藏全部贴图", false),
                        (HitKind::ShowAllPins, "恢复全部贴图", false),
                        (HitKind::CloseAllPins, "关闭全部贴图", true),
                    ];
                    for (i, (kind, label, danger)) in actions.into_iter().enumerate() {
                        let bx = mg_card.left + s(16) + (i as i32) * (bw + gap);
                        let br = RectI::new(bx, mg_card.top + s(15), bx + bw, mg_card.bottom - s(15));
                        hits.push(HitZone { rect: br, kind });
                        draw_ghost_button(&mut pm, br, sf, hovered == Some(kind), danger, &p);
                        let col = if danger && hovered == Some(kind) {
                            p.text_danger
                        } else if danger {
                            p.text_danger_soft
                        } else {
                            p.text_body
                        };
                        texts.push((label.into(), br, col, FontSlot::BodyBold, DT_CENTER.0));
                    }
                }
                _ => {
                    // --- Page 2: Updates ---
                    let snap = self.updater.snapshot();
                    let hero = RectI::new(content_x, s(66), content_r, s(206));
                    draw_card(&mut pm, hero, 11.0 * sf, sf, p.card_bg, &p);

                    // Top row: Version + mode badge + action buttons
                    texts.push((
                        format!("v{}", snap.current_version),
                        RectI::new(hero.left + s(16), hero.top + s(14), hero.left + s(88), hero.top + s(40)),
                        p.text_primary,
                        FontSlot::H1,
                        DT_LEFT.0,
                    ));
                    let (mode_txt, mode_bg, mode_fg) = if snap.managed {
                        ("Velopack 增量更新", p.badge_ok_bg, p.text_success)
                    } else {
                        ("独立运行模式", p.badge_warn_bg, p.text_warning)
                    };
                    let mb = RectI::new(hero.left + s(90), hero.top + s(17), hero.left + s(212), hero.top + s(39));
                    fill_rrect(&mut pm, mb.left as f32, mb.top as f32, mb.width() as f32, mb.height() as f32, 11.0 * sf, mode_bg);
                    texts.push((mode_txt.into(), mb, mode_fg, FontSlot::Small, DT_CENTER.0));

                    // Action button on right of hero header
                    let sec_kind = if snap.busy && snap.cancellable {
                        HitKind::UpdateCancel
                    } else {
                        HitKind::UpdateRecheck
                    };
                    let sec_rect = RectI::new(hero.right - s(84), hero.top + s(14), hero.right - s(14), hero.top + s(44));
                    if !snap.busy || snap.cancellable {
                        hits.push(HitZone { rect: sec_rect, kind: sec_kind });
                    }
                    draw_ghost_button(&mut pm, sec_rect, sf, hovered == Some(sec_kind), sec_kind == HitKind::UpdateCancel, &p);
                    let sec_label = if sec_kind == HitKind::UpdateCancel { "取消" } else { "重检" };
                    texts.push((sec_label.into(), sec_rect, p.text_body, FontSlot::Body, DT_CENTER.0));

                    let pri_rect = RectI::new(hero.left + s(16), hero.top + s(56), hero.left + s(190), hero.top + s(88));
                    if !snap.busy {
                        hits.push(HitZone { rect: pri_rect, kind: HitKind::UpdatePrimary });
                    }
                    draw_primary_button(&mut pm, pri_rect, sf, hovered == Some(HitKind::UpdatePrimary), snap.busy, &p);
                    texts.push((snap.primary_button_label(), pri_rect, (255, 255, 255), FontSlot::BodyBold, DT_CENTER.0));

                    // Status text next to primary button
                    texts.push((
                        snap.status.clone(),
                        RectI::new(pri_rect.right + s(14), hero.top + s(56), hero.right - s(16), hero.top + s(88)),
                        match snap.phase {
                            UpdatePhase::Error => p.text_danger,
                            UpdatePhase::Downloaded | UpdatePhase::Current => p.text_success,
                            UpdatePhase::Available | UpdatePhase::Downloading | UpdatePhase::Verifying => p.text_accent,
                            _ => p.text_secondary,
                        },
                        FontSlot::Small,
                        DT_LEFT.0 | DT_END_ELLIPSIS.0,
                    ));

                    // Sleek luminous gradient progress bar
                    let bar_l = (hero.left + s(16)) as f32;
                    let bar_t = (hero.top + s(106)) as f32;
                    let bar_w = (hero.width() - s(32)) as f32;
                    let bar_h = 8.0 * sf;
                    fill_rrect(&mut pm, bar_l, bar_t, bar_w, bar_h, bar_h * 0.5, p.progress_track);
                    let ratio = if snap.pending_restart {
                        1.0
                    } else if snap.busy && snap.progress == 0 {
                        0.16
                    } else {
                        (snap.progress.clamp(0, 100) as f32) / 100.0
                    };
                    if ratio > 0.0 {
                        let fw = (bar_w * ratio).max(bar_h * 1.5);
                        if let Some(bp) = rrect_path(bar_l, bar_t, fw, bar_h, bar_h * 0.5) {
                            if let Some(g) = LinearGradient::new(
                                Point::from_xy(bar_l, bar_t),
                                Point::from_xy(bar_l + fw, bar_t),
                                vec![
                                    GradientStop::new(0.0, Color::from_rgba8(14, 165, 233, 255)),
                                    GradientStop::new(1.0, Color::from_rgba8(16, 185, 129, 255)),
                                ],
                                SpreadMode::Pad,
                                Transform::identity(),
                            ) {
                                let mut paint = Paint::default();
                                paint.shader = g;
                                paint.anti_alias = true;
                                pm.fill_path(&bp, &paint, FillRule::Winding, Transform::identity(), None);
                            }
                        }
                    }
                    let hint_line = if !snap.provider.is_empty() {
                        format!("当前源：{} · 升级重启时自动启用独立过渡界面", snap.provider)
                    } else {
                        "升级安装时自动启用独立过渡界面，不中断配置保留".into()
                    };
                    texts.push((
                        hint_line,
                        RectI::new(hero.left + s(16), hero.top + s(118), hero.right - s(16), hero.bottom - s(4)),
                        p.text_muted,
                        FontSlot::Small,
                        DT_LEFT.0 | DT_END_ELLIPSIS.0,
                    ));

                    // --- Mirror & Proxy Config Card ---
                    let src_card = RectI::new(content_x, s(218), content_r, s(398));
                    draw_card(&mut pm, src_card, 11.0 * sf, sf, p.card_bg, &p);

                    // Row 1: Preferred source segmented bar + auto-check toggle
                    texts.push((
                        "首选源".into(),
                        RectI::new(src_card.left + s(16), s(228), src_card.left + s(90), s(258)),
                        p.text_body,
                        FontSlot::BodyBold,
                        DT_LEFT.0,
                    ));
                    let src_seg = RectI::new(src_card.left + s(96), s(229), src_card.left + s(276), s(257));
                    draw_segmented_bar(
                        &mut pm,
                        &mut hits,
                        &mut texts,
                        src_seg,
                        sf,
                        &["CNB 优先", "GitHub 优先"],
                        cfg.update.preferred_source.index(),
                        hovered,
                        HitKind::UpdateSourceSeg,
                        &p,
                    );

                    let auto_rect = RectI::new(src_card.right - s(172), s(228), src_card.right - s(14), s(258));
                    hits.push(HitZone { rect: auto_rect, kind: HitKind::ToggleAutoCheck });
                    texts.push((
                        "启动自动检查".into(),
                        RectI::new(auto_rect.left, auto_rect.top, auto_rect.right - s(48), auto_rect.bottom),
                        p.text_secondary,
                        FontSlot::Small,
                        DT_RIGHT.0,
                    ));
                    draw_toggle_switch(&mut pm, (auto_rect.right - s(42)) as f32, (auto_rect.top + s(4)) as f32, sf, cfg.update.auto_check_on_start, &p);

                    // Row 2: CNB URL
                    texts.push((
                        "CNB 地址".into(),
                        RectI::new(src_card.left + s(16), s(272), src_card.left + s(92), s(302)),
                        p.text_body,
                        FontSlot::Body,
                        DT_LEFT.0,
                    ));
                    draw_input_frame(&mut pm, self.cnb_url_box(), sf, focused_edit == Some(ID_EDIT_CNB_URL), &p);

                    // Row 3: GitHub URL
                    texts.push((
                        "GitHub".into(),
                        RectI::new(src_card.left + s(16), s(312), src_card.left + s(92), s(342)),
                        p.text_body,
                        FontSlot::Body,
                        DT_LEFT.0,
                    ));
                    draw_input_frame(&mut pm, self.github_url_box(), sf, focused_edit == Some(ID_EDIT_GITHUB_URL), &p);

                    // Row 4: Proxy
                    let p_tog = RectI::new(src_card.left + s(16), s(354), src_card.left + s(126), s(384));
                    hits.push(HitZone { rect: p_tog, kind: HitKind::ToggleProxy });
                    texts.push((
                        "代理".into(),
                        RectI::new(p_tog.left, p_tog.top, p_tog.left + s(46), p_tog.bottom),
                        p.text_body,
                        FontSlot::Body,
                        DT_LEFT.0,
                    ));
                    draw_toggle_switch(&mut pm, (p_tog.left + s(50)) as f32, (p_tog.top + s(4)) as f32, sf, cfg.update.proxy.enabled, &p);

                    let proto_seg = RectI::new(src_card.left + s(134), s(355), src_card.left + s(238), s(383));
                    draw_segmented_bar(
                        &mut pm,
                        &mut hits,
                        &mut texts,
                        proto_seg,
                        sf,
                        &["HTTP", "HTTPS"],
                        if cfg.update.proxy.protocol == "https" { 1 } else { 0 },
                        hovered,
                        HitKind::ProxyProtoSeg,
                        &p,
                    );
                    draw_input_frame(&mut pm, self.proxy_host_box(), sf, focused_edit == Some(ID_EDIT_PROXY_HOST), &p);
                    draw_input_frame(&mut pm, self.proxy_port_box(), sf, focused_edit == Some(ID_EDIT_PROXY_PORT), &p);

                    // --- Release Notes Card ---
                    texts.push((
                        "版本更新日志".into(),
                        RectI::new(content_x + s(2), s(410), content_r, s(428)),
                        p.text_secondary,
                        FontSlot::Section,
                        DT_LEFT.0,
                    ));
                    let notes_card = RectI::new(content_x, s(432), content_r, h - s(20));
                    draw_card(&mut pm, notes_card, 11.0 * sf, sf, p.card_sub_bg, &p);
                }
            }

            // Copy premultiplied RGBA Pixmap -> top-down BGRA DibSurface
            let src = pm.data();
            let dst = surface.pixels_mut();
            for (spx, dpx) in src.chunks_exact(4).zip(dst.chunks_exact_mut(4)) {
                dpx[0] = spx[2];
                dpx[1] = spx[1];
                dpx[2] = spx[0];
                dpx[3] = 255;
            }
        }

        // Draw all GDI ClearType text cleanly on top
        for (txt, rect, col, slot, align) in texts {
            let font = match slot {
                FontSlot::Brand => self.font_brand.0,
                FontSlot::H1 => self.font_h1.0,
                FontSlot::Section => self.font_sec.0,
                FontSlot::Body => self.font_body.0,
                FontSlot::BodyBold => self.font_body_bold.0,
                FontSlot::Small => self.font_small.0,
            };
            let wide = to_wide(&txt);
            let mut r = RECT {
                left: rect.left,
                top: rect.top,
                right: rect.right,
                bottom: rect.bottom,
            };
            unsafe {
                let old = SelectObject(surface.dc, windows::Win32::Graphics::Gdi::HGDIOBJ(font.0));
                SetBkMode(surface.dc, TRANSPARENT);
                SetTextColor(surface.dc, rgb_colorref(col.0, col.1, col.2));
                let _ = DrawTextW(
                    surface.dc,
                    &mut wide[..wide.len() - 1].to_vec(),
                    &mut r,
                    windows::Win32::Graphics::Gdi::DRAW_TEXT_FORMAT(
                        align | DT_VCENTER.0 | DT_SINGLELINE.0 | DT_NOCLIP.0,
                    ),
                );
                let _ = SelectObject(surface.dc, old);
            }
        }

        // Custom wrapped ClearType release notes on Tab 2 (zero ugly Win32 scrollbars!)
        if self.active_tab.get() == 2 {
            let s = |v: i32| self.s(v);
            let nb = self.notes_box();
            let text_pad_x = s(4);
            let text_pad_y = s(4);
            let viewport_h = (nb.height() - text_pad_y * 2).max(1);
            let notes_text = self.current_release_notes_text();
            let wide = to_wide(&notes_text);
            let mut chars = wide[..wide.len().saturating_sub(1)].to_vec();
            let mut measure = RECT {
                left: nb.left + text_pad_x,
                top: 0,
                right: nb.right - text_pad_x - s(8),
                bottom: 0,
            };
            unsafe {
                let old = SelectObject(
                    surface.dc,
                    windows::Win32::Graphics::Gdi::HGDIOBJ(self.font_small.0.0),
                );
                SetBkMode(surface.dc, TRANSPARENT);
                SetTextColor(surface.dc, rgb_colorref(p.text_body.0, p.text_body.1, p.text_body.2));
                let _ = DrawTextW(
                    surface.dc,
                    &mut chars,
                    &mut measure,
                    windows::Win32::Graphics::Gdi::DRAW_TEXT_FORMAT(
                        DT_LEFT.0 | DT_WORDBREAK.0 | DT_CALCRECT.0,
                    ),
                );
                let content_h = (measure.bottom - measure.top).max(0);
                let max_scroll = (content_h - viewport_h).max(0);
                self.notes_max_scroll.set(max_scroll);
                let scroll = self.notes_scroll.get().clamp(0, max_scroll);
                self.notes_scroll.set(scroll);

                let saved = SaveDC(surface.dc);
                let _ = IntersectClipRect(
                    surface.dc,
                    nb.left + text_pad_x,
                    nb.top + text_pad_y,
                    nb.right - text_pad_x,
                    nb.bottom - text_pad_y,
                );
                let mut draw_r = RECT {
                    left: nb.left + text_pad_x,
                    top: nb.top + text_pad_y - scroll,
                    right: nb.right - text_pad_x - s(8),
                    bottom: nb.top + text_pad_y - scroll + content_h + s(20),
                };
                let _ = DrawTextW(
                    surface.dc,
                    &mut chars,
                    &mut draw_r,
                    windows::Win32::Graphics::Gdi::DRAW_TEXT_FORMAT(DT_LEFT.0 | DT_WORDBREAK.0),
                );
                let _ = RestoreDC(surface.dc, saved);
                let _ = SelectObject(surface.dc, old);
            }
        }

        *self.hits.borrow_mut() = hits;
    }

    fn hit_at(&self, x: i32, y: i32) -> Option<HitKind> {
        self.hits
            .borrow()
            .iter()
            .rev()
            .find(|z| z.rect.contains(x, y))
            .map(|z| z.kind)
    }
}

#[derive(Clone, Copy)]
enum FontSlot {
    Brand,
    H1,
    Section,
    Body,
    BodyBold,
    Small,
}

fn rrect_path(x: f32, y: f32, w: f32, h: f32, r: f32) -> Option<tiny_skia::Path> {
    let r = r.min(w * 0.5).min(h * 0.5).max(0.0);
    let k = 0.5522848 * r;
    let mut pb = PathBuilder::new();
    pb.move_to(x + r, y);
    pb.line_to(x + w - r, y);
    pb.cubic_to(x + w - r + k, y, x + w, y + r - k, x + w, y + r);
    pb.line_to(x + w, y + h - r);
    pb.cubic_to(x + w, y + h - r + k, x + w - r + k, y + h, x + w - r, y + h);
    pb.line_to(x + r, y + h);
    pb.cubic_to(x + r - k, y + h, x, y + h - r + k, x, y + h - r);
    pb.line_to(x, y + r);
    pb.cubic_to(x, y + r - k, x + r - k, y, x + r, y);
    pb.close();
    pb.finish()
}

fn fill_rect(pm: &mut Pixmap, x: f32, y: f32, w: f32, h: f32, color: Color) {
    if let Some(r) = tiny_skia::Rect::from_xywh(x, y, w, h) {
        let mut p = Paint::default();
        p.set_color(color);
        pm.fill_rect(r, &p, Transform::identity(), None);
    }
}

fn fill_rrect(pm: &mut Pixmap, x: f32, y: f32, w: f32, h: f32, r: f32, color: Color) {
    if let Some(path) = rrect_path(x, y, w, h, r) {
        let mut p = Paint::default();
        p.set_color(color);
        p.anti_alias = true;
        pm.fill_path(&path, &p, FillRule::Winding, Transform::identity(), None);
    }
}

fn stroke_rrect(pm: &mut Pixmap, x: f32, y: f32, w: f32, h: f32, r: f32, width: f32, color: Color) {
    if let Some(path) = rrect_path(x, y, w, h, r) {
        let mut p = Paint::default();
        p.set_color(color);
        p.anti_alias = true;
        let st = Stroke {
            width,
            ..Stroke::default()
        };
        pm.stroke_path(&path, &p, &st, Transform::identity(), None);
    }
}

fn draw_card(pm: &mut Pixmap, rect: RectI, radius: f32, sf: f32, bg: Color, p: &Palette) {
    if !p.dark {
        // Ultra-subtle elevation shadow under white Fluent cards in light mode
        fill_rrect(
            pm,
            rect.left as f32,
            rect.top as f32 + 1.5 * sf,
            rect.width() as f32,
            rect.height() as f32,
            radius,
            Color::from_rgba8(15, 23, 42, 10),
        );
    }
    fill_rrect(
        pm,
        rect.left as f32,
        rect.top as f32,
        rect.width() as f32,
        rect.height() as f32,
        radius,
        bg,
    );
    stroke_rrect(
        pm,
        rect.left as f32 + 0.5 * sf,
        rect.top as f32 + 0.5 * sf,
        rect.width() as f32 - sf,
        rect.height() as f32 - sf,
        (radius - 0.5 * sf).max(0.0),
        1.0 * sf,
        p.card_border,
    );
}

fn draw_divider(pm: &mut Pixmap, x0: i32, y: i32, x1: i32, sf: f32, p: &Palette) {
    fill_rect(
        pm,
        x0 as f32,
        y as f32,
        (x1 - x0).max(0) as f32,
        (1.0 * sf).max(1.0),
        p.divider,
    );
}

fn draw_input_frame(pm: &mut Pixmap, rect: RectI, sf: f32, focused: bool, p: &Palette) {
    fill_rrect(
        pm,
        rect.left as f32,
        rect.top as f32,
        rect.width() as f32,
        rect.height() as f32,
        7.0 * sf,
        p.input_bg,
    );
    let border = if focused {
        p.input_border_focus
    } else {
        p.input_border
    };
    stroke_rrect(
        pm,
        rect.left as f32 + 0.5 * sf,
        rect.top as f32 + 0.5 * sf,
        rect.width() as f32 - sf,
        rect.height() as f32 - sf,
        6.5 * sf,
        if focused { 1.4 * sf } else { 1.0 * sf },
        border,
    );
}

fn draw_toggle_switch(pm: &mut Pixmap, x: f32, y: f32, sf: f32, on: bool, p: &Palette) {
    let w = 40.0 * sf;
    let h = 22.0 * sf;
    let r = h * 0.5;
    let track_col = if on {
        Color::from_rgba8(14, 165, 233, 255)
    } else {
        p.toggle_off_track
    };
    fill_rrect(pm, x, y, w, h, r, track_col);
    if !on {
        stroke_rrect(
            pm,
            x + 0.5 * sf,
            y + 0.5 * sf,
            w - sf,
            h - sf,
            r - 0.5 * sf,
            1.0 * sf,
            p.toggle_off_border,
        );
    }
    let knob_r = 7.5 * sf;
    let cx = if on { x + w - 11.0 * sf } else { x + 11.0 * sf };
    let cy = y + h * 0.5;
    if let Some(sh) = PathBuilder::from_circle(cx, cy + 1.0 * sf, knob_r) {
        let mut paint = Paint::default();
        paint.set_color(Color::from_rgba8(0, 0, 0, if p.dark { 50 } else { 32 }));
        paint.anti_alias = true;
        pm.fill_path(&sh, &paint, FillRule::Winding, Transform::identity(), None);
    }
    if let Some(knob) = PathBuilder::from_circle(cx, cy, knob_r) {
        let mut paint = Paint::default();
        paint.set_color(Color::from_rgba8(255, 255, 255, 255));
        paint.anti_alias = true;
        pm.fill_path(&knob, &paint, FillRule::Winding, Transform::identity(), None);
    }
}

fn draw_segmented_bar<F>(
    pm: &mut Pixmap,
    hits: &mut Vec<HitZone>,
    texts: &mut Vec<(String, RectI, (u8, u8, u8), FontSlot, u32)>,
    rect: RectI,
    sf: f32,
    labels: &[&str],
    selected: usize,
    hovered: Option<HitKind>,
    make_kind: F,
    p: &Palette,
) where
    F: Fn(usize) -> HitKind,
{
    fill_rrect(
        pm,
        rect.left as f32,
        rect.top as f32,
        rect.width() as f32,
        rect.height() as f32,
        8.0 * sf,
        p.seg_bg,
    );
    stroke_rrect(
        pm,
        rect.left as f32 + 0.5 * sf,
        rect.top as f32 + 0.5 * sf,
        rect.width() as f32 - sf,
        rect.height() as f32 - sf,
        7.5 * sf,
        1.0 * sf,
        p.seg_border,
    );

    let n = labels.len().max(1) as i32;
    let pad = (3.0 * sf).round() as i32;
    let inner_w = rect.width() - pad * 2;
    let seg_w = inner_w / n;
    for (i, &label) in labels.iter().enumerate() {
        let kind = make_kind(i);
        let sx = rect.left + pad + (i as i32) * seg_w;
        let sr = RectI::new(
            sx,
            rect.top + pad,
            if i + 1 == labels.len() {
                rect.right - pad
            } else {
                sx + seg_w
            },
            rect.bottom - pad,
        );
        hits.push(HitZone { rect: sr, kind });
        let active = i == selected;
        let is_hov = hovered == Some(kind);
        if active {
            fill_rrect(
                pm,
                sr.left as f32,
                sr.top as f32,
                sr.width() as f32,
                sr.height() as f32,
                6.0 * sf,
                p.seg_active_bg,
            );
        } else if is_hov {
            fill_rrect(
                pm,
                sr.left as f32,
                sr.top as f32,
                sr.width() as f32,
                sr.height() as f32,
                6.0 * sf,
                p.seg_hover_bg,
            );
        }
        texts.push((
            label.into(),
            sr,
            if active {
                (255, 255, 255)
            } else if is_hov {
                p.text_primary
            } else {
                p.text_secondary
            },
            FontSlot::Small,
            DT_CENTER.0,
        ));
    }
}

fn draw_primary_button(
    pm: &mut Pixmap,
    rect: RectI,
    sf: f32,
    hovered: bool,
    disabled: bool,
    p: &Palette,
) {
    let (c0, c1) = if disabled {
        let dis = if p.dark {
            Color::from_rgba8(51, 65, 85, 255)
        } else {
            Color::from_rgba8(148, 163, 184, 255)
        };
        (dis, dis)
    } else if hovered {
        (
            Color::from_rgba8(56, 189, 248, 255),
            Color::from_rgba8(37, 99, 235, 255),
        )
    } else {
        (
            Color::from_rgba8(14, 165, 233, 255),
            Color::from_rgba8(37, 99, 235, 255),
        )
    };
    if let Some(path) = rrect_path(
        rect.left as f32,
        rect.top as f32,
        rect.width() as f32,
        rect.height() as f32,
        8.0 * sf,
    ) {
        if let Some(grad) = LinearGradient::new(
            Point::from_xy(rect.left as f32, rect.top as f32),
            Point::from_xy(rect.right as f32, rect.bottom as f32),
            vec![GradientStop::new(0.0, c0), GradientStop::new(1.0, c1)],
            SpreadMode::Pad,
            Transform::identity(),
        ) {
            let mut paint = Paint::default();
            paint.shader = grad;
            paint.anti_alias = true;
            pm.fill_path(&path, &paint, FillRule::Winding, Transform::identity(), None);
        }
    }
}

fn draw_ghost_button(
    pm: &mut Pixmap,
    rect: RectI,
    sf: f32,
    hovered: bool,
    danger: bool,
    p: &Palette,
) {
    let bg = match (hovered, danger) {
        (true, true) => p.ghost_danger_hover_bg,
        (true, false) => p.ghost_hover_bg,
        (false, true) => p.ghost_danger_bg,
        (false, false) => p.ghost_bg,
    };
    let border = match (hovered, danger) {
        (true, true) => p.ghost_danger_hover_border,
        (true, false) => p.ghost_hover_border,
        (false, true) => p.ghost_danger_border,
        (false, false) => p.ghost_border,
    };
    fill_rrect(
        pm,
        rect.left as f32,
        rect.top as f32,
        rect.width() as f32,
        rect.height() as f32,
        7.0 * sf,
        bg,
    );
    stroke_rrect(
        pm,
        rect.left as f32 + 0.5 * sf,
        rect.top as f32 + 0.5 * sf,
        rect.width() as f32 - sf,
        rect.height() as f32 - sf,
        6.5 * sf,
        1.0 * sf,
        border,
    );
}

fn draw_nav_icon(pm: &mut Pixmap, idx: usize, x: f32, y: f32, sf: f32, color: Color) {
    let mut paint = Paint::default();
    paint.set_color(color);
    paint.anti_alias = true;
    let st = Stroke {
        width: 1.6,
        line_cap: LineCap::Round,
        line_join: LineJoin::Round,
        ..Stroke::default()
    };
    let mut pb = PathBuilder::new();
    match idx {
        0 => {
            // Keyboard icon (16x16)
            pb.move_to(2.0, 4.0);
            pb.line_to(14.0, 4.0);
            pb.line_to(14.0, 12.5);
            pb.line_to(2.0, 12.5);
            pb.close();
            pb.move_to(4.5, 7.0);
            pb.line_to(5.5, 7.0);
            pb.move_to(7.5, 7.0);
            pb.line_to(8.5, 7.0);
            pb.move_to(10.5, 7.0);
            pb.line_to(11.5, 7.0);
            pb.move_to(5.0, 10.0);
            pb.line_to(11.0, 10.0);
        }
        1 => {
            // Pin / Frame icon (16x16)
            pb.move_to(5.0, 2.5);
            pb.line_to(11.0, 2.5);
            pb.line_to(10.0, 7.2);
            pb.line_to(12.5, 9.5);
            pb.line_to(3.5, 9.5);
            pb.line_to(6.0, 7.2);
            pb.close();
            pb.move_to(8.0, 9.5);
            pb.line_to(8.0, 14.0);
        }
        _ => {
            // Update / Refresh arrows icon (16x16)
            pb.move_to(13.0, 7.5);
            pb.cubic_to(13.0, 4.5, 10.8, 2.5, 8.0, 2.5);
            pb.cubic_to(5.7, 2.5, 3.8, 3.9, 3.2, 5.8);
            pb.move_to(3.0, 8.5);
            pb.cubic_to(3.0, 11.5, 5.2, 13.5, 8.0, 13.5);
            pb.cubic_to(10.3, 13.5, 12.2, 12.1, 12.8, 10.2);
            pb.move_to(11.0, 6.2);
            pb.line_to(13.2, 7.8);
            pb.line_to(14.5, 5.4);
            pb.move_to(5.0, 9.8);
            pb.line_to(2.8, 8.2);
            pb.line_to(1.5, 10.6);
        }
    }
    if let Some(path) = pb.finish() {
        let xf = Transform::from_translate(x, y).pre_scale(sf, sf);
        pm.stroke_path(&path, &paint, &st, xf, None);
    }
}

fn lparam_xy(lparam: LPARAM) -> (i32, i32) {
    let x = (lparam.0 & 0xFFFF) as i16 as i32;
    let y = ((lparam.0 >> 16) & 0xFFFF) as i16 as i32;
    (x, y)
}

fn key_down(vk: u16) -> bool {
    unsafe { (GetAsyncKeyState(vk as i32) as u16 & 0x8000) != 0 }
}

unsafe extern "system" fn settings_wnd_proc(
    hwnd: HWND,
    msg: u32,
    wparam: WPARAM,
    lparam: LPARAM,
) -> LRESULT {
    match msg {
        WM_ERASEBKGND => return LRESULT(1),
        WM_PAINT => {
            let mut ps = PAINTSTRUCT::default();
            let hdc = unsafe { BeginPaint(hwnd, &mut ps) };
            SETTINGS.with(|cell| {
                if let Ok(borrow) = cell.try_borrow() {
                    if let Some(win) = borrow.as_ref() {
                        if let Some(mut surface) = DibSurface::new(win.client_w, win.client_h) {
                            win.render_to_surface(&mut surface);
                            unsafe {
                                let _ = BitBlt(
                                    hdc,
                                    0,
                                    0,
                                    win.client_w,
                                    win.client_h,
                                    Some(surface.dc),
                                    0,
                                    0,
                                    SRCCOPY,
                                );
                            }
                        }
                    }
                }
            });
            unsafe {
                let _ = EndPaint(hwnd, &ps);
            }
            return LRESULT(0);
        }
        WM_CTLCOLOREDIT | WM_CTLCOLORSTATIC => {
            let hdc = HDC(wparam.0 as *mut _);
            let theme_info = SETTINGS.with(|cell| {
                cell.try_borrow().ok().and_then(|b| {
                    b.as_ref().map(|w| {
                        let p = w.palette();
                        let brush = if p.dark {
                            w.edit_bg_dark.0.0 as isize
                        } else {
                            w.edit_bg_light.0.0 as isize
                        };
                        (p.input_text_rgb, p.input_bg_rgb, brush)
                    })
                })
            });
            if let Some((fg, bg, brush)) = theme_info {
                unsafe {
                    SetTextColor(hdc, rgb_colorref(fg.0, fg.1, fg.2));
                    SetBkColor(hdc, rgb_colorref(bg.0, bg.1, bg.2));
                }
                if brush != 0 {
                    return LRESULT(brush);
                }
            }
        }
        0x001A /* WM_SETTINGCHANGE */ => {
            SETTINGS.with(|cell| {
                if let Ok(borrow) = cell.try_borrow() {
                    if let Some(win) = borrow.as_ref() {
                        if win.cfg.borrow().theme == ThemeMode::System {
                            win.refresh_theme_surfaces();
                        }
                    }
                }
            });
        }
        WM_SETCURSOR => {
            let over_interactive = SETTINGS.with(|cell| {
                cell.try_borrow()
                    .ok()
                    .and_then(|b| b.as_ref().map(|w| w.hovered.get().is_some()))
                    .unwrap_or(false)
            });
            if over_interactive {
                unsafe {
                    if let Ok(c) = LoadCursorW(None, IDC_HAND) {
                        SetCursor(Some(c));
                    }
                }
                return LRESULT(1);
            }
        }
        WM_MOUSEMOVE => {
            let (x, y) = lparam_xy(lparam);
            let need_track = SETTINGS.with(|cell| {
                let Ok(borrow) = cell.try_borrow() else {
                    return false;
                };
                let Some(win) = borrow.as_ref() else {
                    return false;
                };
                let hit = win.hit_at(x, y);
                if hit != win.hovered.get() {
                    win.hovered.set(hit);
                    win.request_repaint();
                }
                if !win.tracking_leave.get() {
                    win.tracking_leave.set(true);
                    true
                } else {
                    false
                }
            });
            if need_track {
                let mut tme = TRACKMOUSEEVENT {
                    cbSize: std::mem::size_of::<TRACKMOUSEEVENT>() as u32,
                    dwFlags: TME_LEAVE,
                    hwndTrack: hwnd,
                    dwHoverTime: 0,
                };
                unsafe {
                    let _ = TrackMouseEvent(&mut tme);
                }
            }
            return LRESULT(0);
        }
        WM_MOUSELEAVE => {
            SETTINGS.with(|cell| {
                if let Ok(borrow) = cell.try_borrow() {
                    if let Some(win) = borrow.as_ref() {
                        win.tracking_leave.set(false);
                        if win.hovered.replace(None).is_some() {
                            win.request_repaint();
                        }
                    }
                }
            });
            return LRESULT(0);
        }
        WM_LBUTTONDOWN => {
            let (x, y) = lparam_xy(lparam);
            let hit = SETTINGS.with(|cell| {
                let borrow = cell.borrow();
                let win = borrow.as_ref()?;
                win.hit_at(x, y)
            });
            if let Some(kind) = hit {
                SETTINGS.with(|cell| {
                    if let Some(win) = cell.borrow().as_ref() {
                        win.handle_click(kind);
                    }
                });
            } else {
                // Clicking empty area exits hotkey recording & removes focus from edits
                SETTINGS.with(|cell| {
                    if let Some(win) = cell.borrow().as_ref() {
                        win.cancel_recording();
                        win.flush_pending_autosave();
                    }
                });
                let cur_focus = unsafe { GetFocus() };
                if cur_focus != hwnd {
                    unsafe {
                        let _ = SetFocus(Some(hwnd));
                    }
                }
            }
            return LRESULT(0);
        }
        WM_KEYDOWN | WM_SYSKEYDOWN => {
            let vk = wparam.0 as u32;
            let rec_slot = SETTINGS.with(|cell| {
                cell.borrow()
                    .as_ref()
                    .and_then(|w| w.recording_slot.get())
            });
            if let Some(slot) = rec_slot {
                if vk == VK_ESCAPE.0 as u32 {
                    SETTINGS.with(|cell| {
                        if let Some(win) = cell.borrow().as_ref() {
                            win.cancel_recording();
                            win.set_toast("已取消快捷键录制", ToastTone::Muted);
                        }
                    });
                    return LRESULT(0);
                }
                if vk == VK_BACK.0 as u32 || vk == VK_DELETE.0 as u32 {
                    SETTINGS.with(|cell| {
                        if let Some(win) = cell.borrow().as_ref() {
                            win.recording_slot.set(None);
                            {
                                let mut cfg = win.cfg.borrow_mut();
                                match slot {
                                    0 => cfg.screenshot_hotkey.clear(),
                                    1 => cfg.pin_clipboard_hotkey.clear(),
                                    _ => cfg.toggle_pins_hotkey.clear(),
                                }
                            }
                            win.save_config(false);
                        }
                    });
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
                    let formatted = HotkeySpec { modifiers: mods, vk }.format();
                    SETTINGS.with(|cell| {
                        if let Some(win) = cell.borrow().as_ref() {
                            win.recording_slot.set(None);
                            {
                                let mut cfg = win.cfg.borrow_mut();
                                match slot {
                                    0 => cfg.screenshot_hotkey = formatted.clone(),
                                    1 => cfg.pin_clipboard_hotkey = formatted.clone(),
                                    _ => cfg.toggle_pins_hotkey = formatted.clone(),
                                }
                            }
                            win.save_config(false);
                        }
                    });
                }
                return LRESULT(0);
            }
        }
        WM_MOUSEWHEEL => {
            let delta = ((wparam.0 >> 16) & 0xFFFF) as i16 as i32;
            SETTINGS.with(|cell| {
                if let Ok(borrow) = cell.try_borrow() {
                    if let Some(win) = borrow.as_ref() {
                        if win.active_tab.get() == 2 && win.notes_max_scroll.get() > 0 {
                            let step = win.s(26);
                            let next = (win.notes_scroll.get() - (delta / 120) * step)
                                .clamp(0, win.notes_max_scroll.get());
                            if next != win.notes_scroll.get() {
                                win.notes_scroll.set(next);
                                win.request_repaint();
                            }
                        }
                    }
                }
            });
            return LRESULT(0);
        }
        WM_COMMAND => {
            let id = (wparam.0 & 0xFFFF) as usize;
            let code = ((wparam.0 >> 16) & 0xFFFF) as u32;
            if matches!(
                id,
                ID_EDIT_SAVE_DIR
                    | ID_EDIT_CNB_URL
                    | ID_EDIT_GITHUB_URL
                    | ID_EDIT_PROXY_HOST
                    | ID_EDIT_PROXY_PORT
            ) {
                SETTINGS.with(|cell| {
                    if let Ok(borrow) = cell.try_borrow() {
                        if let Some(win) = borrow.as_ref() {
                            if code == EN_SETFOCUS {
                                win.focused_edit.set(Some(id));
                                win.request_repaint();
                            } else if code == EN_KILLFOCUS {
                                if win.focused_edit.get() == Some(id) {
                                    win.focused_edit.set(None);
                                }
                                win.flush_pending_autosave();
                                win.request_repaint();
                            } else if code == EN_CHANGE {
                                win.schedule_autosave();
                            }
                        }
                    }
                });
                return LRESULT(0);
            }
        }
        WM_TIMER => {
            if wparam.0 == TIMER_STATS {
                SETTINGS.with(|cell| {
                    if let Ok(borrow) = cell.try_borrow() {
                        if let Some(win) = borrow.as_ref() {
                            win.request_stats_repaint();
                        }
                    }
                });
                return LRESULT(0);
            }
            if wparam.0 == TIMER_AUTOSAVE {
                SETTINGS.with(|cell| {
                    if let Ok(borrow) = cell.try_borrow() {
                        if let Some(win) = borrow.as_ref() {
                            win.save_config(true);
                        }
                    }
                });
                return LRESULT(0);
            }
        }
        WM_CLOSE => {
            SETTINGS.with(|cell| {
                if let Some(win) = cell.borrow().as_ref() {
                    win.cancel_recording();
                    win.flush_pending_autosave();
                }
            });
            unsafe {
                let _ = DestroyWindow(hwnd);
            }
            return LRESULT(0);
        }
        WM_DESTROY => {
            unsafe {
                let _ = KillTimer(Some(hwnd), TIMER_STATS);
                let _ = KillTimer(Some(hwnd), TIMER_AUTOSAVE);
            }
            SETTINGS.with(|cell| {
                if let Some(win) = cell.borrow().as_ref() {
                    win.cancel_recording();
                }
            });
        }
        WM_NCDESTROY => {
            SETTINGS.with(|cell| {
                *cell.borrow_mut() = None;
            });
            crate::util::trim_working_set();
        }
        _ => {}
    }
    unsafe { DefWindowProcW(hwnd, msg, wparam, lparam) }
}

#[cfg(test)]
mod visual_tests {
    use super::*;
    use std::path::PathBuf;

    #[test]
    #[ignore]
    fn render_settings_visual_snapshots() {
        unsafe {
            let _ = windows::Win32::UI::HiDpi::SetProcessDpiAwarenessContext(
                windows::Win32::UI::HiDpi::DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2,
            );
        }
        let out_dir = std::env::var("RHPICTURE_SNAPSHOT_DIR")
            .map(PathBuf::from)
            .unwrap_or_else(|_| std::env::temp_dir());
        let cfg = AppConfig::default();
        let updater = UpdateService::new(cfg.update.clone(), None);
        open_settings(
            &cfg,
            updater,
            Some(0),
            SettingsCallbacks {
                on_config_saved: Box::new(|_| Vec::new()),
                on_suspend_hotkeys: Box::new(|_| {}),
                on_exit_for_update: Arc::new(|| {}),
            },
        );
        for tab in 0..3 {
            let path = out_dir.join(format!("settings-light-tab{tab}.png"));
            assert!(capture_settings_snapshot(tab, &path), "failed light snapshot tab {tab}");
        }
        // Switch live to Dark mode and capture Tab 0 to confirm instant dual-theme switch
        SETTINGS.with(|cell| {
            if let Some(win) = cell.borrow().as_ref() {
                win.cfg.borrow_mut().theme = ThemeMode::Dark;
                win.refresh_theme_surfaces();
            }
        });
        assert!(
            capture_settings_snapshot(0, &out_dir.join("settings-dark-tab0.png")),
            "failed dark snapshot tab 0"
        );
        close_settings_if_open();
    }
}
