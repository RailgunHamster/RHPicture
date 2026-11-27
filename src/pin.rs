use crate::config::{AppConfig, PinDoubleClickAction};
use crate::image_ops::{BgraImage, clipboard};
use crate::popup_menu::{MenuEntry, MenuIcon, show_popup_menu};
use crate::util::{
    DibSurface, OwnedFont, create_ui_font, current_hinstance, disable_ime, resolve_vk,
    rgb_colorref, save_file_dialog, scale_dpi, timestamp_filename, to_wide, trim_working_set,
    window_dpi,
};
use std::cell::RefCell;
use std::collections::HashMap;
use std::sync::Once;
use tiny_skia::{Color, FillRule, Paint, PathBuilder, Pixmap, Stroke, Transform};
use windows::Win32::Foundation::{COLORREF, HWND, LPARAM, LRESULT, POINT, RECT, WPARAM};
use windows::Win32::Graphics::Gdi::{
    BI_RGB, BITMAPINFO, BITMAPINFOHEADER, BeginPaint, BitBlt, DIB_RGB_COLORS, DT_CENTER,
    DT_NOCLIP, DT_SINGLELINE, DT_VCENTER, DrawTextW, EndPaint, HALFTONE, InvalidateRect,
    PAINTSTRUCT, SRCCOPY, SelectObject, SetBkMode, SetBrushOrgEx, SetStretchBltMode,
    SetTextColor, StretchDIBits, TRANSPARENT,
};
use windows::Win32::UI::Input::KeyboardAndMouse::{
    GetAsyncKeyState, ReleaseCapture, SetCapture, SetFocus, TME_LEAVE, TRACKMOUSEEVENT,
    TrackMouseEvent, VK_CONTROL, VK_DOWN, VK_ESCAPE, VK_LEFT, VK_MENU, VK_OEM_MINUS, VK_OEM_PLUS,
    VK_RIGHT, VK_SHIFT, VK_UP,
};
use windows::Win32::UI::Controls::WM_MOUSELEAVE;
use windows::Win32::UI::WindowsAndMessaging::{
    CS_DBLCLKS, CreateWindowExW, DefWindowProcW, DestroyWindow, GetCursorPos, GetWindowLongW,
    GetWindowRect, GWL_EXSTYLE, HTCAPTION, HWND_TOPMOST, IDC_SIZEALL, KillTimer, LWA_ALPHA,
    LoadCursorW, RegisterClassExW, SW_HIDE, SW_SHOW, SW_SHOWNOACTIVATE, SWP_FRAMECHANGED,
    SWP_NOACTIVATE, SWP_NOMOVE, SWP_NOSIZE, SWP_NOZORDER, SendMessageW, SetForegroundWindow,
    SetLayeredWindowAttributes, SetTimer, SetWindowLongW, SetWindowPos, ShowWindow, WINDOWPOS,
    WM_DESTROY, WM_ERASEBKGND, WM_KEYDOWN, WM_LBUTTONDBLCLK, WM_LBUTTONDOWN, WM_LBUTTONUP,
    WM_MOUSEMOVE, WM_MOUSEWHEEL, WM_NCDESTROY, WM_NCLBUTTONDOWN, WM_PAINT, WM_RBUTTONUP, WM_TIMER,
    WM_WINDOWPOSCHANGED, WNDCLASSEXW, WS_EX_LAYERED, WS_EX_TOOLWINDOW, WS_EX_TOPMOST,
    WS_EX_TRANSPARENT, WS_POPUP, WS_VISIBLE,
};
use windows::core::PCWSTR;

const PIN_CLASS: &str = "RHPicturePinClass";
const TIMER_HUD: usize = 1;

const MENU_COPY: u32 = 101;
const MENU_SAVE: u32 = 102;
const MENU_RESET_ZOOM: u32 = 103;
const MENU_ROTATE: u32 = 104;
const MENU_OPACITY_100: u32 = 110;
const MENU_OPACITY_85: u32 = 111;
const MENU_OPACITY_70: u32 = 112;
const MENU_OPACITY_50: u32 = 113;
const MENU_OPACITY_30: u32 = 114;
const MENU_CLICK_THROUGH: u32 = 120;
const MENU_HIDE: u32 = 130;
const MENU_HIDE_ALL: u32 = 131;
const MENU_CLOSE: u32 = 140;
const MENU_CLOSE_ALL: u32 = 141;

#[derive(Clone, Debug)]
pub struct PinSummary {
    pub id: u64,
    pub width: u32,
    pub height: u32,
    pub hidden: bool,
    pub click_through: bool,
}

enum PinStorage {
    Raw(BgraImage),
    /// Lossless PNG bytes while hidden to cut RAM usage by ~85-95%.
    Compressed {
        width: u32,
        height: u32,
        png: Vec<u8>,
    },
}

impl PinStorage {
    fn dimensions(&self) -> (u32, u32) {
        match self {
            PinStorage::Raw(img) => (img.width, img.height),
            PinStorage::Compressed { width, height, .. } => (*width, *height),
        }
    }

    fn ensure_raw(&mut self) -> Option<&BgraImage> {
        if let PinStorage::Compressed { png, .. } = self {
            if let Ok(img) = BgraImage::from_encoded_bytes(png) {
                *self = PinStorage::Raw(img);
            }
        }
        match self {
            PinStorage::Raw(img) => Some(img),
            PinStorage::Compressed { .. } => None,
        }
    }

    fn compress_for_hidden(&mut self) {
        if let PinStorage::Raw(img) = self {
            // Only compress when raw bitmap is > 128 KB so tiny pins stay instant
            if img.data.len() > 128 * 1024 {
                if let Ok(png) = img.to_png_bytes() {
                    let (width, height) = (img.width, img.height);
                    *self = PinStorage::Compressed { width, height, png };
                }
            }
        }
    }
}

