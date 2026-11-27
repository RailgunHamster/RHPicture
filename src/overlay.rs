use crate::capture::{MonitorInfo, RectI, ScreenCapture, capture_virtual_screen};
use crate::image_ops::BgraImage;
use crate::util::{
    DibSurface, OwnedFont, create_ui_font, current_hinstance, disable_ime, resolve_vk,
    rgb_colorref, scale_dpi, to_wide, trim_working_set,
};
use std::cell::RefCell;
use std::collections::HashMap;
use std::sync::Once;
use tiny_skia::{
    Color, FillRule, GradientStop, LineCap, LineJoin, LinearGradient, Paint, PathBuilder, Pixmap,
    Point, SpreadMode, Stroke, Transform,
};
use windows::Win32::Foundation::{HWND, LPARAM, LRESULT, RECT, WPARAM};
use windows::Win32::Graphics::Direct2D::Common::{
    D2D1_ALPHA_MODE_IGNORE, D2D1_ALPHA_MODE_PREMULTIPLIED, D2D1_COLOR_F, D2D1_PIXEL_FORMAT,
    D2D_RECT_F, D2D_SIZE_U,
};
use windows::Win32::Graphics::Direct2D::{
    D2D1CreateFactory, D2D1_BITMAP_INTERPOLATION_MODE_NEAREST_NEIGHBOR, D2D1_BITMAP_PROPERTIES,
    D2D1_ELLIPSE, D2D1_FACTORY_TYPE_SINGLE_THREADED, D2D1_HWND_RENDER_TARGET_PROPERTIES,
    D2D1_PRESENT_OPTIONS_IMMEDIATELY, D2D1_RENDER_TARGET_PROPERTIES,
    D2D1_RENDER_TARGET_TYPE_DEFAULT, ID2D1Bitmap, ID2D1Factory, ID2D1HwndRenderTarget,
    ID2D1SolidColorBrush,
};
use windows::Win32::Graphics::Dxgi::Common::DXGI_FORMAT_B8G8R8A8_UNORM;
use windows::Win32::Graphics::Gdi::{
    BeginPaint, BitBlt, DT_CENTER, DT_LEFT, DT_NOCLIP, DT_SINGLELINE, DT_VCENTER, DrawTextW,
    EndPaint, HDC, InvalidateRect, PAINTSTRUCT, SRCCOPY, SelectObject, SetBkMode, SetTextColor,
    TRANSPARENT,
};
use windows::Win32::UI::Input::KeyboardAndMouse::{
    GetAsyncKeyState, ReleaseCapture, SetCapture, SetFocus, VK_CONTROL, VK_ESCAPE, VK_F3,
    VK_LEFT, VK_MENU, VK_RETURN, VK_RIGHT, VK_SHIFT, VK_UP, VK_DOWN,
};
use windows::Win32::UI::WindowsAndMessaging::{
    CS_DBLCLKS, CreateWindowExW, DefWindowProcW, DestroyWindow, GetCursorPos, IDC_ARROW,
    IDC_CROSS, IDC_HAND, IDC_SIZEALL, IDC_SIZENESW, IDC_SIZENS, IDC_SIZENWSE, IDC_SIZEWE,
    LoadCursorW, RegisterClassExW, SW_SHOW, SetCursor, SetForegroundWindow, SetWindowPos,
    ShowWindow, WM_DESTROY, WM_ERASEBKGND, WM_KEYDOWN, WM_LBUTTONDBLCLK, WM_LBUTTONDOWN,
    WM_LBUTTONUP, WM_MOUSEMOVE, WM_NCDESTROY, WM_PAINT, WM_RBUTTONUP, WM_SETCURSOR,
    WNDCLASSEXW, WS_EX_TOOLWINDOW, WS_EX_TOPMOST, WS_POPUP, HWND_TOPMOST,
    SWP_SHOWWINDOW,
};
use windows::core::PCWSTR;

const OVERLAY_CLASS: &str = "RHPictureOverlayClass";

