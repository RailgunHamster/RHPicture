use crate::capture::{RectI, ScreenCapture, capture_virtual_screen};
use crate::image_ops::BgraImage;
use crate::util::{
    DibSurface, OwnedFont, create_ui_font, current_hinstance, rgb_colorref, to_wide,
    trim_working_set,
};
use std::cell::RefCell;
use std::sync::Once;
use tiny_skia::{Color, FillRule, LineCap, LineJoin, Paint, PathBuilder, Pixmap, Stroke, Transform};
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
    font_ui: OwnedFont,
    font_small: OwnedFont,
    show_magnifier: bool,
    smart_window_detect: bool,

    selection: Option<RectI>,
    hover_rect: Option<RectI>,
    drag_mode: DragMode,
    drag_start: (i32, i32),
    drag_orig_sel: RectI,
    cursor_pos: (i32, i32),

    toolbar_rect: RectI,
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
    let font_ui = OwnedFont(create_ui_font(96, 10, false));
    let font_small = OwnedFont(create_ui_font(96, 9, false));

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
        font_ui,
        font_small,
        show_magnifier,
        smart_window_detect,
        selection: None,
        hover_rect: None,
        drag_mode: DragMode::None,
        drag_start: cursor_local,
        drag_orig_sel: RectI::default(),
        cursor_pos: cursor_local,
        toolbar_rect: RectI::default(),
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

    fn monitor_for_point(&self, x: i32, y: i32) -> RectI {
        self.capture
            .monitors
            .iter()
            .copied()
            .find(|m| m.contains(x, y))
            .unwrap_or_else(|| self.bounds())
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
        let tol = 7;
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

        let btn_w = 72;
        let btn_h = 32;
        let gap = 4;
        let pad = 6;
        let items = [
            (ToolbarAction::Pin, "贴图"),
            (ToolbarAction::Save, "保存"),
            (ToolbarAction::Copy, "复制"),
            (ToolbarAction::Cancel, "取消"),
        ];
        let total_w = pad * 2 + (items.len() as i32) * btn_w + ((items.len() as i32) - 1) * gap;
        let total_h = pad * 2 + btn_h;

        let mon = self.monitor_for_point((sel.left + sel.right) / 2, (sel.top + sel.bottom) / 2);
        let margin = 8;

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
        for (i, (action, label)) in items.iter().enumerate() {
            let bx = tx + pad + (i as i32) * (btn_w + gap);
            let by = ty + pad;
            self.buttons.push(ToolbarButton {
                action: *action,
                rect: RectI::new(bx, by, bx + btn_w, by + btn_h),
                label,
            });
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

        // 2. Draw border & handles
        if let Some(r) = active_rect {
            let r = r.intersect(self.bounds());
            if !r.is_empty() {
                let is_confirmed = self.selection.is_some();
                let color = if is_confirmed {
                    [246, 130, 59, 255] // BGRA for #3B82F6
                } else {
                    [235, 190, 56, 255] // lighter cyan-blue for hover preview
                };
                let thickness = if is_confirmed { 2 } else { 3 };
                self.draw_rect_outline(r, thickness, color);

                if is_confirmed && self.drag_mode == DragMode::None {
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
                    for (hx, hy) in pts {
                        self.fill_rect_solid(
                            RectI::new(hx - 4, hy - 4, hx + 5, hy + 5),
                            [246, 130, 59, 255],
                        );
                        self.fill_rect_solid(
                            RectI::new(hx - 2, hy - 2, hx + 3, hy + 3),
                            [255, 255, 255, 255],
                        );
                    }
                }

                // Size badge above top-left of active rect
                self.draw_size_badge(r);
            }
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
            let font = if small {
                self.font_small.0
            } else {
                self.font_ui.0
            };
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

    fn draw_size_badge(&mut self, sel: RectI) {
        let text = format!("{} × {}", sel.width(), sel.height());
        let badge_w = 96.max((text.len() as i32) * 8 + 16);
        let badge_h = 24;
        let mon = self.monitor_for_point(sel.left, sel.top);
        let bx = sel.left.clamp(mon.left + 4, (mon.right - badge_w - 4).max(mon.left + 4));
        let by = if sel.top - badge_h - 6 >= mon.top + 4 {
            sel.top - badge_h - 6
        } else {
            sel.top + 6
        };

        if let Some(mut pm) = Pixmap::new(badge_w as u32, badge_h as u32) {
            fill_rounded_rect(
                &mut pm,
                0.0,
                0.0,
                badge_w as f32,
                badge_h as f32,
                6.0,
                Color::from_rgba8(20, 24, 33, 224),
            );
            self.composite_pixmap(bx, by, &pm);
        }
        self.draw_text_on_surface(
            &text,
            RectI::new(bx, by, bx + badge_w, by + badge_h),
            (248, 250, 252),
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
        let Some(mut pm) = Pixmap::new(tw as u32, th as u32) else {
            return;
        };

        // Panel background with subtle border
        fill_rounded_rect(
            &mut pm,
            0.0,
            0.0,
            tw as f32,
            th as f32,
            9.0,
            Color::from_rgba8(24, 28, 38, 242),
        );
        stroke_rounded_rect(
            &mut pm,
            0.5,
            0.5,
            tw as f32 - 1.0,
            th as f32 - 1.0,
            8.5,
            1.0,
            Color::from_rgba8(255, 255, 255, 36),
        );

        for btn in &self.buttons {
            let lx = (btn.rect.left - tb.left) as f32;
            let ly = (btn.rect.top - tb.top) as f32;
            let bw = btn.rect.width() as f32;
            let bh = btn.rect.height() as f32;
            let hovered = self.hovered_button == Some(btn.action);

            let bg = match (btn.action, hovered) {
                (ToolbarAction::Copy, true) => Some(Color::from_rgba8(37, 99, 235, 255)),
                (ToolbarAction::Copy, false) => Some(Color::from_rgba8(37, 99, 235, 195)),
                (ToolbarAction::Cancel, true) => Some(Color::from_rgba8(220, 38, 38, 210)),
                (_, true) => Some(Color::from_rgba8(255, 255, 255, 42)),
                _ => None,
            };
            if let Some(c) = bg {
                fill_rounded_rect(&mut pm, lx, ly, bw, bh, 6.0, c);
            }

            // Draw crisp vector icon on left of button
            let ix = lx + 10.0;
            let iy = ly + (bh - 14.0) * 0.5;
            draw_toolbar_icon(&mut pm, btn.action, ix, iy);
        }

        self.composite_pixmap(tb.left, tb.top, &pm);

        // Draw crisp labels via GDI ClearType
        for btn in &self.buttons {
            let text_rect = RectI::new(
                btn.rect.left + 26,
                btn.rect.top,
                btn.rect.right - 6,
                btn.rect.bottom,
            );
            self.draw_text_on_surface(btn.label, text_rect, (248, 250, 252), false, true);
        }
    }

    fn draw_magnifier(&mut self) {
        let (cx, cy) = self.cursor_pos;
        let sample_w = 17;
        let sample_h = 11;
        let zoom = 8;
        let view_w = sample_w * zoom;
        let view_h = sample_h * zoom;
        let info_h = 44;
        let pad = 2;
        let total_w = view_w + pad * 2;
        let total_h = view_h + info_h + pad * 2;

        let mon = self.monitor_for_point(cx, cy);
        let mut mx = cx + 22;
        let mut my = cy + 22;
        if mx + total_w > mon.right - 8 {
            mx = cx - 22 - total_w;
        }
        if my + total_h > mon.bottom - 8 {
            my = cy - 22 - total_h;
        }
        mx = mx.clamp(mon.left + 4, (mon.right - total_w - 4).max(mon.left + 4));
        my = my.clamp(mon.top + 4, (mon.bottom - total_h - 4).max(mon.top + 4));

        // Frame background
        self.fill_rect_solid(
            RectI::new(mx, my, mx + total_w, my + total_h),
            [38, 28, 24, 255],
        );

        // Zoomed pixels
        let half_x = sample_w / 2;
        let half_y = sample_h / 2;
        let img_w = self.capture.image.width as i32;
        let img_h = self.capture.image.height as i32;
        for sy_off in 0..sample_h {
            for sx_off in 0..sample_w {
                let px = cx - half_x + sx_off;
                let py = cy - half_y + sy_off;
                let bgra = if px >= 0 && py >= 0 && px < img_w && py < img_h {
                    let idx = ((py as u32 * self.capture.image.width + px as u32) * 4) as usize;
                    let d = &self.capture.image.data;
                    [d[idx], d[idx + 1], d[idx + 2], 255]
                } else {
                    [20, 20, 20, 255]
                };
                let dx = mx + pad + sx_off * zoom;
                let dy = my + pad + sy_off * zoom;
                self.fill_rect_solid(RectI::new(dx, dy, dx + zoom, dy + zoom), bgra);
            }
        }

        // Crosshair highlighting center pixel
        let center_x = mx + pad + half_x * zoom;
        let center_y = my + pad + half_y * zoom;
        let accent = [246, 130, 59, 255];
        self.fill_rect_solid(
            RectI::new(mx + pad, center_y + zoom / 2, mx + pad + view_w, center_y + zoom / 2 + 1),
            [246, 130, 59, 255],
        );
        self.fill_rect_solid(
            RectI::new(center_x + zoom / 2, my + pad, center_x + zoom / 2 + 1, my + pad + view_h),
            [246, 130, 59, 255],
        );
        self.draw_rect_outline(
            RectI::new(center_x - 1, center_y - 1, center_x + zoom + 1, center_y + zoom + 1),
            1,
            [255, 255, 255, 255],
        );
        self.draw_rect_outline(
            RectI::new(mx, my, mx + total_w, my + total_h),
            1,
            accent,
        );

        let (r, g, b) = self.capture.image.pixel_rgb(cx, cy).unwrap_or((0, 0, 0));
        let swatch = RectI::new(
            mx + 8,
            my + pad + view_h + 7,
            mx + 22,
            my + pad + view_h + 21,
        );
        self.fill_rect_solid(swatch, [b, g, r, 255]);
        self.draw_rect_outline(swatch, 1, [220, 220, 220, 255]);

        let hex_line = format!("#{:02X}{:02X}{:02X}  ({},{})", r, g, b, cx, cy);
        self.draw_text_on_surface(
            &hex_line,
            RectI::new(mx + 28, my + pad + view_h + 4, mx + total_w - 4, my + pad + view_h + 22),
            (241, 245, 249),
            true,
            false,
        );
        self.draw_text_on_surface(
            "按 C 复制色值  |  右键取消",
            RectI::new(mx + 8, my + pad + view_h + 22, mx + total_w - 4, my + total_h - 4),
            (148, 163, 184),
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

fn draw_toolbar_icon(pm: &mut Pixmap, action: ToolbarAction, x: f32, y: f32) {
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
            // Simple pushpin icon
            pb.move_to(x + 4.0, y + 2.0);
            pb.line_to(x + 10.0, y + 2.0);
            pb.line_to(x + 9.0, y + 6.5);
            pb.line_to(x + 11.5, y + 8.5);
            pb.line_to(x + 2.5, y + 8.5);
            pb.line_to(x + 5.0, y + 6.5);
            pb.close();
            pb.move_to(x + 7.0, y + 8.5);
            pb.line_to(x + 7.0, y + 13.0);
        }
        ToolbarAction::Save => {
            // Down-arrow into tray
            pb.move_to(x + 7.0, y + 1.5);
            pb.line_to(x + 7.0, y + 9.5);
            pb.move_to(x + 4.0, y + 6.8);
            pb.line_to(x + 7.0, y + 9.8);
            pb.line_to(x + 10.0, y + 6.8);
            pb.move_to(x + 2.0, y + 10.0);
            pb.line_to(x + 2.0, y + 12.5);
            pb.line_to(x + 12.0, y + 12.5);
            pb.line_to(x + 12.0, y + 10.0);
        }
        ToolbarAction::Copy => {
            // Crisp checkmark
            pb.move_to(x + 2.0, y + 7.5);
            pb.line_to(x + 5.5, y + 11.0);
            pb.line_to(x + 12.2, y + 3.2);
        }
        ToolbarAction::Cancel => {
            // Crisp X
            pb.move_to(x + 3.0, y + 3.0);
            pb.line_to(x + 11.0, y + 11.0);
            pb.move_to(x + 11.0, y + 3.0);
            pb.line_to(x + 3.0, y + 11.0);
        }
    }
    if let Some(path) = pb.finish() {
        pm.stroke_path(&path, &paint, &stroke, Transform::identity(), None);
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