#[derive(PartialEq, Eq)]
struct PinRenderKey {
    win_w: i32,
    win_h: i32,
    hovered: bool,
    show_border: bool,
    dark_theme: bool,
    hud_text: Option<String>,
    image_gen: u64,
}

struct CachedPinSurface {
    key: PinRenderKey,
    surface: DibSurface,
}

struct PinWindow {
    id: u64,
    hwnd: HWND,
    storage: PinStorage,
    image_gen: u64,
    x: i32,
    y: i32,
    scale: f64,
    opacity: u8,
    hidden: bool,
    click_through: bool,
    hovered: bool,
    tracking_leave: bool,
    dragging: bool,
    drag_start_cursor: (i32, i32),
    hud_text: Option<String>,
    hud_dpi: u32,
    font_hud: OwnedFont,
    cached_surface: Option<CachedPinSurface>,
}

struct PinManager {
    next_id: u64,
    pins: HashMap<isize, PinWindow>,
    config: AppConfig,
}

impl Default for PinManager {
    fn default() -> Self {
        Self {
            next_id: 1,
            pins: HashMap::new(),
            config: AppConfig::default(),
        }
    }
}

thread_local! {
    static MANAGER: RefCell<PinManager> = RefCell::new(PinManager::default());
}

static REGISTER_CLASS: Once = Once::new();

fn ensure_class_registered() {
    REGISTER_CLASS.call_once(|| unsafe {
        let class_w = to_wide(PIN_CLASS);
        let wc = WNDCLASSEXW {
            cbSize: std::mem::size_of::<WNDCLASSEXW>() as u32,
            style: CS_DBLCLKS,
            lpfnWndProc: Some(pin_wnd_proc),
            hInstance: current_hinstance(),
            hCursor: LoadCursorW(None, IDC_SIZEALL).unwrap_or_default(),
            lpszClassName: PCWSTR(class_w.as_ptr()),
            ..Default::default()
        };
        let _ = RegisterClassExW(&wc);
    });
}

pub fn sync_config(cfg: &AppConfig) {
    let hwnds: Vec<HWND> = MANAGER.with(|m| {
        let mut mgr = m.borrow_mut();
        mgr.config = cfg.clone();
        mgr.pins.values().map(|p| p.hwnd).collect()
    });
    for hwnd in hwnds {
        unsafe {
            let _ = InvalidateRect(Some(hwnd), None, false);
        }
    }
}

pub fn create_pin(image: BgraImage, screen_x: i32, screen_y: i32) -> Result<u64, String> {
    ensure_class_registered();

    let w = image.width.max(1) as i32;
    let h = image.height.max(1) as i32;
    let id = MANAGER.with(|m| {
        let mut mgr = m.borrow_mut();
        let id = mgr.next_id;
        mgr.next_id += 1;
        id
    });

    let class_w = to_wide(PIN_CLASS);
    let title_w = to_wide(&format!("RHPicture 贴图 #{id}"));
    // Do NOT attach WS_EX_LAYERED by default at 100% opacity so DWM composites natively at full speed!
    let hwnd = unsafe {
        CreateWindowExW(
            WS_EX_TOPMOST | WS_EX_TOOLWINDOW,
            PCWSTR(class_w.as_ptr()),
            PCWSTR(title_w.as_ptr()),
            WS_POPUP | WS_VISIBLE,
            screen_x,
            screen_y,
            w,
            h,
            None,
            None,
            Some(current_hinstance()),
            None,
        )
    }
    .map_err(|e| format!("创建贴图窗口失败：{e}"))?;

    disable_ime(hwnd);

    let initial_dpi = window_dpi(hwnd);
    let pin = PinWindow {
        id,
        hwnd,
        storage: PinStorage::Raw(image),
        image_gen: 1,
        x: screen_x,
        y: screen_y,
        scale: 1.0,
        opacity: 255,
        hidden: false,
        click_through: false,
        hovered: false,
        tracking_leave: false,
        dragging: false,
        drag_start_cursor: (0, 0),
        hud_text: None,
        hud_dpi: initial_dpi,
        font_hud: OwnedFont(create_ui_font(initial_dpi, 10, true)),
        cached_surface: None,
    };

    MANAGER.with(|m| {
        m.borrow_mut().pins.insert(hwnd.0 as isize, pin);
    });

    unsafe {
        let _ = SetWindowPos(
            hwnd,
            Some(HWND_TOPMOST),
            screen_x,
            screen_y,
            w,
            h,
            Default::default(),
        );
        let _ = ShowWindow(hwnd, SW_SHOW);
        let _ = SetForegroundWindow(hwnd);
        let _ = SetFocus(Some(hwnd));
        let _ = InvalidateRect(Some(hwnd), None, false);
    }
    Ok(id)
}

pub fn pin_counts() -> (usize, usize) {
    MANAGER.with(|m| {
        let mgr = m.borrow();
        let visible = mgr.pins.values().filter(|p| !p.hidden).count();
        let hidden = mgr.pins.values().filter(|p| p.hidden).count();
        (visible, hidden)
    })
}

pub fn list_pins() -> Vec<PinSummary> {
    MANAGER.with(|m| {
        let mut items: Vec<PinSummary> = m
            .borrow()
            .pins
            .values()
            .map(|p| {
                let (width, height) = p.storage.dimensions();
                PinSummary {
                    id: p.id,
                    width,
                    height,
                    hidden: p.hidden,
                    click_through: p.click_through,
                }
            })
            .collect();
        items.sort_by_key(|p| p.id);
        items
    })
}