pub enum OverlayOutcome {
    Copy(BgraImage),
    Save(BgraImage),
    Pin {
        image: BgraImage,
        screen_x: i32,
        screen_y: i32,
    },
    CopyColor(String),
    Cancel,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum DragMode {
    None,
    Creating,
    Moving,
    ResizeN,
    ResizeS,
    ResizeW,
    ResizeE,
    ResizeNW,
    ResizeNE,
    ResizeSW,
    ResizeSE,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum ToolbarAction {
    Pin,
    Save,
    Copy,
    Cancel,
}

struct ToolbarButton {
    action: ToolbarAction,
    rect: RectI,
    label: &'static str,
}

struct CachedGpuBitmap<K: PartialEq> {
    key: K,
    bmp: ID2D1Bitmap,
    width: i32,
    height: i32,
}

/// Hardware-accelerated Direct2D renderer active ONLY while the screenshot overlay is open.
/// Dropped immediately on overlay exit so idle/pinned VRAM stays at 0 MB.
struct GpuOverlay {
    _factory: ID2D1Factory,
    rt: ID2D1HwndRenderTarget,
    src_bitmap: ID2D1Bitmap,
    dim_brush: ID2D1SolidColorBrush,
    halo_brush: ID2D1SolidColorBrush,
    border_brush: ID2D1SolidColorBrush,
    corner_brush: ID2D1SolidColorBrush,
    white_brush: ID2D1SolidColorBrush,
    handle_ring_brush: ID2D1SolidColorBrush,
    handle_shadow_brush: ID2D1SolidColorBrush,

    top_hint_cache: Option<CachedGpuBitmap<(u32, bool)>>,
    size_badge_cache: Option<CachedGpuBitmap<(i32, i32, u32, bool)>>,
    toolbar_cache: Option<CachedGpuBitmap<(Option<ToolbarAction>, u32, bool)>>,
    magnifier_cache: Option<CachedGpuBitmap<(i32, i32, u32, bool)>>,
}

impl GpuOverlay {
    fn new(hwnd: HWND, image: &BgraImage) -> Option<Self> {
        // D3D11 max 2D texture dimension is 16384; fall back to CPU on extreme multi-display walls
        if image.width == 0 || image.height == 0 || image.width > 16384 || image.height > 16384 {
            return None;
        }
        let factory: ID2D1Factory =
            unsafe { D2D1CreateFactory(D2D1_FACTORY_TYPE_SINGLE_THREADED, None).ok()? };
        let rt_props = D2D1_RENDER_TARGET_PROPERTIES {
            r#type: D2D1_RENDER_TARGET_TYPE_DEFAULT,
            pixelFormat: D2D1_PIXEL_FORMAT {
                format: DXGI_FORMAT_B8G8R8A8_UNORM,
                alphaMode: D2D1_ALPHA_MODE_IGNORE,
            },
            dpiX: 96.0,
            dpiY: 96.0,
            ..Default::default()
        };
        let hwnd_props = D2D1_HWND_RENDER_TARGET_PROPERTIES {
            hwnd,
            pixelSize: D2D_SIZE_U {
                width: image.width,
                height: image.height,
            },
            presentOptions: D2D1_PRESENT_OPTIONS_IMMEDIATELY,
        };
        let rt = unsafe { factory.CreateHwndRenderTarget(&rt_props, &hwnd_props).ok()? };
        unsafe {
            rt.SetDpi(96.0, 96.0);
        }

        let bmp_props = D2D1_BITMAP_PROPERTIES {
            pixelFormat: D2D1_PIXEL_FORMAT {
                format: DXGI_FORMAT_B8G8R8A8_UNORM,
                alphaMode: D2D1_ALPHA_MODE_IGNORE,
            },
            dpiX: 96.0,
            dpiY: 96.0,
        };
        let src_bitmap = unsafe {
            rt.CreateBitmap(
                D2D_SIZE_U {
                    width: image.width,
                    height: image.height,
                },
                Some(image.data.as_ptr() as *const _),
                image.width * 4,
                &bmp_props,
            )
            .ok()?
        };

        let make_brush = |r: f32, g: f32, b: f32, a: f32| -> Option<ID2D1SolidColorBrush> {
            unsafe {
                rt.CreateSolidColorBrush(&D2D1_COLOR_F { r, g, b, a }, None)
                    .ok()
            }
        };

        Some(Self {
            _factory: factory,
            dim_brush: make_brush(0.0, 0.0, 0.0, 0.52)?,
            halo_brush: make_brush(15.0 / 255.0, 23.0 / 255.0, 42.0 / 255.0, 0.72)?,
            border_brush: make_brush(56.0 / 255.0, 189.0 / 255.0, 248.0 / 255.0, 1.0)?,
            corner_brush: make_brush(125.0 / 255.0, 211.0 / 255.0, 252.0 / 255.0, 1.0)?,
            white_brush: make_brush(1.0, 1.0, 1.0, 1.0)?,
            handle_ring_brush: make_brush(14.0 / 255.0, 165.0 / 255.0, 233.0 / 255.0, 1.0)?,
            handle_shadow_brush: make_brush(0.0, 0.0, 0.0, 0.30)?,
            rt,
            src_bitmap,
            top_hint_cache: None,
            size_badge_cache: None,
            toolbar_cache: None,
            magnifier_cache: None,
        })
    }

    fn upload_widget_bitmap(
        rt: &ID2D1HwndRenderTarget,
        pm: &Pixmap,
        draw_gdi: impl FnOnce(&mut DibSurface),
    ) -> Option<ID2D1Bitmap> {
        let w = pm.width() as i32;
        let h = pm.height() as i32;
        if w <= 0 || h <= 0 {
            return None;
        }
        let mut surface = DibSurface::new(w, h)?;
        let src = pm.data(); // premultiplied RGBA
        {
            let dst = surface.pixels_mut();
            for (spx, dpx) in src.chunks_exact(4).zip(dst.chunks_exact_mut(4)) {
                dpx[0] = spx[2];
                dpx[1] = spx[1];
                dpx[2] = spx[0];
                dpx[3] = spx[3];
            }
        }
        draw_gdi(&mut surface);
        // Restore premultiplied alpha cleanly: GDI ClearType zeros alpha on touched pixels inside the card.
        {
            let dst = surface.pixels_mut();
            for (spx, dpx) in src.chunks_exact(4).zip(dst.chunks_exact_mut(4)) {
                let orig_a = spx[3];
                if orig_a >= 210 && dpx[3] == 0 {
                    dpx[3] = 255;
                } else {
                    dpx[3] = orig_a;
                    // Clamp premultiplied channels to alpha
                    if dpx[0] > orig_a {
                        dpx[0] = orig_a;
                    }
                    if dpx[1] > orig_a {
                        dpx[1] = orig_a;
                    }
                    if dpx[2] > orig_a {
                        dpx[2] = orig_a;
                    }
                }
            }
        }
        let bmp_props = D2D1_BITMAP_PROPERTIES {
            pixelFormat: D2D1_PIXEL_FORMAT {
                format: DXGI_FORMAT_B8G8R8A8_UNORM,
                alphaMode: D2D1_ALPHA_MODE_PREMULTIPLIED,
            },
            dpiX: 96.0,
            dpiY: 96.0,
        };
        unsafe {
            rt.CreateBitmap(
                D2D_SIZE_U {
                    width: w as u32,
                    height: h as u32,
                },
                Some(surface.pixels_mut().as_ptr() as *const _),
                (w as u32) * 4,
                &bmp_props,
            )
            .ok()
        }
    }
}

struct OverlayState {
    hwnd: HWND,
    capture: ScreenCapture,
    gpu: Option<GpuOverlay>,
    /// Lazy CPU fallback buffers (only allocated if Direct2D GPU init fails or in unit tests).
    surface: Option<DibSurface>,
    dimmed_bg: Option<Vec<u8>>,
    /// Per-DPI cached (ui_10pt, small_9pt) ClearType fonts so mixed-DPI monitors render sharply.
    fonts: HashMap<u32, (OwnedFont, OwnedFont)>,
    show_magnifier: bool,
    smart_window_detect: bool,
    dark_theme: bool,
    dirty: bool,

    selection: Option<RectI>,
    hover_rect: Option<RectI>,
    drag_mode: DragMode,
    drag_start: (i32, i32),
    drag_orig_sel: RectI,
    cursor_pos: (i32, i32),

    toolbar_rect: RectI,
    toolbar_dpi: u32,
    buttons: Vec<ToolbarButton>,
    hovered_button: Option<ToolbarAction>,

    on_finish: Option<Box<dyn FnOnce(OverlayOutcome)>>,
    finished: bool,
}

thread_local! {
    static ACTIVE_OVERLAY: RefCell<Option<Box<OverlayState>>> = const { RefCell::new(None) };
}

static REGISTER_CLASS: Once = Once::new();

fn ensure_class_registered() {
    REGISTER_CLASS.call_once(|| unsafe {
        let class_w = to_wide(OVERLAY_CLASS);
        let wc = WNDCLASSEXW {
            cbSize: std::mem::size_of::<WNDCLASSEXW>() as u32,
            style: CS_DBLCLKS,
            lpfnWndProc: Some(overlay_wnd_proc),
            hInstance: current_hinstance(),
            hCursor: LoadCursorW(None, IDC_CROSS).unwrap_or_default(),
            lpszClassName: PCWSTR(class_w.as_ptr()),
            ..Default::default()
        };
        let _ = RegisterClassExW(&wc);
    });
}

pub fn is_overlay_active() -> bool {
    ACTIVE_OVERLAY.with(|cell| cell.borrow().is_some())
}

pub fn start_screenshot_overlay(
    smart_window_detect: bool,
    show_magnifier: bool,
    dark_theme: bool,
    on_finish: impl FnOnce(OverlayOutcome) + 'static,
) -> Result<(), String> {
    if is_overlay_active() {
        return Ok(());
    }
    ensure_class_registered();

    let capture = capture_virtual_screen(smart_window_detect)?;
    let width = capture.image.width as i32;
    let height = capture.image.height as i32;
    let origin_x = capture.origin_x;
    let origin_y = capture.origin_y;

    let mut fonts = HashMap::new();
    for m in &capture.monitors {
        fonts.entry(m.dpi).or_insert_with(|| {
            (
                OwnedFont(create_ui_font(m.dpi, 10, false)),
                OwnedFont(create_ui_font(m.dpi, 9, false)),
            )
        });
    }
    fonts.entry(96).or_insert_with(|| {
        (
            OwnedFont(create_ui_font(96, 10, false)),
            OwnedFont(create_ui_font(96, 9, false)),
        )
    });

    let mut pt = windows::Win32::Foundation::POINT::default();
    unsafe {
        let _ = GetCursorPos(&mut pt);
    }
    let cursor_local = (
        (pt.x - origin_x).clamp(0, width - 1),
        (pt.y - origin_y).clamp(0, height - 1),
    );

    let mut state = Box::new(OverlayState {
        hwnd: HWND::default(),
        capture,
        gpu: None,
        surface: None,
        dimmed_bg: None,
        fonts,
        show_magnifier,
        smart_window_detect,
        dark_theme,
        dirty: true,
        selection: None,
        hover_rect: None,
        drag_mode: DragMode::None,
        drag_start: cursor_local,
        drag_orig_sel: RectI::default(),
        cursor_pos: cursor_local,
        toolbar_rect: RectI::default(),
        toolbar_dpi: 96,
        buttons: Vec::new(),
        hovered_button: None,
        on_finish: Some(Box::new(on_finish)),
        finished: false,
    });
    state.update_hover_rect();

    ACTIVE_OVERLAY.with(|cell| {
        *cell.borrow_mut() = Some(state);
    });

    let class_w = to_wide(OVERLAY_CLASS);
    let title_w = to_wide("RHPicture Capture");
    let hwnd = unsafe {
        CreateWindowExW(
            WS_EX_TOPMOST | WS_EX_TOOLWINDOW,
            PCWSTR(class_w.as_ptr()),
            PCWSTR(title_w.as_ptr()),
            WS_POPUP,
            origin_x,
            origin_y,
            width,
            height,
            None,
            None,
            Some(current_hinstance()),
            None,
        )
    };

    let Ok(hwnd) = hwnd else {
        ACTIVE_OVERLAY.with(|cell| {
            *cell.borrow_mut() = None;
        });
        return Err("创建截图遮罩窗口失败".into());
    };

    disable_ime(hwnd);

    ACTIVE_OVERLAY.with(|cell| {
        if let Some(s) = cell.borrow_mut().as_mut() {
            s.hwnd = hwnd;
            s.gpu = GpuOverlay::new(hwnd, &s.capture.image);
            if s.gpu.is_none() {
                s.ensure_cpu_buffers();
            }
        }
    });

    unsafe {
        let _ = SetWindowPos(
            hwnd,
            Some(HWND_TOPMOST),
            origin_x,
            origin_y,
            width,
            height,
            SWP_SHOWWINDOW,
        );
        let _ = ShowWindow(hwnd, SW_SHOW);
        let _ = SetForegroundWindow(hwnd);
        let _ = SetFocus(Some(hwnd));
        let _ = InvalidateRect(Some(hwnd), None, false);
    }

    Ok(())
}

impl OverlayState {
    fn bounds(&self) -> RectI {
        RectI::new(
            0,
            0,
            self.capture.image.width as i32,
            self.capture.image.height as i32,
        )
    }

    fn monitor_info_for_point(&self, x: i32, y: i32) -> MonitorInfo {
        self.capture
            .monitors
            .iter()
            .copied()
            .find(|m| m.rect.contains(x, y))
            .unwrap_or_else(|| MonitorInfo {
                rect: self.bounds(),
                dpi: 96,
            })
    }

    fn monitor_for_point(&self, x: i32, y: i32) -> RectI {
        self.monitor_info_for_point(x, y).rect
    }

    fn font_for_dpi(&self, dpi: u32, small: bool) -> windows::Win32::Graphics::Gdi::HFONT {
        let pair = self
            .fonts
            .get(&dpi)
            .or_else(|| self.fonts.get(&96))
            .expect("96 DPI fallback font always exists");
        if small { pair.1.0 } else { pair.0.0 }
    }

    fn update_hover_rect(&mut self) {
        if !self.smart_window_detect || self.selection.is_some() {
            self.hover_rect = None;
            return;
        }
        let (x, y) = self.cursor_pos;
        if let Some(&r) = self
            .capture
            .window_rects
            .iter()
            .find(|r| r.contains(x, y))
        {
            self.hover_rect = Some(r);
        } else {
            self.hover_rect = Some(self.monitor_for_point(x, y));
        }
    }

    fn hit_test_selection(&self, x: i32, y: i32) -> DragMode {
        let Some(sel) = self.selection else {
            return DragMode::None;
        };
        let dpi = self.monitor_info_for_point(x, y).dpi;
        let tol = scale_dpi(8, dpi).max(7);
        let near_l = (x - sel.left).abs() <= tol && y >= sel.top - tol && y <= sel.bottom + tol;
        let near_r = (x - sel.right).abs() <= tol && y >= sel.top - tol && y <= sel.bottom + tol;
        let near_t = (y - sel.top).abs() <= tol && x >= sel.left - tol && x <= sel.right + tol;
        let near_b = (y - sel.bottom).abs() <= tol && x >= sel.left - tol && x <= sel.right + tol;

        match (near_l, near_r, near_t, near_b) {
            (true, _, true, _) => DragMode::ResizeNW,
            (_, true, true, _) => DragMode::ResizeNE,
            (true, _, _, true) => DragMode::ResizeSW,
            (_, true, _, true) => DragMode::ResizeSE,
            (_, _, true, _) => DragMode::ResizeN,
            (_, _, _, true) => DragMode::ResizeS,
            (true, _, _, _) => DragMode::ResizeW,
            (_, true, _, _) => DragMode::ResizeE,
            _ if sel.contains(x, y) => DragMode::Moving,
            _ => DragMode::None,
        }
    }

    fn layout_toolbar(&mut self) {
        self.buttons.clear();
        self.toolbar_rect = RectI::default();
        let Some(sel) = self.selection else {
            return;
        };
        if self.drag_mode != DragMode::None || sel.width() < 2 || sel.height() < 2 {
            return;
        }

        let mon_info =
            self.monitor_info_for_point((sel.left + sel.right) / 2, (sel.top + sel.bottom) / 2);
        let mon = mon_info.rect;
        let dpi = mon_info.dpi;
        self.toolbar_dpi = dpi;
        let s = |v: i32| scale_dpi(v, dpi);

        let btn_w = s(80);
        let btn_h = s(36);
        let gap = s(5);
        let sep_extra = s(10); // subtle vertical divider before Cancel
        let pad_x = s(8);
        let pad_y = s(7);
        let items = [
            (ToolbarAction::Pin, "贴图"),
            (ToolbarAction::Save, "保存"),
            (ToolbarAction::Copy, "复制"),
            (ToolbarAction::Cancel, "取消"),
        ];
        let total_w =
            pad_x * 2 + (items.len() as i32) * btn_w + ((items.len() as i32) - 1) * gap + sep_extra;
        let total_h = pad_y * 2 + btn_h;

        let margin = s(12);

        let mut tx = sel.right - total_w;
        if tx < mon.left + margin {
            tx = (sel.left).max(mon.left + margin);
        }
        if tx + total_w > mon.right - margin {
            tx = (mon.right - margin - total_w).max(mon.left + margin);
        }

        let mut ty = sel.bottom + margin;
        if ty + total_h > mon.bottom - margin {
            if sel.top - margin - total_h >= mon.top + margin {
                ty = sel.top - margin - total_h;
            } else {
                ty = (sel.bottom - margin - total_h).max(mon.top + margin);
            }
        }

        self.toolbar_rect = RectI::new(tx, ty, tx + total_w, ty + total_h);
        let mut bx = tx + pad_x;
        for (i, (action, label)) in items.iter().enumerate() {
            if i == 3 {
                bx += sep_extra;
            }
            let by = ty + pad_y;
            self.buttons.push(ToolbarButton {
                action: *action,
                rect: RectI::new(bx, by, bx + btn_w, by + btn_h),
                label,
            });
            bx += btn_w + gap;
        }
    }

    fn button_at(&self, x: i32, y: i32) -> Option<ToolbarAction> {
        self.buttons
            .iter()
            .find(|b| b.rect.contains(x, y))
            .map(|b| b.action)
    }

    fn ensure_cpu_buffers(&mut self) -> bool {
        let w = self.capture.image.width as i32;
        let h = self.capture.image.height as i32;
        if self.surface.is_none() {
            self.surface = DibSurface::new(w, h);
        }
        if self.dimmed_bg.is_none() {
            let src = &self.capture.image.data;
            let mut dim = vec![0u8; src.len()];
            for (s, d) in src.chunks_exact(4).zip(dim.chunks_exact_mut(4)) {
                d[0] = ((s[0] as u16 * 124) >> 8) as u8;
                d[1] = ((s[1] as u16 * 124) >> 8) as u8;
                d[2] = ((s[2] as u16 * 124) >> 8) as u8;
                d[3] = 255;
            }
            self.dimmed_bg = Some(dim);
        }
        self.surface.is_some()
    }

    fn render_cpu_and_blit(&mut self, hdc: HDC, paint_rect: RECT) {
        if !self.ensure_cpu_buffers() {
            return;
        }
        if self.dirty {
            self.render();
            self.dirty = false;
        }
        if let Some(ref surface) = self.surface {
            let x = paint_rect.left.clamp(0, surface.width);
            let y = paint_rect.top.clamp(0, surface.height);
            let r = paint_rect.right.clamp(x, surface.width);
            let b = paint_rect.bottom.clamp(y, surface.height);
            let bw = (r - x).max(0);
            let bh = (b - y).max(0);
            if bw > 0 && bh > 0 {
                unsafe {
                    let _ = BitBlt(hdc, x, y, bw, bh, Some(surface.dc), x, y, SRCCOPY);
                }
            }
        }
    }

    fn render_gpu(&mut self) -> bool {
        self.layout_toolbar();
        if self.gpu.is_none() {
            return false;
        }

        let bounds = self.bounds();
        let w = bounds.width() as f32;
        let h = bounds.height() as f32;
        let active_rect = self.selection.or(self.hover_rect).map(|r| r.intersect(bounds));
        let is_confirmed = self.selection.is_some();
        let show_top_hint = self.selection.is_none() && self.drag_mode == DragMode::None;
        let show_loupe = self.show_magnifier
            && bounds.width() > 20
            && bounds.height() > 20
            && (self.selection.is_none()
                || matches!(
                    self.drag_mode,
                    DragMode::Creating
                        | DragMode::ResizeN
                        | DragMode::ResizeS
                        | DragMode::ResizeW
                        | DragMode::ResizeE
                        | DragMode::ResizeNW
                        | DragMode::ResizeNE
                        | DragMode::ResizeSW
                        | DragMode::ResizeSE
                ));

        // Pre-compute widget placements & ensure cached GPU bitmaps BEFORE BeginDraw
        let top_hint_pos = if show_top_hint {
            let mon_info = self.monitor_info_for_point(self.cursor_pos.0, self.cursor_pos.1);
            let dpi = mon_info.dpi;
            let s = |v: i32| scale_dpi(v, dpi);
            let pill_w = s(370).min(mon_info.rect.width() - s(24));
            let pill_h = s(30);
            let shadow = s(10);
            let px = mon_info.rect.left + (mon_info.rect.width() - pill_w) / 2;
            let py = mon_info.rect.top + s(18);
            self.ensure_gpu_top_hint(dpi, pill_w, pill_h, shadow);
            Some((px - shadow, py - shadow))
        } else {
            None
        };

        let size_badge_pos = if let Some(r) = active_rect.filter(|r| !r.is_empty()) {
            let mon_info = self.monitor_info_for_point(r.left, r.top);
            let mon = mon_info.rect;
            let dpi = mon_info.dpi;
            let s = |v: i32| scale_dpi(v, dpi);
            let text = format!("{} × {}", r.width(), r.height());
            let badge_w = s(106).max(s((text.chars().count() as i32) * 8 + 28));
            let badge_h = s(28);
            let shadow = s(8);
            let pad_edge = s(6);
            let gap = s(8);
            let bx = r
                .left
                .clamp(mon.left + pad_edge, (mon.right - badge_w - pad_edge).max(mon.left + pad_edge));
            let by = if r.top - badge_h - gap >= mon.top + pad_edge {
                r.top - badge_h - gap
            } else {
                r.top + gap
            };
            self.ensure_gpu_size_badge(r.width(), r.height(), dpi, badge_w, badge_h, shadow);
            Some((bx - shadow, by - shadow))
        } else {
            None
        };

        let toolbar_pos = if !self.toolbar_rect.is_empty() {
            let dpi = self.toolbar_dpi;
            let shadow = scale_dpi(12, dpi);
            self.ensure_gpu_toolbar(dpi, shadow);
            Some((self.toolbar_rect.left - shadow, self.toolbar_rect.top - shadow))
        } else {
            None
        };

        let magnifier_pos = if show_loupe {
            let (cx, cy) = self.cursor_pos;
            let mon_info = self.monitor_info_for_point(cx, cy);
            let mon = mon_info.rect;
            let dpi = mon_info.dpi;
            let s = |v: i32| scale_dpi(v, dpi);
            let zoom = s(9).max(8);
            let view_w = 17 * zoom;
            let view_h = 11 * zoom;
            let pad = s(8);
            let info_h = s(52);
            let total_w = view_w.max(s(176)) + pad * 2;
            let total_h = view_h + info_h + pad * 2;
            let shadow = s(12);
            let offset = s(22);
            let edge_pad = s(10);
            let mut mx = cx + offset;
            let mut my = cy + offset;
            if mx + total_w > mon.right - edge_pad {
                mx = cx - offset - total_w;
            }
            if my + total_h > mon.bottom - edge_pad {
                my = cy - offset - total_h;
            }
            mx = mx.clamp(mon.left + s(6), (mon.right - total_w - s(6)).max(mon.left + s(6)));
            my = my.clamp(mon.top + s(6), (mon.bottom - total_h - s(6)).max(mon.top + s(6)));
            self.ensure_gpu_magnifier(cx, cy, dpi, zoom, pad, info_h, total_w, total_h, shadow);
            Some((mx - shadow, my - shadow))
        } else {
            None
        };

        let active_dpi = active_rect
            .filter(|r| !r.is_empty())
            .map(|r| {
                self.monitor_info_for_point((r.left + r.right) / 2, (r.top + r.bottom) / 2)
                    .dpi
            })
            .unwrap_or(96);

        let end_res = {
            let Some(gpu) = self.gpu.as_ref() else {
                return false;
            };
            let rt = &gpu.rt;
            unsafe {
                rt.BeginDraw();

                // 1. Full desktop bitmap on GPU (~0.1 ms)
                let full_rect = D2D_RECT_F {
                    left: 0.0,
                    top: 0.0,
                    right: w,
                    bottom: h,
                };
                rt.DrawBitmap(
                    &gpu.src_bitmap,
                    Some(&full_rect),
                    1.0,
                    D2D1_BITMAP_INTERPOLATION_MODE_NEAREST_NEIGHBOR,
                    None,
                );

                // 2. Dimmed mask strips around active selection/hover rect on GPU (~0.05 ms)
                if let Some(r) = active_rect.filter(|r| !r.is_empty()) {
                    let rl = r.left as f32;
                    let rt_y = r.top as f32;
                    let rr = r.right as f32;
                    let rb = r.bottom as f32;
                    if rt_y > 0.0 {
                        rt.FillRectangle(
                            &D2D_RECT_F { left: 0.0, top: 0.0, right: w, bottom: rt_y },
                            &gpu.dim_brush,
                        );
                    }
                    if rb < h {
                        rt.FillRectangle(
                            &D2D_RECT_F { left: 0.0, top: rb, right: w, bottom: h },
                            &gpu.dim_brush,
                        );
                    }
                    if rl > 0.0 {
                        rt.FillRectangle(
                            &D2D_RECT_F { left: 0.0, top: rt_y, right: rl, bottom: rb },
                            &gpu.dim_brush,
                        );
                    }
                    if rr < w {
                        rt.FillRectangle(
                            &D2D_RECT_F { left: rr, top: rt_y, right: w, bottom: rb },
                            &gpu.dim_brush,
                        );
                    }

                    // 3. Outer dark contrast halo + crisp sky-cyan border
                    let s = |v: i32| scale_dpi(v, active_dpi) as f32;
                    let sf = active_dpi as f32 / 96.0;
                    let halo = s(1).max(1.0);
                    let border_w = s(2).max(2.0);

                    rt.DrawRectangle(
                        &D2D_RECT_F {
                            left: rl - halo * 0.5,
                            top: rt_y - halo * 0.5,
                            right: rr + halo * 0.5,
                            bottom: rb + halo * 0.5,
                        },
                        &gpu.halo_brush,
                        halo,
                        None,
                    );
                    rt.DrawRectangle(
                        &D2D_RECT_F {
                            left: rl + border_w * 0.5,
                            top: rt_y + border_w * 0.5,
                            right: rr - border_w * 0.5,
                            bottom: rb - border_w * 0.5,
                        },
                        &gpu.border_brush,
                        border_w,
                        None,
                    );

                    if !is_confirmed {
                        // Modern viewfinder L-corners for hover window highlight
                        let arm = s(16)
                            .min((r.width() / 3) as f32)
                            .min((r.height() / 3) as f32)
                            .max(4.0);
                        let bt = s(3).max(3.0);
                        let corners = [
                            D2D_RECT_F { left: rl, top: rt_y, right: rl + arm, bottom: rt_y + bt },
                            D2D_RECT_F { left: rl, top: rt_y, right: rl + bt, bottom: rt_y + arm },
                            D2D_RECT_F { left: rr - arm, top: rt_y, right: rr, bottom: rt_y + bt },
                            D2D_RECT_F { left: rr - bt, top: rt_y, right: rr, bottom: rt_y + arm },
                            D2D_RECT_F { left: rl, top: rb - bt, right: rl + arm, bottom: rb },
                            D2D_RECT_F { left: rl, top: rb - arm, right: rl + bt, bottom: rb },
                            D2D_RECT_F { left: rr - arm, top: rb - bt, right: rr, bottom: rb },
                            D2D_RECT_F { left: rr - bt, top: rb - arm, right: rr, bottom: rb },
                        ];
                        for c in &corners {
                            rt.FillRectangle(c, &gpu.corner_brush);
                        }
                    } else if self.drag_mode == DragMode::None {
                        // Anti-aliased floating circular resize handles directly on GPU
                        let mx = (rl + rr) * 0.5;
                        let my = (rt_y + rb) * 0.5;
                        let pts = [
                            (rl, rt_y),
                            (mx, rt_y),
                            (rr, rt_y),
                            (rl, my),
                            (rr, my),
                            (rl, rb),
                            (mx, rb),
                            (rr, rb),
                        ];
                        let hr = 4.8 * sf;
                        let sr = 5.6 * sf;
                        for (hx, hy) in pts {
                            let mut sh_ell = D2D1_ELLIPSE {
                                radiusX: sr,
                                radiusY: sr,
                                ..Default::default()
                            };
                            sh_ell.point.X = hx;
                            sh_ell.point.Y = hy + 1.2 * sf;
                            rt.FillEllipse(&sh_ell, &gpu.handle_shadow_brush);

                            let mut ell = D2D1_ELLIPSE {
                                radiusX: hr,
                                radiusY: hr,
                                ..Default::default()
                            };
                            ell.point.X = hx;
                            ell.point.Y = hy;
                            rt.FillEllipse(&ell, &gpu.white_brush);
                            rt.DrawEllipse(&ell, &gpu.handle_ring_brush, 1.8 * sf, None);
                        }
                    }
                } else {
                    rt.FillRectangle(&full_rect, &gpu.dim_brush);
                }

                // 4. Composite cached floating glass widgets on GPU
                let draw_cached = |pos: Option<(i32, i32)>, bmp: &ID2D1Bitmap, bw: i32, bh: i32| {
                    if let Some((x, y)) = pos {
                        let dst = D2D_RECT_F {
                            left: x as f32,
                            top: y as f32,
                            right: (x + bw) as f32,
                            bottom: (y + bh) as f32,
                        };
                        rt.DrawBitmap(
                            bmp,
                            Some(&dst),
                            1.0,
                            D2D1_BITMAP_INTERPOLATION_MODE_NEAREST_NEIGHBOR,
                            None,
                        );
                    }
                };
                if let Some(ref c) = gpu.size_badge_cache {
                    draw_cached(size_badge_pos, &c.bmp, c.width, c.height);
                }
                if let Some(ref c) = gpu.top_hint_cache {
                    draw_cached(top_hint_pos, &c.bmp, c.width, c.height);
                }
                if let Some(ref c) = gpu.toolbar_cache {
                    draw_cached(toolbar_pos, &c.bmp, c.width, c.height);
                }
                if let Some(ref c) = gpu.magnifier_cache {
                    draw_cached(magnifier_pos, &c.bmp, c.width, c.height);
                }

                rt.EndDraw(None, None)
            }
        };

        self.dirty = false;
        if let Err(err) = end_res {
            // D2DERR_RECREATE_TARGET (0x8899000C) -> recreate device resources cleanly
            if err.code().0 as u32 == 0x8899_000C {
                self.gpu = GpuOverlay::new(self.hwnd, &self.capture.image);
                self.dirty = true;
                return false;
            }
        }
        true
    }

    fn draw_text_on_dc(
        &self,
        dc: HDC,
        text: &str,
        rect: RectI,
        color: (u8, u8, u8),
        dpi: u32,
        small: bool,
        centered: bool,
    ) {
        let wide = to_wide(text);
        let mut r = RECT {
            left: rect.left,
            top: rect.top,
            right: rect.right,
            bottom: rect.bottom,
        };
        unsafe {
            let font = self.font_for_dpi(dpi, small);
            let old = SelectObject(dc, windows::Win32::Graphics::Gdi::HGDIOBJ(font.0));
            SetBkMode(dc, TRANSPARENT);
            SetTextColor(dc, rgb_colorref(color.0, color.1, color.2));
            let flags = if centered {
                DT_CENTER | DT_VCENTER | DT_SINGLELINE | DT_NOCLIP
            } else {
                DT_LEFT | DT_VCENTER | DT_SINGLELINE | DT_NOCLIP
            };
            let _ = DrawTextW(
                dc,
                &mut wide[..wide.len() - 1].to_vec(),
                &mut r,
                flags,
            );
            let _ = SelectObject(dc, old);
        }
    }

    fn ensure_gpu_top_hint(&mut self, dpi: u32, pill_w: i32, pill_h: i32, shadow: i32) {
        let key = (dpi, self.dark_theme);
        if self
            .gpu
            .as_ref()
            .and_then(|g| g.top_hint_cache.as_ref())
            .is_some_and(|c| c.key == key)
        {
            return;
        }
        let sf = dpi as f32 / 96.0;
        let tw = pill_w + shadow * 2;
        let th = pill_h + shadow * 2;
        let Some(mut pm) = Pixmap::new(tw as u32, th as u32) else {
            return;
        };
        let (bg, border, txt_col) = if self.dark_theme {
            (
                Color::from_rgba8(15, 23, 42, 226),
                Color::from_rgba8(255, 255, 255, 36),
                (226, 232, 240),
            )
        } else {
            (
                Color::from_rgba8(255, 255, 255, 242),
                Color::from_rgba8(15, 23, 42, 34),
                (15, 23, 42),
            )
        };
        let ox = shadow as f32;
        let oy = shadow as f32;
        draw_soft_shadow(&mut pm, ox, oy, pill_w as f32, pill_h as f32, 15.0 * sf, sf);
        fill_rounded_rect(&mut pm, ox, oy, pill_w as f32, pill_h as f32, 15.0 * sf, bg);
        stroke_rounded_rect(
            &mut pm,
            ox + 0.5 * sf,
            oy + 0.5 * sf,
            pill_w as f32 - sf,
            pill_h as f32 - sf,
            14.5 * sf,
            1.0 * sf,
            border,
        );
        let rt = self.gpu.as_ref().map(|g| g.rt.clone());
        if let Some(rt) = rt {
            if let Some(bmp) = GpuOverlay::upload_widget_bitmap(&rt, &pm, |surf| {
                self.draw_text_on_dc(
                    surf.dc,
                    "单击选中窗口 · 拖拽自由框选 · 按 C 取色 · 右键 / Esc 退出",
                    RectI::new(shadow, shadow, shadow + pill_w, shadow + pill_h),
                    txt_col,
                    dpi,
                    true,
                    true,
                );
            }) {
                if let Some(g) = self.gpu.as_mut() {
                    g.top_hint_cache = Some(CachedGpuBitmap {
                        key,
                        bmp,
                        width: tw,
                        height: th,
                    });
                }
            }
        }
    }

    fn ensure_gpu_size_badge(
        &mut self,
        sel_w: i32,
        sel_h: i32,
        dpi: u32,
        badge_w: i32,
        badge_h: i32,
        shadow: i32,
    ) {
        let key = (sel_w, sel_h, dpi, self.dark_theme);
        if self
            .gpu
            .as_ref()
            .and_then(|g| g.size_badge_cache.as_ref())
            .is_some_and(|c| c.key == key)
        {
            return;
        }
        let s = |v: i32| scale_dpi(v, dpi);
        let sf = dpi as f32 / 96.0;
        let tw = badge_w + shadow * 2;
        let th = badge_h + shadow * 2;
        let Some(mut pm) = Pixmap::new(tw as u32, th as u32) else {
            return;
        };
        let (bg, border, dot_col, txt_col) = if self.dark_theme {
            (
                Color::from_rgba8(15, 23, 42, 234),
                Color::from_rgba8(255, 255, 255, 36),
                Color::from_rgba8(56, 189, 248, 255),
                (248, 250, 252),
            )
        } else {
            (
                Color::from_rgba8(255, 255, 255, 246),
                Color::from_rgba8(15, 23, 42, 34),
                Color::from_rgba8(2, 132, 199, 255),
                (15, 23, 42),
            )
        };
        let ox = shadow as f32;
        let oy = shadow as f32;
        draw_soft_shadow(&mut pm, ox, oy, badge_w as f32, badge_h as f32, 8.0 * sf, sf);
        fill_rounded_rect(&mut pm, ox, oy, badge_w as f32, badge_h as f32, 8.0 * sf, bg);
        stroke_rounded_rect(
            &mut pm,
            ox + 0.5 * sf,
            oy + 0.5 * sf,
            badge_w as f32 - sf,
            badge_h as f32 - sf,
            7.5 * sf,
            1.0 * sf,
            border,
        );
        if let Some(dot) = PathBuilder::from_circle(
            ox + 11.0 * sf,
            oy + (badge_h as f32) * 0.5,
            3.0 * sf,
        ) {
            let mut paint = Paint::default();
            paint.set_color(dot_col);
            paint.anti_alias = true;
            pm.fill_path(&dot, &paint, FillRule::Winding, Transform::identity(), None);
        }
        let text = format!("{sel_w} × {sel_h}");
        let rt = self.gpu.as_ref().map(|g| g.rt.clone());
        if let Some(rt) = rt {
            if let Some(bmp) = GpuOverlay::upload_widget_bitmap(&rt, &pm, |surf| {
                self.draw_text_on_dc(
                    surf.dc,
                    &text,
                    RectI::new(shadow + s(16), shadow, shadow + badge_w - s(6), shadow + badge_h),
                    txt_col,
                    dpi,
                    true,
                    true,
                );
            }) {
                if let Some(g) = self.gpu.as_mut() {
                    g.size_badge_cache = Some(CachedGpuBitmap {
                        key,
                        bmp,
                        width: tw,
                        height: th,
                    });
                }
            }
        }
    }

    fn ensure_gpu_toolbar(&mut self, dpi: u32, shadow: i32) {
        let key = (self.hovered_button, dpi, self.dark_theme);
        let tb = self.toolbar_rect;
        let tw = tb.width();
        let th = tb.height();
        if tw <= 0 || th <= 0 {
            return;
        }
        let full_w = tw + shadow * 2;
        let full_h = th + shadow * 2;
        if self
            .gpu
            .as_ref()
            .and_then(|g| g.toolbar_cache.as_ref())
            .is_some_and(|c| c.key == key && c.width == full_w && c.height == full_h)
        {
            return;
        }
        let sf = dpi as f32 / 96.0;
        let s = |v: i32| scale_dpi(v, dpi);
        let Some(mut pm) = Pixmap::new(full_w as u32, full_h as u32) else {
            return;
        };
        let ox = shadow as f32;
        let oy = shadow as f32;
        draw_soft_shadow(&mut pm, ox, oy, tw as f32, th as f32, 12.0 * sf, sf);

        let (dock_bg, dock_border, div_col, hover_bg) = if self.dark_theme {
            (
                Color::from_rgba8(15, 23, 42, 240),
                Color::from_rgba8(255, 255, 255, 38),
                Color::from_rgba8(255, 255, 255, 32),
                Color::from_rgba8(255, 255, 255, 30),
            )
        } else {
            (
                Color::from_rgba8(255, 255, 255, 248),
                Color::from_rgba8(15, 23, 42, 34),
                Color::from_rgba8(15, 23, 42, 26),
                Color::from_rgba8(15, 23, 42, 16),
            )
        };
        fill_rounded_rect(&mut pm, ox, oy, tw as f32, th as f32, 12.0 * sf, dock_bg);
        stroke_rounded_rect(
            &mut pm,
            ox + 0.5 * sf,
            oy + 0.5 * sf,
            tw as f32 - sf,
            th as f32 - sf,
            11.5 * sf,
            1.0 * sf,
            dock_border,
        );
        if let Some(cancel_btn) = self.buttons.get(3) {
            let div_x = ox + (cancel_btn.rect.left - tb.left) as f32 - 5.5 * sf;
            fill_rounded_rect(
                &mut pm,
                div_x,
                oy + 10.0 * sf,
                (1.0 * sf).max(1.0),
                th as f32 - 20.0 * sf,
                0.5 * sf,
                div_col,
            );
        }
        for btn in &self.buttons {
            let lx = ox + (btn.rect.left - tb.left) as f32;
            let ly = oy + (btn.rect.top - tb.top) as f32;
            let bw = btn.rect.width() as f32;
            let bh = btn.rect.height() as f32;
            let hovered = self.hovered_button == Some(btn.action);
            if btn.action == ToolbarAction::Copy {
                let (c0, c1) = if hovered {
                    (Color::from_rgba8(56, 189, 248, 255), Color::from_rgba8(37, 99, 235, 255))
                } else {
                    (Color::from_rgba8(14, 165, 233, 248), Color::from_rgba8(37, 99, 235, 248))
                };
                if let Some(path) = rounded_rect_path(lx, ly, bw, bh, 8.0 * sf) {
                    if let Some(grad) = LinearGradient::new(
                        Point::from_xy(lx, ly),
                        Point::from_xy(lx + bw, ly + bh),
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
            } else {
                let bg = match (btn.action, hovered) {
                    (ToolbarAction::Cancel, true) => Some(Color::from_rgba8(239, 68, 68, 220)),
                    (_, true) => Some(hover_bg),
                    _ => None,
                };
                if let Some(c) = bg {
                    fill_rounded_rect(&mut pm, lx, ly, bw, bh, 8.0 * sf, c);
                }
            }
            let icon_col = if btn.action == ToolbarAction::Copy
                || (btn.action == ToolbarAction::Cancel && hovered)
                || self.dark_theme
            {
                Color::from_rgba8(248, 250, 252, 255)
            } else if btn.action == ToolbarAction::Cancel {
                Color::from_rgba8(100, 116, 139, 255)
            } else {
                Color::from_rgba8(30, 41, 59, 255)
            };
            draw_toolbar_icon(
                &mut pm,
                btn.action,
                lx + 11.0 * sf,
                ly + (bh - 14.0 * sf) * 0.5,
                sf,
                icon_col,
            );
        }

        let rt = self.gpu.as_ref().map(|g| g.rt.clone());
        if let Some(rt) = rt {
            if let Some(bmp) = GpuOverlay::upload_widget_bitmap(&rt, &pm, |surf| {
                for btn in &self.buttons {
                    let lx = shadow + (btn.rect.left - tb.left);
                    let ly = shadow + (btn.rect.top - tb.top);
                    let hovered = self.hovered_button == Some(btn.action);
                    let txt_color = if btn.action == ToolbarAction::Copy
                        || (btn.action == ToolbarAction::Cancel && hovered)
                    {
                        (255, 255, 255)
                    } else if self.dark_theme {
                        if btn.action == ToolbarAction::Cancel {
                            (203, 213, 225)
                        } else {
                            (248, 250, 252)
                        }
                    } else if btn.action == ToolbarAction::Cancel {
                        (100, 116, 139)
                    } else {
                        (15, 23, 42)
                    };
                    self.draw_text_on_dc(
                        surf.dc,
                        btn.label,
                        RectI::new(lx + s(27), ly, lx + btn.rect.width() - s(6), ly + btn.rect.height()),
                        txt_color,
                        dpi,
                        false,
                        true,
                    );
                }
            }) {
                if let Some(g) = self.gpu.as_mut() {
                    g.toolbar_cache = Some(CachedGpuBitmap {
                        key,
                        bmp,
                        width: full_w,
                        height: full_h,
                    });
                }
            }
        }
    }

    #[allow(clippy::too_many_arguments)]
    fn ensure_gpu_magnifier(
        &mut self,
        cx: i32,
        cy: i32,
        dpi: u32,
        zoom: i32,
        pad: i32,
        _info_h: i32,
        total_w: i32,
        total_h: i32,
        shadow: i32,
    ) {
        let key = (cx, cy, dpi, self.dark_theme);
        if self
            .gpu
            .as_ref()
            .and_then(|g| g.magnifier_cache.as_ref())
            .is_some_and(|c| c.key == key)
        {
            return;
        }
        let s = |v: i32| scale_dpi(v, dpi);
        let sf = dpi as f32 / 96.0;
        let sample_w = 17;
        let sample_h = 11;
        let view_w = sample_w * zoom;
        let view_h = sample_h * zoom;
        let view_left_off = (total_w - view_w) / 2;
        let full_w = total_w + shadow * 2;
        let full_h = total_h + shadow * 2;
        let Some(mut pm) = Pixmap::new(full_w as u32, full_h as u32) else {
            return;
        };

        let (card_bg, card_border, ring_col, hex_col, hint_col) = if self.dark_theme {
            (
                Color::from_rgba8(15, 23, 42, 240),
                Color::from_rgba8(255, 255, 255, 42),
                Color::from_rgba8(255, 255, 255, 200),
                (248, 250, 252),
                (148, 163, 184),
            )
        } else {
            (
                Color::from_rgba8(255, 255, 255, 248),
                Color::from_rgba8(15, 23, 42, 34),
                Color::from_rgba8(100, 116, 139, 200),
                (15, 23, 42),
                (100, 116, 139),
            )
        };
        let ox = shadow as f32;
        let oy = shadow as f32;
        draw_soft_shadow(&mut pm, ox, oy, total_w as f32, total_h as f32, 12.0 * sf, sf);
        fill_rounded_rect(&mut pm, ox, oy, total_w as f32, total_h as f32, 12.0 * sf, card_bg);
        stroke_rounded_rect(
            &mut pm,
            ox + 0.5 * sf,
            oy + 0.5 * sf,
            total_w as f32 - sf,
            total_h as f32 - sf,
            11.5 * sf,
            1.0 * sf,
            card_border,
        );

        let (r, g, b) = self.capture.image.pixel_rgb(cx, cy).unwrap_or((0, 0, 0));
        let sw_cx = ox + (pad + s(10)) as f32;
        let sw_cy = oy + (pad + view_h + s(17)) as f32;
        if let Some(circ) = PathBuilder::from_circle(sw_cx, sw_cy, 7.0 * sf) {
            let mut paint = Paint::default();
            paint.set_color(Color::from_rgba8(r, g, b, 255));
            paint.anti_alias = true;
            pm.fill_path(&circ, &paint, FillRule::Winding, Transform::identity(), None);
            let mut ring = Paint::default();
            ring.set_color(ring_col);
            ring.anti_alias = true;
            let st = Stroke {
                width: 1.2 * sf,
                ..Stroke::default()
            };
            pm.stroke_path(&circ, &ring, &st, Transform::identity(), None);
        }

        // Paint zoomed pixel grid directly into pm's pixels (super fast!)
        let half_x = sample_w / 2;
        let half_y = sample_h / 2;
        let img_w = self.capture.image.width as i32;
        let img_h = self.capture.image.height as i32;
        let vx0 = shadow + view_left_off;
        let vy0 = shadow + pad;
        let pm_stride = full_w as usize;
        {
            let pm_px = pm.data_mut();
            for sy_off in 0..sample_h {
                for sx_off in 0..sample_w {
                    let px = cx - half_x + sx_off;
                    let py = cy - half_y + sy_off;
                    let rgba = if px >= 0 && py >= 0 && px < img_w && py < img_h {
                        let idx = ((py as u32 * self.capture.image.width + px as u32) * 4) as usize;
                        let d = &self.capture.image.data;
                        [d[idx + 2], d[idx + 1], d[idx], 255]
                    } else {
                        [34, 24, 20, 255]
                    };
                    let dx0 = vx0 + sx_off * zoom;
                    let dy0 = vy0 + sy_off * zoom;
                    for yy in dy0..(dy0 + zoom) {
                        let row = (yy as usize) * pm_stride;
                        for xx in dx0..(dx0 + zoom) {
                            let off = (row + xx as usize) * 4;
                            pm_px[off..off + 4].copy_from_slice(&rgba);
                        }
                    }
                }
            }
        }
        // Crosshair + center pixel box + viewport rim
        let border_w = s(1).max(1) as f32;
        let center_x = (vx0 + half_x * zoom) as f32;
        let center_y = (vy0 + half_y * zoom) as f32;
        let mut p_cyan = Paint::default();
        p_cyan.set_color(Color::from_rgba8(56, 189, 248, 255));
        if let Some(rr) = tiny_skia::Rect::from_xywh(
            vx0 as f32,
            center_y + (zoom / 2) as f32,
            view_w as f32,
            border_w,
        ) {
            pm.fill_rect(rr, &p_cyan, Transform::identity(), None);
        }
        if let Some(rr) = tiny_skia::Rect::from_xywh(
            center_x + (zoom / 2) as f32,
            vy0 as f32,
            border_w,
            view_h as f32,
        ) {
            pm.fill_rect(rr, &p_cyan, Transform::identity(), None);
        }
        stroke_rounded_rect(
            &mut pm,
            center_x - border_w * 0.5,
            center_y - border_w * 0.5,
            zoom as f32 + border_w,
            zoom as f32 + border_w,
            0.0,
            border_w,
            Color::from_rgba8(255, 255, 255, 255),
        );
        stroke_rounded_rect(
            &mut pm,
            vx0 as f32 + border_w * 0.5,
            vy0 as f32 + border_w * 0.5,
            view_w as f32 - border_w,
            view_h as f32 - border_w,
            0.0,
            border_w,
            Color::from_rgba8(75, 85, 99, 220),
        );

        let info_top = vy0 + view_h;
        let hex_line = format!("#{:02X}{:02X}{:02X}  ({},{})", r, g, b, cx, cy);
        let rt = self.gpu.as_ref().map(|g| g.rt.clone());
        if let Some(rt) = rt {
            if let Some(bmp) = GpuOverlay::upload_widget_bitmap(&rt, &pm, |surf| {
                self.draw_text_on_dc(
                    surf.dc,
                    &hex_line,
                    RectI::new(
                        shadow + pad + s(24),
                        info_top + s(6),
                        shadow + total_w - pad,
                        info_top + s(28),
                    ),
                    hex_col,
                    dpi,
                    true,
                    false,
                );
                self.draw_text_on_dc(
                    surf.dc,
                    "按 C 复制色值 · 右键重选",
                    RectI::new(
                        shadow + pad + s(2),
                        info_top + s(28),
                        shadow + total_w - pad,
                        shadow + total_h - s(6),
                    ),
                    hint_col,
                    dpi,
                    true,
                    false,
                );
            }) {
                if let Some(g) = self.gpu.as_mut() {
                    g.magnifier_cache = Some(CachedGpuBitmap {
                        key,
                        bmp,
                        width: full_w,
                        height: full_h,
                    });
                }
            }
        }
    }

    fn render(&mut self) {
        self.layout_toolbar();
        if !self.ensure_cpu_buffers() {
            return;
        }

        let width = self.capture.image.width as usize;
        let height = self.capture.image.height as usize;
        let src = &self.capture.image.data;
        let active_rect = self.selection.or(self.hover_rect);

        // 1. Copy pre-computed dimmed backdrop + bright cutout inside active selection/hover rect
        let bounds = self.bounds();
        if let (Some(surface), Some(dimmed)) = (self.surface.as_mut(), self.dimmed_bg.as_ref()) {
            let dst = surface.pixels_mut();
            dst.copy_from_slice(dimmed);

            if let Some(r) = active_rect {
                let r = r.intersect(bounds);
                if !r.is_empty() {
                    let row_start = (r.left as usize) * 4;
                    let row_len = (r.width() as usize) * 4;
                    let stride = width * 4;
                    for y in r.top as usize..r.bottom as usize {
                        let off = y * stride + row_start;
                        dst[off..off + row_len].copy_from_slice(&src[off..off + row_len]);
                    }
                }
            }
        }

        // 2. Draw crisp modern border, corner brackets (hover) or circular handles (selected)
        if let Some(r) = active_rect {
            let r = r.intersect(self.bounds());
            if !r.is_empty() {
                let dpi = self
                    .monitor_info_for_point((r.left + r.right) / 2, (r.top + r.bottom) / 2)
                    .dpi;
                let s = |v: i32| scale_dpi(v, dpi);
                let sf = dpi as f32 / 96.0;
                let is_confirmed = self.selection.is_some();

                // Subtle outer dark contrast rim so border is crystal-clear on pure white backgrounds
                let halo = s(1).max(1);
                self.draw_rect_outline(
                    RectI::new(r.left - halo, r.top - halo, r.right + halo, r.bottom + halo),
                    halo,
                    [42, 23, 15, 255],
                );

                // Luminous sky-cyan border (#38BDF8 in BGRA)
                let thickness = s(2).max(2);
                self.draw_rect_outline(r, thickness, [248, 189, 56, 255]);

                if !is_confirmed {
                    // Modern viewfinder L-corner accents for smart window hover preview
                    let arm = s(16).min(r.width() / 3).min(r.height() / 3).max(4);
                    let bt = s(3).max(3);
                    let bright = [253, 211, 125, 255]; // #7DD3FC in BGRA
                    // Top-left
                    self.fill_rect_solid(RectI::new(r.left, r.top, r.left + arm, r.top + bt), bright);
                    self.fill_rect_solid(RectI::new(r.left, r.top, r.left + bt, r.top + arm), bright);
                    // Top-right
                    self.fill_rect_solid(RectI::new(r.right - arm, r.top, r.right, r.top + bt), bright);
                    self.fill_rect_solid(RectI::new(r.right - bt, r.top, r.right, r.top + arm), bright);
                    // Bottom-left
                    self.fill_rect_solid(RectI::new(r.left, r.bottom - bt, r.left + arm, r.bottom), bright);
                    self.fill_rect_solid(RectI::new(r.left, r.bottom - arm, r.left + bt, r.bottom), bright);
                    // Bottom-right
                    self.fill_rect_solid(RectI::new(r.right - arm, r.bottom - bt, r.right, r.bottom), bright);
                    self.fill_rect_solid(RectI::new(r.right - bt, r.bottom - arm, r.right, r.bottom), bright);
                } else if self.drag_mode == DragMode::None {
                    // Anti-aliased floating circular resize handles
                    let mx = (r.left + r.right) / 2;
                    let my = (r.top + r.bottom) / 2;
                    let pts = [
                        (r.left, r.top),
                        (mx, r.top),
                        (r.right, r.top),
                        (r.left, my),
                        (r.right, my),
                        (r.left, r.bottom),
                        (mx, r.bottom),
                        (r.right, r.bottom),
                    ];
                    if let Some(handle_pm) = build_circle_handle_pixmap(sf) {
                        let hw = handle_pm.width() as i32;
                        let hh = handle_pm.height() as i32;
                        for (hx, hy) in pts {
                            self.composite_pixmap(hx - hw / 2, hy - hh / 2, &handle_pm);
                        }
                    }
                }

                // Size pill badge above top-left of active rect
                self.draw_size_badge(r);
            }
        }

        // 2.5 Subtle top-center onboarding hint pill before dragging
        if self.selection.is_none() && self.drag_mode == DragMode::None {
            self.draw_top_hint_pill();
        }

        // 3. Draw floating toolbar
        if !self.toolbar_rect.is_empty() {
            self.draw_toolbar();
        }

        // 4. Draw magnifier loupe during hover / creation / resize
        let show_loupe = self.show_magnifier
            && (self.selection.is_none()
                || matches!(
                    self.drag_mode,
                    DragMode::Creating
                        | DragMode::ResizeN
                        | DragMode::ResizeS
                        | DragMode::ResizeW
                        | DragMode::ResizeE
                        | DragMode::ResizeNW
                        | DragMode::ResizeNE
                        | DragMode::ResizeSW
                        | DragMode::ResizeSE
                ));
        if show_loupe && width > 20 && height > 20 {
            self.draw_magnifier();
        }
    }

    fn fill_rect_solid(&mut self, rect: RectI, bgra: [u8; 4]) {
        let r = rect.intersect(self.bounds());
        if r.is_empty() {
            return;
        }
        let Some(surface) = self.surface.as_mut() else {
            return;
        };
        let w = surface.width as usize;
        let dst = surface.pixels_mut();
        for y in r.top as usize..r.bottom as usize {
            let row = y * w;
            for x in r.left as usize..r.right as usize {
                let idx = (row + x) * 4;
                dst[idx..idx + 4].copy_from_slice(&bgra);
            }
        }
    }

    fn draw_rect_outline(&mut self, rect: RectI, thickness: i32, bgra: [u8; 4]) {
        self.fill_rect_solid(
            RectI::new(rect.left, rect.top, rect.right, rect.top + thickness),
            bgra,
        );
        self.fill_rect_solid(
            RectI::new(rect.left, rect.bottom - thickness, rect.right, rect.bottom),
            bgra,
        );
        self.fill_rect_solid(
            RectI::new(rect.left, rect.top, rect.left + thickness, rect.bottom),
            bgra,
        );
        self.fill_rect_solid(
            RectI::new(rect.right - thickness, rect.top, rect.right, rect.bottom),
            bgra,
        );
    }

    fn composite_pixmap(&mut self, dest_x: i32, dest_y: i32, pixmap: &Pixmap) {
        let pw = pixmap.width() as i32;
        let ph = pixmap.height() as i32;
        let Some(surface) = self.surface.as_mut() else {
            return;
        };
        let sw = surface.width;
        let sh = surface.height;
        let src = pixmap.data(); // premultiplied RGBA
        let dst = surface.pixels_mut();

        for py in 0..ph {
            let sy = dest_y + py;
            if sy < 0 || sy >= sh {
                continue;
            }
            for px in 0..pw {
                let sx = dest_x + px;
                if sx < 0 || sx >= sw {
                    continue;
                }
                let s_idx = ((py * pw + px) * 4) as usize;
                let sa = src[s_idx + 3] as u32;
                if sa == 0 {
                    continue;
                }
                let d_idx = ((sy * sw + sx) * 4) as usize;
                if sa == 255 {
                    dst[d_idx] = src[s_idx + 2];
                    dst[d_idx + 1] = src[s_idx + 1];
                    dst[d_idx + 2] = src[s_idx];
                    dst[d_idx + 3] = 255;
                } else {
                    let inv = 255 - sa;
                    // src is premultiplied RGBA; dst is opaque BGRA
                    let out_b = (src[s_idx + 2] as u32) + ((dst[d_idx] as u32 * inv + 127) / 255);
                    let out_g =
                        (src[s_idx + 1] as u32) + ((dst[d_idx + 1] as u32 * inv + 127) / 255);
                    let out_r = (src[s_idx] as u32) + ((dst[d_idx + 2] as u32 * inv + 127) / 255);
                    dst[d_idx] = out_b.min(255) as u8;
                    dst[d_idx + 1] = out_g.min(255) as u8;
                    dst[d_idx + 2] = out_r.min(255) as u8;
                    dst[d_idx + 3] = 255;
                }
            }
        }
    }

    fn draw_text_on_surface(
        &self,
        text: &str,
        rect: RectI,
        color: (u8, u8, u8),
        dpi: u32,
        small: bool,
        centered: bool,
    ) {
        let Some(ref surface) = self.surface else {
            return;
        };
        self.draw_text_on_dc(surface.dc, text, rect, color, dpi, small, centered);
    }

    fn draw_top_hint_pill(&mut self) {
        let mon_info = self.monitor_info_for_point(self.cursor_pos.0, self.cursor_pos.1);
        let mon = mon_info.rect;
        let dpi = mon_info.dpi;
        let s = |v: i32| scale_dpi(v, dpi);
        let sf = dpi as f32 / 96.0;

        let text = "单击选中窗口 · 拖拽自由框选 · 按 C 取色 · 右键 / Esc 退出";
        let pill_w = s(370).min(mon.width() - s(24));
        let pill_h = s(30);
        let shadow = s(10);
        let px = mon.left + (mon.width() - pill_w) / 2;
        let py = mon.top + s(18);

        let (bg, border, txt_col) = if self.dark_theme {
            (
                Color::from_rgba8(15, 23, 42, 226),
                Color::from_rgba8(255, 255, 255, 36),
                (226, 232, 240),
            )
        } else {
            (
                Color::from_rgba8(255, 255, 255, 242),
                Color::from_rgba8(15, 23, 42, 34),
                (15, 23, 42),
            )
        };
        if let Some(mut pm) =
            Pixmap::new((pill_w + shadow * 2) as u32, (pill_h + shadow * 2) as u32)
        {
            let ox = shadow as f32;
            let oy = shadow as f32;
            draw_soft_shadow(&mut pm, ox, oy, pill_w as f32, pill_h as f32, 15.0 * sf, sf);
            fill_rounded_rect(&mut pm, ox, oy, pill_w as f32, pill_h as f32, 15.0 * sf, bg);
            stroke_rounded_rect(
                &mut pm,
                ox + 0.5 * sf,
                oy + 0.5 * sf,
                pill_w as f32 - sf,
                pill_h as f32 - sf,
                14.5 * sf,
                1.0 * sf,
                border,
            );
            self.composite_pixmap(px - shadow, py - shadow, &pm);
        }
        self.draw_text_on_surface(
            text,
            RectI::new(px, py, px + pill_w, py + pill_h),
            txt_col,
            dpi,
            true,
            true,
        );
    }

    fn draw_size_badge(&mut self, sel: RectI) {
        let mon_info = self.monitor_info_for_point(sel.left, sel.top);
        let mon = mon_info.rect;
        let dpi = mon_info.dpi;
        let s = |v: i32| scale_dpi(v, dpi);
        let sf = dpi as f32 / 96.0;

        let text = format!("{} × {}", sel.width(), sel.height());
        let badge_w = s(106).max(s((text.chars().count() as i32) * 8 + 28));
        let badge_h = s(28);
        let shadow = s(8);
        let pad_edge = s(6);
        let gap = s(8);
        let bx = sel
            .left
            .clamp(mon.left + pad_edge, (mon.right - badge_w - pad_edge).max(mon.left + pad_edge));
        let by = if sel.top - badge_h - gap >= mon.top + pad_edge {
            sel.top - badge_h - gap
        } else {
            sel.top + gap
        };

        let (bg, border, dot_col, txt_col) = if self.dark_theme {
            (
                Color::from_rgba8(15, 23, 42, 234),
                Color::from_rgba8(255, 255, 255, 36),
                Color::from_rgba8(56, 189, 248, 255),
                (248, 250, 252),
            )
        } else {
            (
                Color::from_rgba8(255, 255, 255, 246),
                Color::from_rgba8(15, 23, 42, 34),
                Color::from_rgba8(2, 132, 199, 255),
                (15, 23, 42),
            )
        };
        if let Some(mut pm) =
            Pixmap::new((badge_w + shadow * 2) as u32, (badge_h + shadow * 2) as u32)
        {
            let ox = shadow as f32;
            let oy = shadow as f32;
            draw_soft_shadow(&mut pm, ox, oy, badge_w as f32, badge_h as f32, 8.0 * sf, sf);
            fill_rounded_rect(&mut pm, ox, oy, badge_w as f32, badge_h as f32, 8.0 * sf, bg);
            stroke_rounded_rect(
                &mut pm,
                ox + 0.5 * sf,
                oy + 0.5 * sf,
                badge_w as f32 - sf,
                badge_h as f32 - sf,
                7.5 * sf,
                1.0 * sf,
                border,
            );
            if let Some(dot) = PathBuilder::from_circle(
                ox + 11.0 * sf,
                oy + (badge_h as f32) * 0.5,
                3.0 * sf,
            ) {
                let mut paint = Paint::default();
                paint.set_color(dot_col);
                paint.anti_alias = true;
                pm.fill_path(&dot, &paint, FillRule::Winding, Transform::identity(), None);
            }
            self.composite_pixmap(bx - shadow, by - shadow, &pm);
        }
        self.draw_text_on_surface(
            &text,
            RectI::new(bx + s(16), by, bx + badge_w - s(6), by + badge_h),
            txt_col,
            dpi,
            true,
            true,
        );
    }

    fn draw_toolbar(&mut self) {
        let tb = self.toolbar_rect;
        let tw = tb.width();
        let th = tb.height();
        if tw <= 0 || th <= 0 {
            return;
        }
        let dpi = self.toolbar_dpi;
        let sf = dpi as f32 / 96.0;
        let s = |v: i32| scale_dpi(v, dpi);
        let shadow = s(12);
        let Some(mut pm) = Pixmap::new((tw + shadow * 2) as u32, (th + shadow * 2) as u32) else {
            return;
        };

        let ox = shadow as f32;
        let oy = shadow as f32;

        // Soft multi-layer elevation shadow
        draw_soft_shadow(&mut pm, ox, oy, tw as f32, th as f32, 12.0 * sf, sf);

        // Frosted glass dock surface (Light or Dark theme)
        let (dock_bg, dock_border, div_col, hover_bg) = if self.dark_theme {
            (
                Color::from_rgba8(15, 23, 42, 240),
                Color::from_rgba8(255, 255, 255, 38),
                Color::from_rgba8(255, 255, 255, 32),
                Color::from_rgba8(255, 255, 255, 30),
            )
        } else {
            (
                Color::from_rgba8(255, 255, 255, 248),
                Color::from_rgba8(15, 23, 42, 34),
                Color::from_rgba8(15, 23, 42, 26),
                Color::from_rgba8(15, 23, 42, 16),
            )
        };
        fill_rounded_rect(&mut pm, ox, oy, tw as f32, th as f32, 12.0 * sf, dock_bg);
        stroke_rounded_rect(
            &mut pm,
            ox + 0.5 * sf,
            oy + 0.5 * sf,
            tw as f32 - sf,
            th as f32 - sf,
            11.5 * sf,
            1.0 * sf,
            dock_border,
        );

        // Subtle divider before Cancel button
        if let Some(cancel_btn) = self.buttons.get(3) {
            let div_x = ox + (cancel_btn.rect.left - tb.left) as f32 - 5.5 * sf;
            fill_rounded_rect(
                &mut pm,
                div_x,
                oy + 10.0 * sf,
                (1.0 * sf).max(1.0),
                th as f32 - 20.0 * sf,
                0.5 * sf,
                div_col,
            );
        }

        for btn in &self.buttons {
            let lx = ox + (btn.rect.left - tb.left) as f32;
            let ly = oy + (btn.rect.top - tb.top) as f32;
            let bw = btn.rect.width() as f32;
            let bh = btn.rect.height() as f32;
            let hovered = self.hovered_button == Some(btn.action);

            if btn.action == ToolbarAction::Copy {
                let (c0, c1) = if hovered {
                    (
                        Color::from_rgba8(56, 189, 248, 255),
                        Color::from_rgba8(37, 99, 235, 255),
                    )
                } else {
                    (
                        Color::from_rgba8(14, 165, 233, 248),
                        Color::from_rgba8(37, 99, 235, 248),
                    )
                };
                if let Some(path) = rounded_rect_path(lx, ly, bw, bh, 8.0 * sf) {
                    if let Some(grad) = LinearGradient::new(
                        Point::from_xy(lx, ly),
                        Point::from_xy(lx + bw, ly + bh),
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
            } else {
                let bg = match (btn.action, hovered) {
                    (ToolbarAction::Cancel, true) => Some(Color::from_rgba8(239, 68, 68, 220)),
                    (_, true) => Some(hover_bg),
                    _ => None,
                };
                if let Some(c) = bg {
                    fill_rounded_rect(&mut pm, lx, ly, bw, bh, 8.0 * sf, c);
                }
            }

            let icon_col = if btn.action == ToolbarAction::Copy
                || (btn.action == ToolbarAction::Cancel && hovered)
                || self.dark_theme
            {
                Color::from_rgba8(248, 250, 252, 255)
            } else if btn.action == ToolbarAction::Cancel {
                Color::from_rgba8(100, 116, 139, 255)
            } else {
                Color::from_rgba8(30, 41, 59, 255)
            };
            let ix = lx + 11.0 * sf;
            let iy = ly + (bh - 14.0 * sf) * 0.5;
            draw_toolbar_icon(&mut pm, btn.action, ix, iy, sf, icon_col);
        }

        self.composite_pixmap(tb.left - shadow, tb.top - shadow, &pm);

        // Draw crisp labels via GDI ClearType at the monitor's exact DPI
        for btn in &self.buttons {
            let text_rect = RectI::new(
                btn.rect.left + s(27),
                btn.rect.top,
                btn.rect.right - s(6),
                btn.rect.bottom,
            );
            let hovered = self.hovered_button == Some(btn.action);
            let txt_color = if btn.action == ToolbarAction::Copy
                || (btn.action == ToolbarAction::Cancel && hovered)
            {
                (255, 255, 255)
            } else if self.dark_theme {
                if btn.action == ToolbarAction::Cancel {
                    (203, 213, 225)
                } else {
                    (248, 250, 252)
                }
            } else if btn.action == ToolbarAction::Cancel {
                (100, 116, 139)
            } else {
                (15, 23, 42)
            };
            self.draw_text_on_surface(btn.label, text_rect, txt_color, dpi, false, true);
        }
    }

    fn draw_magnifier(&mut self) {
        let (cx, cy) = self.cursor_pos;
        let mon_info = self.monitor_info_for_point(cx, cy);
        let mon = mon_info.rect;
        let dpi = mon_info.dpi;
        let s = |v: i32| scale_dpi(v, dpi);
        let sf = dpi as f32 / 96.0;

        let sample_w = 17;
        let sample_h = 11;
        let zoom = s(9).max(8);
        let view_w = sample_w * zoom;
        let view_h = sample_h * zoom;
        let pad = s(8);
        let info_h = s(52);
        let total_w = view_w.max(s(176)) + pad * 2;
        let total_h = view_h + info_h + pad * 2;
        let view_left_off = (total_w - view_w) / 2;
        let shadow = s(12);

        let offset = s(22);
        let edge_pad = s(10);
        let mut mx = cx + offset;
        let mut my = cy + offset;
        if mx + total_w > mon.right - edge_pad {
            mx = cx - offset - total_w;
        }
        if my + total_h > mon.bottom - edge_pad {
            my = cy - offset - total_h;
        }
        mx = mx.clamp(mon.left + s(6), (mon.right - total_w - s(6)).max(mon.left + s(6)));
        my = my.clamp(mon.top + s(6), (mon.bottom - total_h - s(6)).max(mon.top + s(6)));

        // Rounded floating card with soft shadow (Light or Dark)
        let (card_bg, card_border, ring_col, hex_col, hint_col) = if self.dark_theme {
            (
                Color::from_rgba8(15, 23, 42, 240),
                Color::from_rgba8(255, 255, 255, 42),
                Color::from_rgba8(255, 255, 255, 200),
                (248, 250, 252),
                (148, 163, 184),
            )
        } else {
            (
                Color::from_rgba8(255, 255, 255, 248),
                Color::from_rgba8(15, 23, 42, 34),
                Color::from_rgba8(100, 116, 139, 200),
                (15, 23, 42),
                (100, 116, 139),
            )
        };
        if let Some(mut pm) =
            Pixmap::new((total_w + shadow * 2) as u32, (total_h + shadow * 2) as u32)
        {
            let ox = shadow as f32;
            let oy = shadow as f32;
            draw_soft_shadow(&mut pm, ox, oy, total_w as f32, total_h as f32, 12.0 * sf, sf);
            fill_rounded_rect(&mut pm, ox, oy, total_w as f32, total_h as f32, 12.0 * sf, card_bg);
            stroke_rounded_rect(
                &mut pm,
                ox + 0.5 * sf,
                oy + 0.5 * sf,
                total_w as f32 - sf,
                total_h as f32 - sf,
                11.5 * sf,
                1.0 * sf,
                card_border,
            );

            // Circular color swatch in footer
            let (r, g, b) = self.capture.image.pixel_rgb(cx, cy).unwrap_or((0, 0, 0));
            let sw_cx = ox + (pad + s(10)) as f32;
            let sw_cy = oy + (pad + view_h + s(17)) as f32;
            let sw_r = 7.0 * sf;
            if let Some(circ) = PathBuilder::from_circle(sw_cx, sw_cy, sw_r) {
                let mut paint = Paint::default();
                paint.set_color(Color::from_rgba8(r, g, b, 255));
                paint.anti_alias = true;
                pm.fill_path(&circ, &paint, FillRule::Winding, Transform::identity(), None);
                let mut ring = Paint::default();
                ring.set_color(ring_col);
                ring.anti_alias = true;
                let st = Stroke {
                    width: 1.2 * sf,
                    ..Stroke::default()
                };
                pm.stroke_path(&circ, &ring, &st, Transform::identity(), None);
            }

            self.composite_pixmap(mx - shadow, my - shadow, &pm);
        }

        // Zoomed pixel viewport inset inside card
        let half_x = sample_w / 2;
        let half_y = sample_h / 2;
        let img_w = self.capture.image.width as i32;
        let img_h = self.capture.image.height as i32;
        let vx0 = mx + view_left_off;
        let vy0 = my + pad;
        for sy_off in 0..sample_h {
            for sx_off in 0..sample_w {
                let px = cx - half_x + sx_off;
                let py = cy - half_y + sy_off;
                let bgra = if px >= 0 && py >= 0 && px < img_w && py < img_h {
                    let idx = ((py as u32 * self.capture.image.width + px as u32) * 4) as usize;
                    let d = &self.capture.image.data;
                    [d[idx], d[idx + 1], d[idx + 2], 255]
                } else {
                    [20, 24, 34, 255]
                };
                let dx = vx0 + sx_off * zoom;
                let dy = vy0 + sy_off * zoom;
                self.fill_rect_solid(RectI::new(dx, dy, dx + zoom, dy + zoom), bgra);
            }
        }

        // Subtle pixel grid + luminous cyan crosshair + center cell ring
        let border_w = s(1).max(1);
        let center_x = vx0 + half_x * zoom;
        let center_y = vy0 + half_y * zoom;
        self.fill_rect_solid(
            RectI::new(vx0, center_y + zoom / 2, vx0 + view_w, center_y + zoom / 2 + border_w),
            [248, 189, 56, 255],
        );
        self.fill_rect_solid(
            RectI::new(center_x + zoom / 2, vy0, center_x + zoom / 2 + border_w, vy0 + view_h),
            [248, 189, 56, 255],
        );
        self.draw_rect_outline(
            RectI::new(
                center_x - border_w,
                center_y - border_w,
                center_x + zoom + border_w,
                center_y + zoom + border_w,
            ),
            border_w,
            [255, 255, 255, 255],
        );
        self.draw_rect_outline(
            RectI::new(vx0, vy0, vx0 + view_w, vy0 + view_h),
            border_w,
            [75, 58, 45, 255],
        );

        let (r, g, b) = self.capture.image.pixel_rgb(cx, cy).unwrap_or((0, 0, 0));
        let info_top = vy0 + view_h;
        let hex_line = format!("#{:02X}{:02X}{:02X}  ({},{})", r, g, b, cx, cy);
        self.draw_text_on_surface(
            &hex_line,
            RectI::new(
                mx + pad + s(24),
                info_top + s(6),
                mx + total_w - pad,
                info_top + s(28),
            ),
            hex_col,
            dpi,
            true,
            false,
        );
        self.draw_text_on_surface(
            "按 C 复制色值 · 右键重选",
            RectI::new(
                mx + pad + s(2),
                info_top + s(28),
                mx + total_w - pad,
                my + total_h - s(6),
            ),
            hint_col,
            dpi,
            true,
            false,
        );
    }

    fn compute_action_outcome(&self, action: ToolbarAction) -> OverlayOutcome {
        let Some(sel) = self.selection else {
            return OverlayOutcome::Cancel;
        };
        let sel = sel.normalized().intersect(self.bounds());
        if sel.width() < 1 || sel.height() < 1 {
            return OverlayOutcome::Cancel;
        }
        match action {
            ToolbarAction::Cancel => OverlayOutcome::Cancel,
            ToolbarAction::Copy => self
                .capture
                .image
                .crop(sel.left, sel.top, sel.width(), sel.height())
                .map(OverlayOutcome::Copy)
                .unwrap_or(OverlayOutcome::Cancel),
            ToolbarAction::Save => self
                .capture
                .image
                .crop(sel.left, sel.top, sel.width(), sel.height())
                .map(OverlayOutcome::Save)
                .unwrap_or(OverlayOutcome::Cancel),
            ToolbarAction::Pin => self
                .capture
                .image
                .crop(sel.left, sel.top, sel.width(), sel.height())
                .map(|image| OverlayOutcome::Pin {
                    image,
                    screen_x: self.capture.origin_x + sel.left,
                    screen_y: self.capture.origin_y + sel.top,
                })
                .unwrap_or(OverlayOutcome::Cancel),
        }
    }

    fn close_window_and_take_callback(&mut self) -> Option<Box<dyn FnOnce(OverlayOutcome)>> {
        if self.finished {
            return None;
        }
        self.finished = true;
        let hwnd = self.hwnd;
        let callback = self.on_finish.take();

        unsafe {
            let _ = ReleaseCapture();
            if !hwnd.is_invalid() {
                let _ = DestroyWindow(hwnd);
            }
        }
        callback
    }

    fn request_repaint(&mut self) {
        self.dirty = true;
        if !self.hwnd.is_invalid() {
            unsafe {
                let _ = InvalidateRect(Some(self.hwnd), None, false);
            }
        }
    }
}

fn draw_soft_shadow(pm: &mut Pixmap, x: f32, y: f32, w: f32, h: f32, r: f32, sf: f32) {
    let layers = [
        (6.0, 4.0, 14u8),
        (4.0, 2.8, 22u8),
        (2.2, 1.5, 34u8),
        (1.0, 0.8, 46u8),
    ];
    for (spread, dy, alpha) in layers {
        let sp = spread * sf;
        fill_rounded_rect(
            pm,
            x - sp,
            y - sp + dy * sf,
            w + sp * 2.0,
            h + sp * 2.0,
            r + sp * 0.6,
            Color::from_rgba8(0, 0, 0, alpha),
        );
    }
}

fn build_circle_handle_pixmap(sf: f32) -> Option<Pixmap> {
    let dim = (18.0 * sf).ceil().max(16.0) as u32;
    let mut pm = Pixmap::new(dim, dim)?;
    let c = dim as f32 * 0.5;

    // Subtle shadow circle
    if let Some(sh) = PathBuilder::from_circle(c, c + 1.2 * sf, 5.8 * sf) {
        let mut p = Paint::default();
        p.set_color(Color::from_rgba8(0, 0, 0, 72));
        p.anti_alias = true;
        pm.fill_path(&sh, &p, FillRule::Winding, Transform::identity(), None);
    }
    // Crisp white inner pill
    if let Some(dot) = PathBuilder::from_circle(c, c, 4.8 * sf) {
        let mut fill = Paint::default();
        fill.set_color(Color::from_rgba8(255, 255, 255, 255));
        fill.anti_alias = true;
        pm.fill_path(&dot, &fill, FillRule::Winding, Transform::identity(), None);

        let mut ring = Paint::default();
        ring.set_color(Color::from_rgba8(14, 165, 233, 255));
        ring.anti_alias = true;
        let st = Stroke {
            width: 1.8 * sf,
            ..Stroke::default()
        };
        pm.stroke_path(&dot, &ring, &st, Transform::identity(), None);
    }
    Some(pm)
}

fn fill_rounded_rect(pm: &mut Pixmap, x: f32, y: f32, w: f32, h: f32, r: f32, color: Color) {
    if let Some(path) = rounded_rect_path(x, y, w, h, r) {
        let mut paint = Paint::default();
        paint.set_color(color);
        paint.anti_alias = true;
        pm.fill_path(&path, &paint, FillRule::Winding, Transform::identity(), None);
    }
}

fn stroke_rounded_rect(
    pm: &mut Pixmap,
    x: f32,
    y: f32,
    w: f32,
    h: f32,
    r: f32,
    width: f32,
    color: Color,
) {
    if let Some(path) = rounded_rect_path(x, y, w, h, r) {
        let mut paint = Paint::default();
        paint.set_color(color);
        paint.anti_alias = true;
        let stroke = Stroke {
            width,
            ..Stroke::default()
        };
        pm.stroke_path(&path, &paint, &stroke, Transform::identity(), None);
    }
}

fn rounded_rect_path(x: f32, y: f32, w: f32, h: f32, r: f32) -> Option<tiny_skia::Path> {
    let r = r.min(w * 0.5).min(h * 0.5);
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

fn draw_toolbar_icon(
    pm: &mut Pixmap,
    action: ToolbarAction,
    x: f32,
    y: f32,
    scale: f32,
    color: Color,
) {
    let mut paint = Paint::default();
    paint.set_color(color);
    paint.anti_alias = true;
    let stroke = Stroke {
        width: 1.7,
        line_cap: LineCap::Round,
        line_join: LineJoin::Round,
        ..Stroke::default()
    };

    let mut pb = PathBuilder::new();
    match action {
        ToolbarAction::Pin => {
            // Simple pushpin icon (14x14 logical units)
            pb.move_to(4.0, 2.0);
            pb.line_to(10.0, 2.0);
            pb.line_to(9.0, 6.5);
            pb.line_to(11.5, 8.5);
            pb.line_to(2.5, 8.5);
            pb.line_to(5.0, 6.5);
            pb.close();
            pb.move_to(7.0, 8.5);
            pb.line_to(7.0, 13.0);
        }
        ToolbarAction::Save => {
            // Down-arrow into tray
            pb.move_to(7.0, 1.5);
            pb.line_to(7.0, 9.5);
            pb.move_to(4.0, 6.8);
            pb.line_to(7.0, 9.8);
            pb.line_to(10.0, 6.8);
            pb.move_to(2.0, 10.0);
            pb.line_to(2.0, 12.5);
            pb.line_to(12.0, 12.5);
            pb.line_to(12.0, 10.0);
        }
        ToolbarAction::Copy => {
            // Crisp checkmark
            pb.move_to(2.0, 7.5);
            pb.line_to(5.5, 11.0);
            pb.line_to(12.2, 3.2);
        }
        ToolbarAction::Cancel => {
            // Crisp X
            pb.move_to(3.0, 3.0);
            pb.line_to(11.0, 11.0);
            pb.move_to(11.0, 3.0);
            pb.line_to(3.0, 11.0);
        }
    }
    if let Some(path) = pb.finish() {
        let xf = Transform::from_translate(x, y).pre_scale(scale, scale);
        pm.stroke_path(&path, &paint, &stroke, xf, None);
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

unsafe extern "system" fn overlay_wnd_proc(
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
            ACTIVE_OVERLAY.with(|cell| {
                if let Some(s) = cell.borrow_mut().as_mut() {
                    if !s.render_gpu() {
                        s.render_cpu_and_blit(hdc, ps.rcPaint);
                    }
                }
            });
            unsafe {
                let _ = EndPaint(hwnd, &ps);
            }
            return LRESULT(0);
        }
        WM_SETCURSOR => {
            let cursor_id = ACTIVE_OVERLAY.with(|cell| {
                let borrow = cell.borrow();
                let Some(s) = borrow.as_ref() else {
                    return IDC_ARROW;
                };
                let (x, y) = s.cursor_pos;
                if s.button_at(x, y).is_some() {
                    return IDC_HAND;
                }
                match s.hit_test_selection(x, y) {
                    DragMode::ResizeNW | DragMode::ResizeSE => IDC_SIZENWSE,
                    DragMode::ResizeNE | DragMode::ResizeSW => IDC_SIZENESW,
                    DragMode::ResizeN | DragMode::ResizeS => IDC_SIZENS,
                    DragMode::ResizeW | DragMode::ResizeE => IDC_SIZEWE,
                    DragMode::Moving => IDC_SIZEALL,
                    _ => IDC_CROSS,
                }
            });
            unsafe {
                if let Ok(c) = LoadCursorW(None, cursor_id) {
                    SetCursor(Some(c));
                }
            }
            return LRESULT(1);
        }
        WM_MOUSEMOVE => {
            let (x, y) = lparam_xy(lparam);
            ACTIVE_OVERLAY.with(|cell| {
                let mut borrow = cell.borrow_mut();
                let Some(s) = borrow.as_mut() else { return };
                let bounds = s.bounds();
                let cx = x.clamp(0, bounds.right - 1);
                let cy = y.clamp(0, bounds.bottom - 1);
                s.cursor_pos = (cx, cy);

                match s.drag_mode {
                    DragMode::None => {
                        let old_hover_btn = s.hovered_button;
                        s.hovered_button = s.button_at(cx, cy);
                        let old_hover_rect = s.hover_rect;
                        s.update_hover_rect();
                        if s.selection.is_none()
                            || old_hover_btn != s.hovered_button
                            || old_hover_rect != s.hover_rect
                        {
                            s.request_repaint();
                        }
                    }
                    DragMode::Creating => {
                        let r = RectI::new(s.drag_start.0, s.drag_start.1, cx, cy).normalized();
                        s.selection = Some(r);
                        s.request_repaint();
                    }
                    DragMode::Moving => {
                        let dx = cx - s.drag_start.0;
                        let dy = cy - s.drag_start.1;
                        let orig = s.drag_orig_sel;
                        let w = orig.width();
                        let h = orig.height();
                        let nl = (orig.left + dx).clamp(0, (bounds.right - w).max(0));
                        let nt = (orig.top + dy).clamp(0, (bounds.bottom - h).max(0));
                        s.selection = Some(RectI::new(nl, nt, nl + w, nt + h));
                        s.request_repaint();
                    }
                    mode => {
                        let mut r = s.drag_orig_sel;
                        match mode {
                            DragMode::ResizeN => r.top = cy,
                            DragMode::ResizeS => r.bottom = cy,
                            DragMode::ResizeW => r.left = cx,
                            DragMode::ResizeE => r.right = cx,
                            DragMode::ResizeNW => {
                                r.left = cx;
                                r.top = cy;
                            }
                            DragMode::ResizeNE => {
                                r.right = cx;
                                r.top = cy;
                            }
                            DragMode::ResizeSW => {
                                r.left = cx;
                                r.bottom = cy;
                            }
                            DragMode::ResizeSE => {
                                r.right = cx;
                                r.bottom = cy;
                            }
                            _ => {}
                        }
                        s.selection = Some(r.normalized().intersect(bounds));
                        s.request_repaint();
                    }
                }
            });
            return LRESULT(0);
        }
        WM_LBUTTONDOWN => {
            let (x, y) = lparam_xy(lparam);
            let mut action_to_run: Option<ToolbarAction> = None;
            let mut need_capture = false;
            ACTIVE_OVERLAY.with(|cell| {
                let mut borrow = cell.borrow_mut();
                let Some(s) = borrow.as_mut() else { return };
                s.cursor_pos = (x, y);

                if let Some(action) = s.button_at(x, y) {
                    action_to_run = Some(action);
                    return;
                }

                let hit = s.hit_test_selection(x, y);
                if hit != DragMode::None {
                    s.drag_mode = hit;
                    s.drag_start = (x, y);
                    s.drag_orig_sel = s.selection.unwrap_or_default();
                    need_capture = true;
                    s.request_repaint();
                } else {
                    s.drag_mode = DragMode::Creating;
                    s.drag_start = (x, y);
                    s.selection = None;
                    need_capture = true;
                    s.request_repaint();
                }
            });
            if need_capture {
                unsafe {
                    SetCapture(hwnd);
                }
            }
            if let Some(action) = action_to_run {
                finish_with_action(action);
            }
            return LRESULT(0);
        }
        WM_LBUTTONUP => {
            let (x, y) = lparam_xy(lparam);
            let mut need_release = false;
            ACTIVE_OVERLAY.with(|cell| {
                let mut borrow = cell.borrow_mut();
                let Some(s) = borrow.as_mut() else { return };
                if s.drag_mode == DragMode::None {
                    return;
                }
                need_release = true;
                let was_creating = s.drag_mode == DragMode::Creating;
                s.drag_mode = DragMode::None;
                s.cursor_pos = (x, y);

                if was_creating {
                    let moved = (x - s.drag_start.0).abs().max((y - s.drag_start.1).abs());
                    if moved <= 3 {
                        // Single-click selects hovered smart window!
                        if let Some(hr) = s.hover_rect {
                            s.selection = Some(hr);
                        }
                    } else {
                        let r = RectI::new(s.drag_start.0, s.drag_start.1, x, y)
                            .normalized()
                            .intersect(s.bounds());
                        if r.width() >= 2 && r.height() >= 2 {
                            s.selection = Some(r);
                        }
                    }
                }
                s.hovered_button = s.button_at(x, y);
                s.request_repaint();
            });
            if need_release {
                unsafe {
                    let _ = ReleaseCapture();
                }
            }
            return LRESULT(0);
        }
        WM_LBUTTONDBLCLK => {
            let (x, y) = lparam_xy(lparam);
            let inside = ACTIVE_OVERLAY.with(|cell| {
                cell.borrow()
                    .as_ref()
                    .and_then(|s| s.selection)
                    .is_some_and(|r| r.contains(x, y))
            });
            if inside {
                finish_with_action(ToolbarAction::Copy);
            }
            return LRESULT(0);
        }
        WM_RBUTTONUP => {
            let should_cancel = ACTIVE_OVERLAY.with(|cell| {
                let mut borrow = cell.borrow_mut();
                let Some(s) = borrow.as_mut() else {
                    return false;
                };
                if s.selection.is_some() {
                    // Right-click once clears selection so user can re-select
                    s.selection = None;
                    s.drag_mode = DragMode::None;
                    s.hovered_button = None;
                    s.update_hover_rect();
                    s.request_repaint();
                    false
                } else {
                    true
                }
            });
            if should_cancel {
                finish_with_outcome(OverlayOutcome::Cancel);
            }
            return LRESULT(0);
        }
        0x010D /* WM_IME_STARTCOMPOSITION */ => return LRESULT(0),
        WM_KEYDOWN => {
            let vk = resolve_vk(wparam, lparam);
            let ctrl = key_down(VK_CONTROL.0);
            let shift = key_down(VK_SHIFT.0);
            let alt = key_down(VK_MENU.0);

            if vk == VK_ESCAPE.0 {
                finish_with_outcome(OverlayOutcome::Cancel);
                return LRESULT(0);
            }
            if vk == VK_RETURN.0 {
                finish_with_action(ToolbarAction::Copy);
                return LRESULT(0);
            }
            if vk == VK_F3.0 || vk == b'T' as u16 {
                finish_with_action(ToolbarAction::Pin);
                return LRESULT(0);
            }
            if ctrl && vk == b'S' as u16 {
                finish_with_action(ToolbarAction::Save);
                return LRESULT(0);
            }
            if ctrl && vk == b'C' as u16 {
                finish_with_action(ToolbarAction::Copy);
                return LRESULT(0);
            }
            if ctrl && vk == b'A' as u16 {
                ACTIVE_OVERLAY.with(|cell| {
                    if let Some(s) = cell.borrow_mut().as_mut() {
                        let mon = s.monitor_for_point(s.cursor_pos.0, s.cursor_pos.1);
                        s.selection = Some(mon);
                        s.request_repaint();
                    }
                });
                return LRESULT(0);
            }
            if !ctrl && vk == b'C' as u16 {
                let hex = ACTIVE_OVERLAY.with(|cell| {
                    let borrow = cell.borrow();
                    let s = borrow.as_ref()?;
                    if s.selection.is_some() {
                        return None;
                    }
                    let (r, g, b) = s.capture.image.pixel_rgb(s.cursor_pos.0, s.cursor_pos.1)?;
                    Some(format!("#{:02X}{:02X}{:02X}", r, g, b))
                });
                if let Some(hex) = hex {
                    finish_with_outcome(OverlayOutcome::CopyColor(hex));
                    return LRESULT(0);
                }
            }

            // Pixel-level adjustment with arrow keys
            if matches!(vk, x if x == VK_LEFT.0 || x == VK_RIGHT.0 || x == VK_UP.0 || x == VK_DOWN.0) {
                let step = if alt { 10 } else { 1 };
                ACTIVE_OVERLAY.with(|cell| {
                    let mut borrow = cell.borrow_mut();
                    let Some(s) = borrow.as_mut() else { return };
                    let bounds = s.bounds();
                    let Some(mut sel) = s.selection else { return };

                    if ctrl {
                        // Expand edge
                        if vk == VK_LEFT.0 { sel.left = (sel.left - step).max(0); }
                        if vk == VK_RIGHT.0 { sel.right = (sel.right + step).min(bounds.right); }
                        if vk == VK_UP.0 { sel.top = (sel.top - step).max(0); }
                        if vk == VK_DOWN.0 { sel.bottom = (sel.bottom + step).min(bounds.bottom); }
                    } else if shift {
                        // Shrink edge
                        if vk == VK_LEFT.0 { sel.right = (sel.right - step).max(sel.left + 1); }
                        if vk == VK_RIGHT.0 { sel.left = (sel.left + step).min(sel.right - 1); }
                        if vk == VK_UP.0 { sel.bottom = (sel.bottom - step).max(sel.top + 1); }
                        if vk == VK_DOWN.0 { sel.top = (sel.top + step).min(sel.bottom - 1); }
                    } else {
                        // Move whole selection
                        let dx = if vk == VK_LEFT.0 { -step } else if vk == VK_RIGHT.0 { step } else { 0 };
                        let dy = if vk == VK_UP.0 { -step } else if vk == VK_DOWN.0 { step } else { 0 };
                        let w = sel.width();
                        let h = sel.height();
                        let nl = (sel.left + dx).clamp(0, (bounds.right - w).max(0));
                        let nt = (sel.top + dy).clamp(0, (bounds.bottom - h).max(0));
                        sel = RectI::new(nl, nt, nl + w, nt + h);
                    }
                    s.selection = Some(sel);
                    s.request_repaint();
                });
                return LRESULT(0);
            }
        }
        WM_DESTROY => {}
        WM_NCDESTROY => {
            // Free full-screen capture & DIB buffer immediately!
            ACTIVE_OVERLAY.with(|cell| {
                *cell.borrow_mut() = None;
            });
        }
        _ => {}
    }
    unsafe { DefWindowProcW(hwnd, msg, wparam, lparam) }
}

fn finish_with_action(action: ToolbarAction) {
    let extracted = ACTIVE_OVERLAY.with(|cell| {
        let mut state = cell.borrow_mut().take()?;
        let outcome = state.compute_action_outcome(action);
        let cb = state.close_window_and_take_callback();
        // Drop full-screen capture & DIB surface BEFORE running callback (e.g. Save dialog)
        drop(state);
        Some((outcome, cb))
    });
    trim_working_set();
    if let Some((outcome, Some(cb))) = extracted {
        cb(outcome);
    }
}

fn finish_with_outcome(outcome: OverlayOutcome) {
    let cb = ACTIVE_OVERLAY.with(|cell| {
        let mut state = cell.borrow_mut().take()?;
        let cb = state.close_window_and_take_callback();
        drop(state);
        cb
    });
    trim_working_set();
    if let Some(cb) = cb {
        cb(outcome);
    }
}

#[cfg(test)]
mod visual_tests {
    use super::*;
    use crate::config::SaveFormat;
    use std::path::PathBuf;

    #[test]
    #[ignore]
    fn render_overlay_visual_snapshot() {
        unsafe {
            let _ = windows::Win32::UI::HiDpi::SetProcessDpiAwarenessContext(
                windows::Win32::UI::HiDpi::DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2,
            );
        }
        let out_dir = std::env::var("RHPICTURE_SNAPSHOT_DIR")
            .map(PathBuf::from)
            .unwrap_or_else(|_| std::env::temp_dir());

        // Synthesize a realistic 225%-DPI desktop frame (1920x1080 region with a light editor card + subtle grid)
        let w = 1920u32;
        let h = 1080u32;
        let mut data = vec![0u8; (w * h * 4) as usize];
        for y in 0..h {
            for x in 0..w {
                let idx = ((y * w + x) * 4) as usize;
                // Soft gradient desktop wallpaper
                let r = (24 + (x * 26 / w)) as u8;
                let g = (38 + (y * 34 / h)) as u8;
                let b = (66 + ((x + y) * 32 / (w + h))) as u8;
                // Mock bright application window inside selection area
                let in_win = x >= 260 && x < 1640 && y >= 150 && y < 930;
                let in_title = in_win && y < 220;
                let in_card = x >= 340 && x < 1180 && y >= 270 && y < 740;
                let (cr, cg, cb) = if in_title {
                    (241u8, 245u8, 249u8)
                } else if in_card {
                    if ((x / 40) + (y / 40)) % 2 == 0 {
                        (255u8, 255u8, 255u8)
                    } else {
                        (246u8, 250u8, 255u8)
                    }
                } else if in_win {
                    (248u8, 250u8, 252u8)
                } else {
                    (r, g, b)
                };
                data[idx] = cb;
                data[idx + 1] = cg;
                data[idx + 2] = cr;
                data[idx + 3] = 255;
            }
        }
        let img = BgraImage::from_bgra(w, h, data).unwrap();
        let dpi = 216u32; // 225% real user monitor DPI
        let capture = ScreenCapture {
            image: img,
            origin_x: 0,
            origin_y: 0,
            monitors: vec![MonitorInfo {
                rect: RectI::new(0, 0, w as i32, h as i32),
                dpi,
            }],
            window_rects: Vec::new(),
        };
        let surface = DibSurface::new(w as i32, h as i32).unwrap();
        let mut fonts = HashMap::new();
        for d in [dpi, 96] {
            fonts.insert(
                d,
                (
                    OwnedFont(create_ui_font(d, 10, false)),
                    OwnedFont(create_ui_font(d, 9, false)),
                ),
            );
        }
        let mut state = OverlayState {
            hwnd: HWND::default(),
            capture,
            gpu: None,
            surface: Some(surface),
            dimmed_bg: None,
            fonts,
            show_magnifier: true,
            smart_window_detect: true,
            dark_theme: false,
            dirty: true,
            selection: Some(RectI::new(380, 220, 1280, 740)),
            hover_rect: None,
            drag_mode: DragMode::None,
            drag_start: (0, 0),
            drag_orig_sel: RectI::default(),
            cursor_pos: (1260, 420),
            toolbar_rect: RectI::default(),
            toolbar_dpi: dpi,
            buttons: Vec::new(),
            hovered_button: Some(ToolbarAction::Copy),
            on_finish: None,
            finished: false,
        };
        state.render();
        let mut pix1 = state.surface.as_mut().unwrap().pixels_mut().to_vec();
        for px in pix1.chunks_exact_mut(4) {
            px[3] = 255;
        }
        let out = BgraImage::from_bgra(w, h, pix1).unwrap();
        out.save_to_file(&out_dir.join("overlay-light-selected.png"), SaveFormat::Png)
            .unwrap();

        // Also render hover / pre-selection state with L-corner viewfinder + top hint pill + magnifier!
        state.selection = None;
        state.hover_rect = Some(RectI::new(340, 270, 1180, 740));
        state.cursor_pos = (880, 460);
        state.render();
        let mut pix2 = state.surface.as_mut().unwrap().pixels_mut().to_vec();
        for px in pix2.chunks_exact_mut(4) {
            px[3] = 255;
        }
        let out2 = BgraImage::from_bgra(w, h, pix2).unwrap();
        out2.save_to_file(&out_dir.join("overlay-light-hover.png"), SaveFormat::Png)
            .unwrap();
    }

    #[test]
    fn gpu_overlay_initializes_and_renders_fast() {
        ensure_class_registered();
        let class_w = to_wide(OVERLAY_CLASS);
        let w = 3840u32;
        let h = 2160u32;
        let Ok(hwnd) = (unsafe {
            CreateWindowExW(
                WS_EX_TOOLWINDOW,
                PCWSTR(class_w.as_ptr()),
                PCWSTR::null(),
                WS_POPUP,
                0,
                0,
                w as i32,
                h as i32,
                None,
                None,
                Some(current_hinstance()),
                None,
            )
        }) else {
            return;
        };

        let img = BgraImage::new_solid(w, h, 240, 244, 248, 255);
        let gpu = GpuOverlay::new(hwnd, &img);
        assert!(gpu.is_some(), "Direct2D GPU overlay should initialize on Windows desktop");

        let mut fonts = HashMap::new();
        for d in [216u32, 96u32] {
            fonts.insert(
                d,
                (
                    OwnedFont(create_ui_font(d, 10, false)),
                    OwnedFont(create_ui_font(d, 9, false)),
                ),
            );
        }
        let mut state = OverlayState {
            hwnd,
            capture: ScreenCapture {
                image: img,
                origin_x: 0,
                origin_y: 0,
                monitors: vec![MonitorInfo {
                    rect: RectI::new(0, 0, w as i32, h as i32),
                    dpi: 216,
                }],
                window_rects: Vec::new(),
            },
            gpu,
            surface: None,
            dimmed_bg: None,
            fonts,
            show_magnifier: true,
            smart_window_detect: true,
            dark_theme: false,
            dirty: true,
            selection: Some(RectI::new(400, 300, 1400, 900)),
            hover_rect: None,
            drag_mode: DragMode::Creating,
            drag_start: (400, 300),
            drag_orig_sel: RectI::default(),
            cursor_pos: (1400, 900),
            toolbar_rect: RectI::default(),
            toolbar_dpi: 216,
            buttons: Vec::new(),
            hovered_button: None,
            on_finish: None,
            finished: false,
        };

        // Warmup 1 frame, then benchmark 20 consecutive drag frames at 4K (3840x2160)
        assert!(state.render_gpu());
        let t0 = std::time::Instant::now();
        for i in 0..20 {
            state.selection = Some(RectI::new(400, 300, 1400 + i * 20, 900 + i * 12));
            state.cursor_pos = (1400 + i * 20, 900 + i * 12);
            assert!(state.render_gpu());
        }
        let avg_ms = t0.elapsed().as_secs_f64() * 1000.0 / 20.0;
        println!("Direct2D GPU overlay 4K drag avg: {avg_ms:.2} ms/frame ({:.0} FPS)", 1000.0 / avg_ms.max(0.01));
        assert!(state.surface.is_none(), "GPU path must not allocate 32-88MB CPU fallback DIB");

        drop(state);
        unsafe {
            let _ = DestroyWindow(hwnd);
        }
    }
}
