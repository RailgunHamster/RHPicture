use crate::capture::{MonitorInfo, RectI, ScreenCapture, capture_virtual_screen};
use crate::image_ops::BgraImage;
use crate::util::{
    DibSurface, OwnedFont, create_ui_font, current_hinstance, rgb_colorref, scale_dpi, to_wide,
    trim_working_set,
};
use std::cell::RefCell;
use std::collections::HashMap;
use std::sync::Once;
use tiny_skia::{
    Color, FillRule, GradientStop, LineCap, LineJoin, LinearGradient, Paint, PathBuilder, Pixmap,
    Point, SpreadMode, Stroke, Transform,
};
use windows::Win32::Foundation::{HWND, LPARAM, LRESULT, RECT, WPARAM};
use windows::Win32::Graphics::Gdi::{
    BeginPaint, BitBlt, DT_CENTER, DT_LEFT, DT_NOCLIP, DT_SINGLELINE, DT_VCENTER, DrawTextW,
    EndPaint, InvalidateRect, PAINTSTRUCT, SRCCOPY, SelectObject, SetBkMode, SetTextColor,
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
    WNDCLASSEXW, WS_EX_TOOLWINDOW, WS_EX_TOPMOST, WS_POPUP, WS_VISIBLE, HWND_TOPMOST,
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

struct OverlayState {
    hwnd: HWND,
    capture: ScreenCapture,
    surface: DibSurface,
    /// Per-DPI cached (ui_10pt, small_9pt) ClearType fonts so mixed-DPI monitors render sharply.
    fonts: HashMap<u32, (OwnedFont, OwnedFont)>,
    show_magnifier: bool,
    smart_window_detect: bool,

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

    let surface = DibSurface::new(width, height).ok_or_else(|| "无法创建绘制缓冲".to_string())?;
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
        surface,
        fonts,
        show_magnifier,
        smart_window_detect,
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
    state.render();

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
            WS_POPUP | WS_VISIBLE,
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

    ACTIVE_OVERLAY.with(|cell| {
        if let Some(s) = cell.borrow_mut().as_mut() {
            s.hwnd = hwnd;
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

    fn render(&mut self) {
        self.layout_toolbar();

        let width = self.capture.image.width as usize;
        let height = self.capture.image.height as usize;
        let src = &self.capture.image.data;
        let active_rect = self.selection.or(self.hover_rect);

        // 1. Blit full screen dimmed + bright cutout inside active selection/hover rect
        let bounds = self.bounds();
        {
            let dst = self.surface.pixels_mut();
            // Fast dimmed fill (~48% brightness)
            for (s, d) in src.chunks_exact(4).zip(dst.chunks_exact_mut(4)) {
                d[0] = ((s[0] as u16 * 124) >> 8) as u8;
                d[1] = ((s[1] as u16 * 124) >> 8) as u8;
                d[2] = ((s[2] as u16 * 124) >> 8) as u8;
                d[3] = 255;
            }

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
        let w = self.surface.width as usize;
        let dst = self.surface.pixels_mut();
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
        let sw = self.surface.width;
        let sh = self.surface.height;
        let src = pixmap.data(); // premultiplied RGBA
        let dst = self.surface.pixels_mut();

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
        let wide = to_wide(text);
        let mut r = RECT {
            left: rect.left,
            top: rect.top,
            right: rect.right,
            bottom: rect.bottom,
        };
        unsafe {
            let font = self.font_for_dpi(dpi, small);
            let old = SelectObject(self.surface.dc, windows::Win32::Graphics::Gdi::HGDIOBJ(font.0));
            SetBkMode(self.surface.dc, TRANSPARENT);
            SetTextColor(self.surface.dc, rgb_colorref(color.0, color.1, color.2));
            let flags = if centered {
                DT_CENTER | DT_VCENTER | DT_SINGLELINE | DT_NOCLIP
            } else {
                DT_LEFT | DT_VCENTER | DT_SINGLELINE | DT_NOCLIP
            };
            let _ = DrawTextW(
                self.surface.dc,
                &mut wide[..wide.len() - 1].to_vec(),
                &mut r,
                flags,
            );
            let _ = SelectObject(self.surface.dc, old);
        }
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

        if let Some(mut pm) =
            Pixmap::new((pill_w + shadow * 2) as u32, (pill_h + shadow * 2) as u32)
        {
            let ox = shadow as f32;
            let oy = shadow as f32;
            draw_soft_shadow(&mut pm, ox, oy, pill_w as f32, pill_h as f32, 15.0 * sf, sf);
            fill_rounded_rect(
                &mut pm,
                ox,
                oy,
                pill_w as f32,
                pill_h as f32,
                15.0 * sf,
                Color::from_rgba8(15, 23, 42, 226),
            );
            stroke_rounded_rect(
                &mut pm,
                ox + 0.5 * sf,
                oy + 0.5 * sf,
                pill_w as f32 - sf,
                pill_h as f32 - sf,
                14.5 * sf,
                1.0 * sf,
                Color::from_rgba8(255, 255, 255, 36),
            );
            self.composite_pixmap(px - shadow, py - shadow, &pm);
        }
        self.draw_text_on_surface(
            text,
            RectI::new(px, py, px + pill_w, py + pill_h),
            (226, 232, 240),
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

        if let Some(mut pm) =
            Pixmap::new((badge_w + shadow * 2) as u32, (badge_h + shadow * 2) as u32)
        {
            let ox = shadow as f32;
            let oy = shadow as f32;
            draw_soft_shadow(&mut pm, ox, oy, badge_w as f32, badge_h as f32, 8.0 * sf, sf);
            fill_rounded_rect(
                &mut pm,
                ox,
                oy,
                badge_w as f32,
                badge_h as f32,
                8.0 * sf,
                Color::from_rgba8(15, 23, 42, 234),
            );
            stroke_rounded_rect(
                &mut pm,
                ox + 0.5 * sf,
                oy + 0.5 * sf,
                badge_w as f32 - sf,
                badge_h as f32 - sf,
                7.5 * sf,
                1.0 * sf,
                Color::from_rgba8(255, 255, 255, 36),
            );
            // Tiny glowing sky-cyan status dot on left of dimensions
            if let Some(dot) = PathBuilder::from_circle(
                ox + 11.0 * sf,
                oy + (badge_h as f32) * 0.5,
                3.0 * sf,
            ) {
                let mut paint = Paint::default();
                paint.set_color(Color::from_rgba8(56, 189, 248, 255));
                paint.anti_alias = true;
                pm.fill_path(&dot, &paint, FillRule::Winding, Transform::identity(), None);
            }
            self.composite_pixmap(bx - shadow, by - shadow, &pm);
        }
        self.draw_text_on_surface(
            &text,
            RectI::new(bx + s(16), by, bx + badge_w - s(6), by + badge_h),
            (248, 250, 252),
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

        // Dark frosted glass dock surface
        fill_rounded_rect(
            &mut pm,
            ox,
            oy,
            tw as f32,
            th as f32,
            12.0 * sf,
            Color::from_rgba8(15, 23, 42, 240),
        );
        stroke_rounded_rect(
            &mut pm,
            ox + 0.5 * sf,
            oy + 0.5 * sf,
            tw as f32 - sf,
            th as f32 - sf,
            11.5 * sf,
            1.0 * sf,
            Color::from_rgba8(255, 255, 255, 38),
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
                Color::from_rgba8(255, 255, 255, 32),
            );
        }

        for btn in &self.buttons {
            let lx = ox + (btn.rect.left - tb.left) as f32;
            let ly = oy + (btn.rect.top - tb.top) as f32;
            let bw = btn.rect.width() as f32;
            let bh = btn.rect.height() as f32;
            let hovered = self.hovered_button == Some(btn.action);

            if btn.action == ToolbarAction::Copy {
                // Vibrant sky->blue gradient primary action pill
                let (c0, c1) = if hovered {
                    (
                        Color::from_rgba8(56, 189, 248, 255),
                        Color::from_rgba8(37, 99, 235, 255),
                    )
                } else {
                    (
                        Color::from_rgba8(14, 165, 233, 240),
                        Color::from_rgba8(37, 99, 235, 240),
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
                    (ToolbarAction::Cancel, true) => Some(Color::from_rgba8(239, 68, 68, 195)),
                    (_, true) => Some(Color::from_rgba8(255, 255, 255, 30)),
                    _ => None,
                };
                if let Some(c) = bg {
                    fill_rounded_rect(&mut pm, lx, ly, bw, bh, 8.0 * sf, c);
                }
            }

            let ix = lx + 11.0 * sf;
            let iy = ly + (bh - 14.0 * sf) * 0.5;
            draw_toolbar_icon(&mut pm, btn.action, ix, iy, sf);
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
            let txt_color = if btn.action == ToolbarAction::Cancel && self.hovered_button != Some(ToolbarAction::Cancel) {
                (203, 213, 225)
            } else {
                (248, 250, 252)
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

        // Rounded floating card with soft shadow
        if let Some(mut pm) =
            Pixmap::new((total_w + shadow * 2) as u32, (total_h + shadow * 2) as u32)
        {
            let ox = shadow as f32;
            let oy = shadow as f32;
            draw_soft_shadow(&mut pm, ox, oy, total_w as f32, total_h as f32, 12.0 * sf, sf);
            fill_rounded_rect(
                &mut pm,
                ox,
                oy,
                total_w as f32,
                total_h as f32,
                12.0 * sf,
                Color::from_rgba8(15, 23, 42, 240),
            );
            stroke_rounded_rect(
                &mut pm,
                ox + 0.5 * sf,
                oy + 0.5 * sf,
                total_w as f32 - sf,
                total_h as f32 - sf,
                11.5 * sf,
                1.0 * sf,
                Color::from_rgba8(255, 255, 255, 42),
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
                ring.set_color(Color::from_rgba8(255, 255, 255, 200));
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
            (248, 250, 252),
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
            (148, 163, 184),
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
        self.render();
        unsafe {
            let _ = InvalidateRect(Some(self.hwnd), None, false);
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

fn draw_toolbar_icon(pm: &mut Pixmap, action: ToolbarAction, x: f32, y: f32, scale: f32) {
    let mut paint = Paint::default();
    paint.set_color(Color::from_rgba8(248, 250, 252, 255));
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
                if let Some(s) = cell.borrow().as_ref() {
                    let _ = unsafe {
                        BitBlt(
                            hdc,
                            0,
                            0,
                            s.surface.width,
                            s.surface.height,
                            Some(s.surface.dc),
                            0,
                            0,
                            SRCCOPY,
                        )
                    };
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
        WM_KEYDOWN => {
            let vk = wparam.0 as u16;
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
            surface,
            fonts,
            show_magnifier: true,
            smart_window_detect: true,
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
        let mut pix1 = state.surface.pixels_mut().to_vec();
        for px in pix1.chunks_exact_mut(4) {
            px[3] = 255;
        }
        let out = BgraImage::from_bgra(w, h, pix1).unwrap();
        out.save_to_file(&out_dir.join("overlay-selected.png"), SaveFormat::Png)
            .unwrap();

        // Also render hover / pre-selection state with L-corner viewfinder + top hint pill + magnifier!
        state.selection = None;
        state.hover_rect = Some(RectI::new(340, 270, 1180, 740));
        state.cursor_pos = (880, 460);
        state.render();
        let mut pix2 = state.surface.pixels_mut().to_vec();
        for px in pix2.chunks_exact_mut(4) {
            px[3] = 255;
        }
        let out2 = BgraImage::from_bgra(w, h, pix2).unwrap();
        out2.save_to_file(&out_dir.join("overlay-hover.png"), SaveFormat::Png)
            .unwrap();
    }
}