pub fn hide_all_pins() {
    let hwnds: Vec<HWND> = MANAGER.with(|m| {
        let mut mgr = m.borrow_mut();
        let mut list = Vec::new();
        for pin in mgr.pins.values_mut() {
            if !pin.hidden {
                pin.hidden = true;
                pin.storage.compress_for_hidden();
                list.push(pin.hwnd);
            }
        }
        list
    });
    for hwnd in hwnds {
        unsafe {
            let _ = ShowWindow(hwnd, SW_HIDE);
        }
    }
    trim_working_set();
}

pub fn show_all_pins() {
    let hwnds: Vec<HWND> = MANAGER.with(|m| {
        let mut mgr = m.borrow_mut();
        let mut list = Vec::new();
        for pin in mgr.pins.values_mut() {
            if pin.hidden {
                pin.hidden = false;
                let _ = pin.storage.ensure_raw();
                list.push(pin.hwnd);
            }
        }
        list
    });
    for hwnd in hwnds {
        unsafe {
            let _ = ShowWindow(hwnd, SW_SHOWNOACTIVATE);
            let _ = InvalidateRect(Some(hwnd), None, false);
        }
    }
}

pub fn toggle_all_pins() {
    let (visible, hidden) = pin_counts();
    if visible > 0 {
        hide_all_pins();
    } else if hidden > 0 {
        show_all_pins();
    }
}

pub fn show_pin_by_id(id: u64) {
    let hwnd = MANAGER.with(|m| {
        let mut mgr = m.borrow_mut();
        let pin = mgr.pins.values_mut().find(|p| p.id == id)?;
        pin.hidden = false;
        let _ = pin.storage.ensure_raw();
        Some(pin.hwnd)
    });
    if let Some(hwnd) = hwnd {
        unsafe {
            let _ = ShowWindow(hwnd, SW_SHOW);
            let _ = SetWindowPos(hwnd, Some(HWND_TOPMOST), 0, 0, 0, 0, SWP_NOMOVE | SWP_NOSIZE);
            let _ = SetForegroundWindow(hwnd);
            let _ = SetFocus(Some(hwnd));
            let _ = InvalidateRect(Some(hwnd), None, false);
        }
    }
}

pub fn disable_all_click_through() {
    let hwnds: Vec<HWND> = MANAGER.with(|m| {
        let mut mgr = m.borrow_mut();
        let mut list = Vec::new();
        for pin in mgr.pins.values_mut() {
            if pin.click_through {
                pin.click_through = false;
                list.push(pin.hwnd);
            }
        }
        list
    });
    for hwnd in hwnds {
        apply_click_through_style(hwnd, false);
    }
}

pub fn close_all_pins() {
    let hwnds: Vec<HWND> = MANAGER.with(|m| m.borrow().pins.values().map(|p| p.hwnd).collect());
    for hwnd in hwnds {
        unsafe {
            let _ = DestroyWindow(hwnd);
        }
    }
    trim_working_set();
}

fn sync_window_exstyle(hwnd: HWND, opacity: u8, click_through: bool) {
    let need_layered = opacity < 255 || click_through;
    unsafe {
        let ex = GetWindowLongW(hwnd, GWL_EXSTYLE) as u32;
        let mut new_ex = ex;
        if need_layered {
            new_ex |= WS_EX_LAYERED.0;
        } else {
            new_ex &= !WS_EX_LAYERED.0;
        }
        if click_through {
            new_ex |= WS_EX_TRANSPARENT.0;
        } else {
            new_ex &= !WS_EX_TRANSPARENT.0;
        }
        if new_ex != ex {
            SetWindowLongW(hwnd, GWL_EXSTYLE, new_ex as i32);
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
        if need_layered {
            let _ = SetLayeredWindowAttributes(hwnd, COLORREF(0), opacity, LWA_ALPHA);
        }
    }
}

fn apply_click_through_style(hwnd: HWND, enabled: bool) {
    let opacity = MANAGER.with(|m| {
        m.borrow()
            .pins
            .get(&(hwnd.0 as isize))
            .map(|p| p.opacity)
            .unwrap_or(255)
    });
    sync_window_exstyle(hwnd, opacity, enabled);
}

impl PinWindow {
    fn display_size(&self) -> (i32, i32) {
        let (w, h) = self.storage.dimensions();
        let dw = ((w as f64) * self.scale).round().max(24.0) as i32;
        let dh = ((h as f64) * self.scale).round().max(24.0) as i32;
        (dw, dh)
    }

    fn set_hud(&mut self, text: impl Into<String>) {
        self.hud_text = Some(text.into());
        unsafe {
            let _ = SetTimer(Some(self.hwnd), TIMER_HUD, 900, None);
            let _ = InvalidateRect(Some(self.hwnd), None, false);
        }
    }

    fn zoom_by(&mut self, factor: f64, anchor_screen: Option<(i32, i32)>) -> Option<(i32, i32, i32, i32)> {
        let (old_w, old_h) = self.display_size();
        let new_scale = (self.scale * factor).clamp(0.10, 8.0);
        if (new_scale - self.scale).abs() < 1e-4 {
            return None;
        }
        self.scale = new_scale;
        let (new_w, new_h) = self.display_size();

        if let Some((ax, ay)) = anchor_screen {
            let rel_x = (ax - self.x) as f64 / (old_w.max(1) as f64);
            let rel_y = (ay - self.y) as f64 / (old_h.max(1) as f64);
            self.x = (ax as f64 - rel_x * new_w as f64).round() as i32;
            self.y = (ay as f64 - rel_y * new_h as f64).round() as i32;
        } else {
            self.x += (old_w - new_w) / 2;
            self.y += (old_h - new_h) / 2;
        }

        self.set_hud(format!("{}%", (self.scale * 100.0).round() as i32));
        Some((self.x, self.y, new_w, new_h))
    }

    fn reset_view(&mut self) -> (i32, i32, i32, i32) {
        let (old_w, old_h) = self.display_size();
        self.scale = 1.0;
        self.opacity = 255;
        let (new_w, new_h) = self.display_size();
        self.x += (old_w - new_w) / 2;
        self.y += (old_h - new_h) / 2;
        self.set_hud("100%");
        (self.x, self.y, new_w, new_h)
    }

    fn set_opacity_value(&mut self, alpha: u8) -> u8 {
        let clamped = alpha.clamp(38, 255);
        self.opacity = clamped;
        let pct = ((clamped as f64 / 255.0) * 100.0).round() as i32;
        self.set_hud(format!("透明度 {pct}%"));
        clamped
    }

    fn rotate_90(&mut self) -> Option<(i32, i32, i32, i32)> {
        if let Some(img) = self.storage.ensure_raw() {
            let rotated = img.rotate_90_cw();
            self.storage = PinStorage::Raw(rotated);
            self.image_gen = self.image_gen.wrapping_add(1);
            let (new_w, new_h) = self.display_size();
            self.set_hud("已旋转 90°");
            return Some((self.x, self.y, new_w, new_h));
        }
        None
    }

    fn mark_hidden(&mut self) {
        self.hidden = true;
        self.dragging = false;
        self.hovered = false;
        self.cached_surface = None;
        self.storage.compress_for_hidden();
    }

    fn paint(&mut self, show_border: bool, dark_theme: bool) {
        let mut ps = PAINTSTRUCT::default();
        let hdc = unsafe { BeginPaint(self.hwnd, &mut ps) };

        let mut rect = RECT::default();
        unsafe {
            let _ = GetWindowRect(self.hwnd, &mut rect);
        }
        let win_w = (rect.right - rect.left).max(1);
        let win_h = (rect.bottom - rect.top).max(1);

        let key = PinRenderKey {
            win_w,
            win_h,
            hovered: self.hovered,
            show_border,
            dark_theme,
            hud_text: self.hud_text.clone(),
            image_gen: self.image_gen,
        };

        let cache_hit = self
            .cached_surface
            .as_ref()
            .is_some_and(|c| c.key == key);

        if !cache_hit {
            // Reuse existing DibSurface when dimensions match to avoid multi-MB allocations
            let existing = self
                .cached_surface
                .take()
                .map(|c| c.surface)
                .filter(|s| s.width == win_w && s.height == win_h);
            if let Some(mut surface) = existing.or_else(|| DibSurface::new(win_w, win_h)) {
                if let Some(img) = self.storage.ensure_raw() {
                    if win_w == img.width as i32 && win_h == img.height as i32 {
                        // Fast 1:1 raw copy (50x faster than StretchDIBits HALFTONE!)
                        surface.pixels_mut().copy_from_slice(&img.data);
                    } else {
                        let bmi = BITMAPINFO {
                            bmiHeader: BITMAPINFOHEADER {
                                biSize: std::mem::size_of::<BITMAPINFOHEADER>() as u32,
                                biWidth: img.width as i32,
                                biHeight: -(img.height as i32), // top-down BGRA
                                biPlanes: 1,
                                biBitCount: 32,
                                biCompression: BI_RGB.0,
                                ..Default::default()
                            },
                            ..Default::default()
                        };
                        unsafe {
                            let _ = SetStretchBltMode(surface.dc, HALFTONE);
                            let _ = SetBrushOrgEx(surface.dc, 0, 0, None);
                            let _ = StretchDIBits(
                                surface.dc,
                                0,
                                0,
                                win_w,
                                win_h,
                                0,
                                0,
                                img.width as i32,
                                img.height as i32,
                                Some(img.data.as_ptr() as *const _),
                                &bmi,
                                DIB_RGB_COLORS,
                                SRCCOPY,
                            );
                        }
                    }
                }

                let dpi = window_dpi(self.hwnd);
                if dpi != self.hud_dpi {
                    self.hud_dpi = dpi;
                    self.font_hud = OwnedFont(create_ui_font(dpi, 10, true));
                }

                if show_border || self.hovered {
                    let bw = scale_dpi(1, dpi).clamp(1, 2) as usize;
                    let border = if self.hovered {
                        [248u8, 189, 56, 255] // #38BDF8 sky-cyan in BGRA
                    } else {
                        [105u8, 85, 71, 255] // refined slate edge
                    };
                    draw_border(surface.pixels_mut(), win_w as usize, win_h as usize, bw, border);
                }

                if let Some(ref text) = self.hud_text {
                    draw_hud_pill(&mut surface, self.font_hud.0, dpi, text, dark_theme);
                }

                self.cached_surface = Some(CachedPinSurface { key, surface });
            }
        }

        if let Some(ref cached) = self.cached_surface {
            let x = ps.rcPaint.left.clamp(0, win_w);
            let y = ps.rcPaint.top.clamp(0, win_h);
            let r = ps.rcPaint.right.clamp(x, win_w);
            let b = ps.rcPaint.bottom.clamp(y, win_h);
            let bw = (r - x).max(0);
            let bh = (b - y).max(0);
            if bw > 0 && bh > 0 {
                unsafe {
                    let _ = BitBlt(hdc, x, y, bw, bh, Some(cached.surface.dc), x, y, SRCCOPY);
                }
            }
        }

        unsafe {
            let _ = EndPaint(self.hwnd, &ps);
        }
    }
}

fn draw_border(pixels: &mut [u8], w: usize, h: usize, thickness: usize, bgra: [u8; 4]) {
    if w < 2 || h < 2 {
        return;
    }
    let t = thickness.min(w / 2).min(h / 2).max(1);
    for b in 0..t {
        for x in 0..w {
            let top = (b * w + x) * 4;
            let bot = ((h - 1 - b) * w + x) * 4;
            pixels[top..top + 4].copy_from_slice(&bgra);
            pixels[bot..bot + 4].copy_from_slice(&bgra);
        }
        for y in 0..h {
            let left = (y * w + b) * 4;
            let right = (y * w + w - 1 - b) * 4;
            pixels[left..left + 4].copy_from_slice(&bgra);
            pixels[right..right + 4].copy_from_slice(&bgra);
        }
    }
}

fn draw_hud_pill(
    surface: &mut DibSurface,
    font: windows::Win32::Graphics::Gdi::HFONT,
    dpi: u32,
    text: &str,
    dark_theme: bool,
) {
    let s = |v: i32| scale_dpi(v, dpi);
    let sf = dpi as f32 / 96.0;
    let desired_w = s((text.chars().count() as i32) * 11 + 26).max(s(68));
    let max_w = (surface.width - s(8)).max(24);
    let pw = desired_w.min(max_w);
    let ph = s(30).min((surface.height - s(4)).max(20));
    let px = ((surface.width - pw) / 2).max(0);
    let py = ((surface.height - ph) / 2).max(0);

    if let Some(mut pm) = Pixmap::new(pw as u32, ph as u32) {
        let r = (10.0 * sf).min(pw as f32 * 0.5).min(ph as f32 * 0.5);
        let k = 0.5522848 * r;
        let (w, h) = (pw as f32, ph as f32);
        let mut pb = PathBuilder::new();
        pb.move_to(r, 0.0);
        pb.line_to(w - r, 0.0);
        pb.cubic_to(w - r + k, 0.0, w, r - k, w, r);
        pb.line_to(w, h - r);
        pb.cubic_to(w, h - r + k, w - r + k, h, w - r, h);
        pb.line_to(r, h);
        pb.cubic_to(r - k, h, 0.0, h - r + k, 0.0, h - r);
        pb.line_to(0.0, r);
        pb.cubic_to(0.0, r - k, r - k, 0.0, r, 0.0);
        pb.close();
        if let Some(path) = pb.finish() {
            let mut paint = Paint::default();
            paint.set_color(if dark_theme {
                Color::from_rgba8(15, 23, 42, 232)
            } else {
                Color::from_rgba8(255, 255, 255, 240)
            });
            paint.anti_alias = true;
            pm.fill_path(&path, &paint, FillRule::Winding, Transform::identity(), None);

            let mut rim = Paint::default();
            rim.set_color(if dark_theme {
                Color::from_rgba8(255, 255, 255, 42)
            } else {
                Color::from_rgba8(15, 23, 42, 34)
            });
            rim.anti_alias = true;
            let st = Stroke {
                width: (1.0 * sf).max(1.0),
                ..Stroke::default()
            };
            pm.stroke_path(&path, &rim, &st, Transform::identity(), None);
        }

        let sw = surface.width;
        let sh = surface.height;
        let src = pm.data();
        let dst = surface.pixels_mut();
        for y in 0..ph {
            let sy = py + y;
            if sy < 0 || sy >= sh {
                continue;
            }
            for x in 0..pw {
                let sx = px + x;
                if sx < 0 || sx >= sw {
                    continue;
                }
                let si = ((y * pw + x) * 4) as usize;
                let sa = src[si + 3] as u32;
                if sa == 0 {
                    continue;
                }
                let inv = 255 - sa;
                let di = ((sy * sw + sx) * 4) as usize;
                dst[di] = ((src[si + 2] as u32) + ((dst[di] as u32 * inv + 127) / 255)).min(255) as u8;
                dst[di + 1] =
                    ((src[si + 1] as u32) + ((dst[di + 1] as u32 * inv + 127) / 255)).min(255) as u8;
                dst[di + 2] =
                    ((src[si] as u32) + ((dst[di + 2] as u32 * inv + 127) / 255)).min(255) as u8;
                dst[di + 3] = 255;
            }
        }
    }

    let wide = to_wide(text);
    let mut r = RECT {
        left: px,
        top: py,
        right: px + pw,
        bottom: py + ph,
    };
    unsafe {
        let old = SelectObject(surface.dc, windows::Win32::Graphics::Gdi::HGDIOBJ(font.0));
        SetBkMode(surface.dc, TRANSPARENT);
        let tc = if dark_theme {
            (248, 250, 252)
        } else {
            (15, 23, 42)
        };
        SetTextColor(surface.dc, rgb_colorref(tc.0, tc.1, tc.2));
        let _ = DrawTextW(
            surface.dc,
            &mut wide[..wide.len() - 1].to_vec(),
            &mut r,
            DT_CENTER | DT_VCENTER | DT_SINGLELINE | DT_NOCLIP,
        );
        let _ = SelectObject(surface.dc, old);
    }
}

fn save_pin_image(hwnd: HWND) {
    let extracted = MANAGER.with(|m| {
        let mut mgr = m.borrow_mut();
        let cfg = mgr.config.clone();
        let img = mgr
            .pins
            .get_mut(&(hwnd.0 as isize))
            .and_then(|p| p.storage.ensure_raw().cloned())?;
        Some((img, cfg))
    });
    let Some((img, cfg)) = extracted else {
        return;
    };

    let saved = if cfg.quick_save_no_dialog {
        let dir = cfg.resolved_save_dir();
        let path = dir.join(timestamp_filename(cfg.save_format.extension()));
        img.save_to_file(&path, cfg.save_format).is_ok()
    } else if let Some((path, fmt)) =
        save_file_dialog(Some(hwnd), &cfg.resolved_save_dir(), cfg.save_format)
    {
        img.save_to_file(&path, fmt).is_ok()
    } else {
        false
    };

    if saved {
        MANAGER.with(|m| {
            if let Some(pin) = m.borrow_mut().pins.get_mut(&(hwnd.0 as isize)) {
                pin.set_hud("已保存");
            }
        });
    }
}

fn copy_pin_image(hwnd: HWND) {
    let img = MANAGER.with(|m| {
        m.borrow_mut()
            .pins
            .get_mut(&(hwnd.0 as isize))
            .and_then(|p| p.storage.ensure_raw().cloned())
    });
    if let Some(img) = img {
        if clipboard::copy_image(&img).is_ok() {
            MANAGER.with(|m| {
                if let Some(pin) = m.borrow_mut().pins.get_mut(&(hwnd.0 as isize)) {
                    pin.set_hud("已复制");
                }
            });
        }
    }
}

fn show_context_menu(hwnd: HWND) {
    let (opacity, click_through, dark_theme) = MANAGER.with(|m| {
        let mgr = m.borrow();
        let dark = mgr.config.theme.is_dark();
        let (op, ct) = mgr
            .pins
            .get(&(hwnd.0 as isize))
            .map(|p| (p.opacity, p.click_through))
            .unwrap_or((255, false));
        (op, ct, dark)
    });

    let op_pct = ((opacity as f64 / 255.0) * 100.0).round() as i32;
    let entries = vec![
        MenuEntry::Action {
            id: MENU_COPY,
            icon: MenuIcon::Copy,
            label: "复制图像".into(),
            shortcut: "Ctrl+C".into(),
            enabled: true,
            checked: false,
            danger: false,
        },
        MenuEntry::Action {
            id: MENU_SAVE,
            icon: MenuIcon::Save,
            label: "保存到本地…".into(),
            shortcut: "Ctrl+S".into(),
            enabled: true,
            checked: false,
            danger: false,
        },
        MenuEntry::Separator,
        MenuEntry::Action {
            id: MENU_RESET_ZOOM,
            icon: MenuIcon::Reset,
            label: "重置为 100%".into(),
            shortcut: "0".into(),
            enabled: true,
            checked: false,
            danger: false,
        },
        MenuEntry::Action {
            id: MENU_ROTATE,
            icon: MenuIcon::Rotate,
            label: "顺时针旋转 90°".into(),
            shortcut: "R".into(),
            enabled: true,
            checked: false,
            danger: false,
        },
        MenuEntry::Chips {
            icon: MenuIcon::Opacity,
            label: "透明度（Ctrl+滚轮）".into(),
            items: vec![
                (MENU_OPACITY_100, "100%".into(), op_pct >= 93),
                (MENU_OPACITY_85, "85%".into(), (78..93).contains(&op_pct)),
                (MENU_OPACITY_70, "70%".into(), (60..78).contains(&op_pct)),
                (MENU_OPACITY_50, "50%".into(), (40..60).contains(&op_pct)),
                (MENU_OPACITY_30, "30%".into(), op_pct < 40),
            ],
        },
        MenuEntry::Action {
            id: MENU_CLICK_THROUGH,
            icon: MenuIcon::Pointer,
            label: "鼠标穿透（可从托盘恢复）".into(),
            shortcut: String::new(),
            enabled: true,
            checked: click_through,
            danger: false,
        },
        MenuEntry::Separator,
        MenuEntry::Action {
            id: MENU_HIDE,
            icon: MenuIcon::EyeOff,
            label: "隐藏此贴图".into(),
            shortcut: "H".into(),
            enabled: true,
            checked: false,
            danger: false,
        },
        MenuEntry::Action {
            id: MENU_HIDE_ALL,
            icon: MenuIcon::EyeOff,
            label: "隐藏全部贴图".into(),
            shortcut: String::new(),
            enabled: true,
            checked: false,
            danger: false,
        },
        MenuEntry::Separator,
        MenuEntry::Action {
            id: MENU_CLOSE,
            icon: MenuIcon::Close,
            label: "关闭此贴图".into(),
            shortcut: "Esc".into(),
            enabled: true,
            checked: false,
            danger: true,
        },
        MenuEntry::Action {
            id: MENU_CLOSE_ALL,
            icon: MenuIcon::Close,
            label: "关闭全部贴图".into(),
            shortcut: String::new(),
            enabled: true,
            checked: false,
            danger: true,
        },
    ];

    let mut pt = POINT::default();
    unsafe {
        let _ = GetCursorPos(&mut pt);
    }
    let Some(cmd) = show_popup_menu(hwnd, pt.x, pt.y, entries, dark_theme) else {
        return;
    };

    match cmd {
        MENU_COPY => copy_pin_image(hwnd),
        MENU_SAVE => save_pin_image(hwnd),
        MENU_RESET_ZOOM => reset_pin_view(hwnd),
        MENU_ROTATE => rotate_pin_90(hwnd),
        MENU_OPACITY_100 => set_pin_opacity(hwnd, 255),
        MENU_OPACITY_85 => set_pin_opacity(hwnd, 217),
        MENU_OPACITY_70 => set_pin_opacity(hwnd, 179),
        MENU_OPACITY_50 => set_pin_opacity(hwnd, 128),
        MENU_OPACITY_30 => set_pin_opacity(hwnd, 77),
        MENU_CLICK_THROUGH => {
            let next = MANAGER.with(|m| {
                let mut mgr = m.borrow_mut();
                let pin = mgr.pins.get_mut(&(hwnd.0 as isize))?;
                pin.click_through = !pin.click_through;
                Some(pin.click_through)
            });
            if let Some(enabled) = next {
                apply_click_through_style(hwnd, enabled);
            }
        }
        MENU_HIDE => hide_pin_window(hwnd),
        MENU_HIDE_ALL => hide_all_pins(),
        MENU_CLOSE => unsafe {
            let _ = DestroyWindow(hwnd);
        },
        MENU_CLOSE_ALL => close_all_pins(),
        _ => {}
    }
}

fn hide_pin_window(hwnd: HWND) {
    let found = MANAGER.with(|m| {
        if let Some(p) = m.borrow_mut().pins.get_mut(&(hwnd.0 as isize)) {
            p.mark_hidden();
            true
        } else {
            false
        }
    });
    if found {
        unsafe {
            let _ = ReleaseCapture();
            let _ = ShowWindow(hwnd, SW_HIDE);
        }
        trim_working_set();
    }
}

fn apply_pin_pos(hwnd: HWND, x: i32, y: i32, w: i32, h: i32) {
    unsafe {
        let _ = SetWindowPos(hwnd, Some(HWND_TOPMOST), x, y, w, h, SWP_NOACTIVATE);
    }
}

fn reset_pin_view(hwnd: HWND) {
    let geom = MANAGER.with(|m| {
        m.borrow_mut()
            .pins
            .get_mut(&(hwnd.0 as isize))
            .map(|p| (p.reset_view(), p.click_through))
    });
    if let Some(((x, y, w, h), ct)) = geom {
        sync_window_exstyle(hwnd, 255, ct);
        apply_pin_pos(hwnd, x, y, w, h);
    }
}

fn rotate_pin_90(hwnd: HWND) {
    let geom = MANAGER.with(|m| {
        m.borrow_mut()
            .pins
            .get_mut(&(hwnd.0 as isize))
            .and_then(|p| p.rotate_90())
    });
    if let Some((x, y, w, h)) = geom {
        apply_pin_pos(hwnd, x, y, w, h);
    }
}

fn zoom_pin_by(hwnd: HWND, factor: f64, anchor: Option<(i32, i32)>) {
    let geom = MANAGER.with(|m| {
        m.borrow_mut()
            .pins
            .get_mut(&(hwnd.0 as isize))
            .and_then(|p| p.zoom_by(factor, anchor))
    });
    if let Some((x, y, w, h)) = geom {
        apply_pin_pos(hwnd, x, y, w, h);
    }
}

fn set_pin_opacity(hwnd: HWND, alpha: u8) {
    let updated = MANAGER.with(|m| {
        m.borrow_mut()
            .pins
            .get_mut(&(hwnd.0 as isize))
            .map(|p| (p.set_opacity_value(alpha), p.click_through))
    });
    if let Some((a, ct)) = updated {
        sync_window_exstyle(hwnd, a, ct);
    }
}

fn key_down(vk: u16) -> bool {
    unsafe { (GetAsyncKeyState(vk as i32) as u16 & 0x8000) != 0 }
}

unsafe extern "system" fn pin_wnd_proc(
    hwnd: HWND,
    msg: u32,
    wparam: WPARAM,
    lparam: LPARAM,
) -> LRESULT {
    match msg {
        WM_ERASEBKGND => return LRESULT(1),
        WM_PAINT => {
            MANAGER.with(|m| {
                let mut mgr = m.borrow_mut();
                let show_border = mgr.config.pin_show_border;
                let dark_theme = mgr.config.theme.is_dark();
                if let Some(pin) = mgr.pins.get_mut(&(hwnd.0 as isize)) {
                    pin.paint(show_border, dark_theme);
                }
            });
            return LRESULT(0);
        }
        WM_LBUTTONDOWN => {
            let mut pt = POINT::default();
            unsafe {
                let _ = GetCursorPos(&mut pt);
                let _ = SetForegroundWindow(hwnd);
                let _ = SetFocus(Some(hwnd));
                SetCapture(hwnd);
            }
            MANAGER.with(|m| {
                if let Some(pin) = m.borrow_mut().pins.get_mut(&(hwnd.0 as isize)) {
                    pin.dragging = true;
                    pin.drag_start_cursor = (pt.x, pt.y);
                }
            });
            return LRESULT(0);
        }
        WM_MOUSEMOVE => {
            let mut pt = POINT::default();
            unsafe {
                let _ = GetCursorPos(&mut pt);
            }
            let (need_track, need_repaint, start_native_drag) = MANAGER.with(|m| {
                if let Some(pin) = m.borrow_mut().pins.get_mut(&(hwnd.0 as isize)) {
                    let mut track = false;
                    if !pin.tracking_leave {
                        pin.tracking_leave = true;
                        track = true;
                    }
                    let mut repaint = false;
                    if !pin.hovered {
                        pin.hovered = true;
                        repaint = true;
                    }
                    let mut native_drag = false;
                    if pin.dragging {
                        let dx = (pt.x - pin.drag_start_cursor.0).abs();
                        let dy = (pt.y - pin.drag_start_cursor.1).abs();
                        // Hand off to native Windows DWM compositor move as soon as cursor moves >= 2px
                        // (preserves WM_LBUTTONDBLCLK on click while giving silky 144/240Hz DWM dragging!)
                        if dx >= 2 || dy >= 2 {
                            pin.dragging = false;
                            native_drag = true;
                        }
                    }
                    (track, repaint, native_drag)
                } else {
                    (false, false, false)
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
            if need_repaint {
                unsafe {
                    let _ = InvalidateRect(Some(hwnd), None, false);
                }
            }
            if start_native_drag {
                let packed = (((pt.y as i16 as u16) as usize) << 16) | ((pt.x as i16 as u16) as usize);
                unsafe {
                    let _ = ReleaseCapture();
                    let _ = SendMessageW(
                        hwnd,
                        WM_NCLBUTTONDOWN,
                        Some(WPARAM(HTCAPTION as usize)),
                        Some(LPARAM(packed as isize)),
                    );
                }
            }
            return LRESULT(0);
        }
        WM_WINDOWPOSCHANGED => {
            if lparam.0 != 0 {
                let wp = unsafe { &*(lparam.0 as *const WINDOWPOS) };
                if (wp.flags & SWP_NOMOVE).0 == 0 {
                    MANAGER.with(|m| {
                        if let Ok(mut mgr) = m.try_borrow_mut() {
                            if let Some(pin) = mgr.pins.get_mut(&(hwnd.0 as isize)) {
                                pin.x = wp.x;
                                pin.y = wp.y;
                            }
                        }
                    });
                }
            }
        }
        WM_LBUTTONUP => {
            let was_dragging = MANAGER.with(|m| {
                if let Some(pin) = m.borrow_mut().pins.get_mut(&(hwnd.0 as isize)) {
                    if pin.dragging {
                        pin.dragging = false;
                        return true;
                    }
                }
                false
            });
            if was_dragging {
                unsafe {
                    let _ = ReleaseCapture();
                }
            }
            return LRESULT(0);
        }
        WM_MOUSELEAVE => {
            let need_repaint = MANAGER.with(|m| {
                if let Some(pin) = m.borrow_mut().pins.get_mut(&(hwnd.0 as isize)) {
                    pin.tracking_leave = false;
                    if pin.hovered {
                        pin.hovered = false;
                        return true;
                    }
                }
                false
            });
            if need_repaint {
                unsafe {
                    let _ = InvalidateRect(Some(hwnd), None, false);
                }
            }
            return LRESULT(0);
        }
        WM_LBUTTONDBLCLK => {
            let action = MANAGER.with(|m| m.borrow().config.pin_double_click);
            match action {
                PinDoubleClickAction::Hide => hide_pin_window(hwnd),
                PinDoubleClickAction::ResetZoom => reset_pin_view(hwnd),
                PinDoubleClickAction::Close => unsafe {
                    let _ = DestroyWindow(hwnd);
                },
            }
            return LRESULT(0);
        }
        WM_RBUTTONUP => {
            show_context_menu(hwnd);
            return LRESULT(0);
        }
        WM_MOUSEWHEEL => {
            let delta = ((wparam.0 >> 16) as u16 as i16) as i32;
            let ctrl = key_down(VK_CONTROL.0) || key_down(VK_MENU.0);
            let mut pt = POINT::default();
            unsafe {
                let _ = GetCursorPos(&mut pt);
            }
            if ctrl {
                let next = MANAGER.with(|m| {
                    m.borrow().pins.get(&(hwnd.0 as isize)).map(|pin| {
                        let step: i32 = if delta > 0 { 20 } else { -20 };
                        (pin.opacity as i32 + step).clamp(38, 255) as u8
                    })
                });
                if let Some(next) = next {
                    set_pin_opacity(hwnd, next);
                }
            } else {
                let factor = if delta > 0 { 1.1 } else { 1.0 / 1.1 };
                zoom_pin_by(hwnd, factor, Some((pt.x, pt.y)));
            }
            return LRESULT(0);
        }
        0x010D /* WM_IME_STARTCOMPOSITION */ => return LRESULT(0),
        WM_KEYDOWN => {
            let vk = resolve_vk(wparam, lparam);
            let ctrl = key_down(VK_CONTROL.0);
            let shift = key_down(VK_SHIFT.0);
            if vk == VK_ESCAPE.0 {
                unsafe {
                    let _ = DestroyWindow(hwnd);
                }
                return LRESULT(0);
            }
            if ctrl && vk == b'C' as u16 {
                copy_pin_image(hwnd);
                return LRESULT(0);
            }
            if ctrl && vk == b'S' as u16 {
                save_pin_image(hwnd);
                return LRESULT(0);
            }
            if vk == b'H' as u16 {
                hide_pin_window(hwnd);
                return LRESULT(0);
            }
            if vk == b'0' as u16 {
                reset_pin_view(hwnd);
                return LRESULT(0);
            }
            if vk == b'R' as u16 {
                rotate_pin_90(hwnd);
                return LRESULT(0);
            }
            if vk == VK_OEM_PLUS.0 {
                zoom_pin_by(hwnd, 1.1, None);
                return LRESULT(0);
            }
            if vk == VK_OEM_MINUS.0 {
                zoom_pin_by(hwnd, 1.0 / 1.1, None);
                return LRESULT(0);
            }
            if matches!(vk, x if x == VK_LEFT.0 || x == VK_RIGHT.0 || x == VK_UP.0 || x == VK_DOWN.0) {
                let step = if shift { 10 } else { 1 };
                let move_to = MANAGER.with(|m| {
                    let mut mgr = m.borrow_mut();
                    let p = mgr.pins.get_mut(&(hwnd.0 as isize))?;
                    if vk == VK_LEFT.0 { p.x -= step; }
                    if vk == VK_RIGHT.0 { p.x += step; }
                    if vk == VK_UP.0 { p.y -= step; }
                    if vk == VK_DOWN.0 { p.y += step; }
                    Some((p.x, p.y))
                });
                if let Some((nx, ny)) = move_to {
                    unsafe {
                        let _ = SetWindowPos(
                            hwnd,
                            None,
                            nx,
                            ny,
                            0,
                            0,
                            SWP_NOSIZE | SWP_NOZORDER | SWP_NOACTIVATE,
                        );
                    }
                }
                return LRESULT(0);
            }
        }
        WM_TIMER => {
            if wparam.0 == TIMER_HUD {
                unsafe {
                    let _ = KillTimer(Some(hwnd), TIMER_HUD);
                }
                MANAGER.with(|m| {
                    if let Some(pin) = m.borrow_mut().pins.get_mut(&(hwnd.0 as isize)) {
                        pin.hud_text = None;
                        unsafe {
                            let _ = InvalidateRect(Some(hwnd), None, false);
                        }
                    }
                });
                return LRESULT(0);
            }
        }
        WM_DESTROY => {
            unsafe {
                let _ = KillTimer(Some(hwnd), TIMER_HUD);
            }
        }
        WM_NCDESTROY => {
            MANAGER.with(|m| {
                m.borrow_mut().pins.remove(&(hwnd.0 as isize));
            });
            trim_working_set();
        }
        _ => {}
    }
    unsafe { DefWindowProcW(hwnd, msg, wparam, lparam) }
}
