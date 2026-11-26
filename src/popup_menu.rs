//! Modern Windows 11 Fluent popup context menu rendered purely via tiny-skia + GDI ClearType
//! (0 MB VRAM, Per-Monitor DPI scaled, Light & Dark theme support, native DWM rounded corners & shadow).

use crate::capture::RectI;
use crate::update_splash::draw_rhpicture_emblem;
use crate::util::{
    DibSurface, OwnedFont, create_ui_font, current_hinstance, rgb_colorref, scale_dpi, to_wide,
};
use std::cell::{Cell, RefCell};
use std::sync::Once;
use tiny_skia::{
    Color, FillRule, LineCap, LineJoin, Paint, PathBuilder, Pixmap, Stroke, Transform,
};
use windows::Win32::Foundation::{HWND, LPARAM, LRESULT, POINT, RECT, SIZE, WPARAM};
use windows::Win32::Graphics::Dwm::{
    DWMWA_USE_IMMERSIVE_DARK_MODE, DWMWA_WINDOW_CORNER_PREFERENCE, DWMWCP_ROUND,
    DwmSetWindowAttribute,
};
use windows::Win32::Graphics::Gdi::{
    BeginPaint, BitBlt, DT_CENTER, DT_END_ELLIPSIS, DT_LEFT, DT_NOCLIP, DT_SINGLELINE, DT_VCENTER,
    DrawTextW, EndPaint, GetMonitorInfoW, GetTextExtentPoint32W, InvalidateRect,
    MONITOR_DEFAULTTONEAREST, MONITORINFOEXW, MonitorFromPoint, PAINTSTRUCT, SRCCOPY, SelectObject,
    SetBkMode, SetTextColor, TRANSPARENT,
};
use windows::Win32::UI::Controls::WM_MOUSELEAVE;
use windows::Win32::UI::HiDpi::{GetDpiForMonitor, MDT_EFFECTIVE_DPI};
use windows::Win32::UI::Input::KeyboardAndMouse::{
    TME_LEAVE, TRACKMOUSEEVENT, TrackMouseEvent, VK_DOWN, VK_ESCAPE, VK_RETURN, VK_UP,
};
use windows::Win32::UI::WindowsAndMessaging::{
    CreateWindowExW, DefWindowProcW, DestroyWindow, DispatchMessageW, GetMessageW, HWND_TOPMOST,
    IDC_ARROW, IDC_HAND, LoadCursorW, MSG, PostQuitMessage, RegisterClassExW, SW_SHOW,
    SWP_SHOWWINDOW, SetCursor, SetForegroundWindow, SetWindowPos, ShowWindow, TranslateMessage,
    WA_INACTIVE, WM_ACTIVATE, WM_ERASEBKGND, WM_KEYDOWN, WM_LBUTTONDOWN, WM_LBUTTONUP,
    WM_MOUSEMOVE, WM_PAINT, WM_RBUTTONUP, WM_SETCURSOR, WNDCLASSEXW, WS_EX_TOOLWINDOW,
    WS_EX_TOPMOST, WS_POPUP,
};
use windows::core::PCWSTR;

const POPUP_CLASS: &str = "RHPicturePopupMenuClass";

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum MenuIcon {
    None,
    Crop,
    Pin,
    EyeOff,
    Eye,
    Pointer,
    Close,
    Settings,
    Update,
    Power,
    Copy,
    Save,
    Reset,
    Rotate,
    Opacity,
}

#[derive(Clone, Debug)]
pub enum MenuEntry {
    Header {
        title: String,
        subtitle: String,
    },
    Action {
        id: u32,
        icon: MenuIcon,
        label: String,
        shortcut: String,
        enabled: bool,
        checked: bool,
        danger: bool,
    },
    Chips {
        icon: MenuIcon,
        label: String,
        items: Vec<(u32, String, bool)>,
    },
    Separator,
}

#[derive(Clone, Copy)]
struct MenuHit {
    rect: RectI,
    id: u32,
    enabled: bool,
}

struct MenuState {
    entries: Vec<MenuEntry>,
    dark: bool,
    dpi: u32,
    width: i32,
    height: i32,
    hovered_id: Cell<Option<u32>>,
    tracking_leave: Cell<bool>,
    chosen_id: Cell<Option<u32>>,
    closed: Cell<bool>,
    hits: RefCell<Vec<MenuHit>>,
    font_title: OwnedFont,
    font_item: OwnedFont,
    font_small: OwnedFont,
}

thread_local! {
    static ACTIVE_MENU: RefCell<Option<Box<MenuState>>> = const { RefCell::new(None) };
}

static REGISTER_CLASS: Once = Once::new();

fn ensure_registered() {
    REGISTER_CLASS.call_once(|| unsafe {
        let class_w = to_wide(POPUP_CLASS);
        let wc = WNDCLASSEXW {
            cbSize: std::mem::size_of::<WNDCLASSEXW>() as u32,
            lpfnWndProc: Some(menu_wnd_proc),
            hInstance: current_hinstance(),
            hCursor: LoadCursorW(None, IDC_ARROW).unwrap_or_default(),
            lpszClassName: PCWSTR(class_w.as_ptr()),
            ..Default::default()
        };
        let _ = RegisterClassExW(&wc);
    });
}

fn measure_text_px(dc: windows::Win32::Graphics::Gdi::HDC, font: windows::Win32::Graphics::Gdi::HFONT, text: &str) -> i32 {
    if text.is_empty() {
        return 0;
    }
    let wide = to_wide(text);
    let mut sz = SIZE::default();
    unsafe {
        let old = SelectObject(dc, windows::Win32::Graphics::Gdi::HGDIOBJ(font.0));
        let _ = GetTextExtentPoint32W(dc, &wide[..wide.len().saturating_sub(1)], &mut sz);
        let _ = SelectObject(dc, old);
    }
    sz.cx.max(0)
}

fn compute_menu_dimensions(entries: &[MenuEntry], dpi: u32, font_item: &OwnedFont, font_small: &OwnedFont) -> (i32, i32) {
    let s = |v: i32| scale_dpi(v, dpi);
    let pad_y = s(6);
    let mut total_h = pad_y * 2;
    let mut max_w = s(228);

    // Measure actual GDI ClearType text widths so shortcuts NEVER clip at 225%/250% DPI!
    let measure_surf = DibSurface::new(16, 16);

    for entry in entries {
        match entry {
            MenuEntry::Header { title, subtitle } => {
                total_h += s(42);
                if let Some(ref surf) = measure_surf {
                    let tw = measure_text_px(surf.dc, font_item.0, title)
                        + measure_text_px(surf.dc, font_small.0, subtitle)
                        + s(76);
                    max_w = max_w.max(tw);
                }
            }
            MenuEntry::Action {
                label, shortcut, ..
            } => {
                total_h += s(34);
                if let Some(ref surf) = measure_surf {
                    let lw = measure_text_px(surf.dc, font_item.0, label);
                    let sw = if shortcut.is_empty() {
                        0
                    } else {
                        measure_text_px(surf.dc, font_small.0, shortcut) + s(26)
                    };
                    let row_w = s(42) + lw + sw + s(22);
                    max_w = max_w.max(row_w);
                }
            }
            MenuEntry::Chips { items, .. } => {
                total_h += s(56);
                let chips_w = s(28) + (items.len() as i32) * s(46);
                max_w = max_w.max(chips_w);
            }
            MenuEntry::Separator => {
                total_h += s(11);
            }
        }
    }

    (max_w.min(s(360)), total_h)
}

/// Show a modal Windows 11 Fluent popup menu anchored at screen point `(anchor_x, anchor_y)`.
/// Returns `Some(command_id)` if the user clicked an enabled item, or `None` if dismissed.
pub fn show_popup_menu(
    owner: HWND,
    anchor_x: i32,
    anchor_y: i32,
    entries: Vec<MenuEntry>,
    dark: bool,
) -> Option<u32> {
    if entries.is_empty() {
        return None;
    }
    ensure_registered();

    // Determine target monitor work area & exact per-monitor DPI at cursor
    let pt = POINT {
        x: anchor_x,
        y: anchor_y,
    };
    let (work, dpi) = unsafe {
        let hmon = MonitorFromPoint(pt, MONITOR_DEFAULTTONEAREST);
        let mut info = MONITORINFOEXW::default();
        info.monitorInfo.cbSize = std::mem::size_of::<MONITORINFOEXW>() as u32;
        let work = if GetMonitorInfoW(hmon, &mut info.monitorInfo).as_bool() {
            info.monitorInfo.rcWork
        } else {
            RECT {
                left: 0,
                top: 0,
                right: 1920,
                bottom: 1080,
            }
        };
        let mut dx = 96u32;
        let mut dy = 96u32;
        let dpi = if GetDpiForMonitor(hmon, MDT_EFFECTIVE_DPI, &mut dx, &mut dy).is_ok() && dx >= 96
        {
            dx
        } else {
            96
        };
        (work, dpi)
    };

    let font_title = OwnedFont(create_ui_font(dpi, 9, true));
    let font_item = OwnedFont(create_ui_font(dpi, 9, false));
    let font_small = OwnedFont(create_ui_font(dpi, 8, false));
    let (width, height) = compute_menu_dimensions(&entries, dpi, &font_item, &font_small);

    let margin = scale_dpi(6, dpi);
    // Smart vertical anchor: pop UP when clicked near taskbar/bottom edge, otherwise pop DOWN
    let mut x = anchor_x;
    if x + width > work.right - margin {
        x = (anchor_x - width).max(work.left + margin);
    }
    x = x.clamp(work.left + margin, (work.right - width - margin).max(work.left + margin));

    let mut y = if anchor_y + height > work.bottom - margin {
        anchor_y - height - scale_dpi(4, dpi)
    } else {
        anchor_y + scale_dpi(2, dpi)
    };
    y = y.clamp(work.top + margin, (work.bottom - height - margin).max(work.top + margin));

    let state = Box::new(MenuState {
        entries,
        dark,
        dpi,
        width,
        height,
        hovered_id: Cell::new(None),
        tracking_leave: Cell::new(false),
        chosen_id: Cell::new(None),
        closed: Cell::new(false),
        hits: RefCell::new(Vec::new()),
        font_title,
        font_item,
        font_small,
    });
    ACTIVE_MENU.with(|cell| {
        *cell.borrow_mut() = Some(state);
    });

    let class_w = to_wide(POPUP_CLASS);
    let Ok(hwnd) = (unsafe {
        CreateWindowExW(
            WS_EX_TOPMOST | WS_EX_TOOLWINDOW,
            PCWSTR(class_w.as_ptr()),
            PCWSTR::null(),
            WS_POPUP,
            x,
            y,
            width,
            height,
            if owner.is_invalid() { None } else { Some(owner) },
            None,
            Some(current_hinstance()),
            None,
        )
    }) else {
        ACTIVE_MENU.with(|cell| {
            *cell.borrow_mut() = None;
        });
        return None;
    };

    // Native Windows 11 rounded corners + shadow + light/dark border
    unsafe {
        let dark_flag: i32 = if dark { 1 } else { 0 };
        let _ = DwmSetWindowAttribute(
            hwnd,
            DWMWA_USE_IMMERSIVE_DARK_MODE,
            &dark_flag as *const i32 as *const _,
            std::mem::size_of::<i32>() as u32,
        );
        let corner = DWMWCP_ROUND;
        let _ = DwmSetWindowAttribute(
            hwnd,
            DWMWA_WINDOW_CORNER_PREFERENCE,
            &corner as *const _ as *const _,
            std::mem::size_of_val(&corner) as u32,
        );
        let _ = SetWindowPos(
            hwnd,
            Some(HWND_TOPMOST),
            x,
            y,
            width,
            height,
            SWP_SHOWWINDOW,
        );
        let _ = ShowWindow(hwnd, SW_SHOW);
        let _ = SetForegroundWindow(hwnd);
        let _ = InvalidateRect(Some(hwnd), None, false);
    }

    // Lightweight nested modal message loop until menu closes (preserving any external WM_QUIT)
    let mut msg = MSG::default();
    let is_closed = || {
        ACTIVE_MENU.with(|cell| {
            cell.borrow()
                .as_ref()
                .map(|m| m.closed.get())
                .unwrap_or(true)
        })
    };
    unsafe {
        while !is_closed() {
            let gm = GetMessageW(&mut msg, None, 0, 0);
            if gm.0 <= 0 {
                if gm.0 == 0 {
                    // Re-post external WM_QUIT so the outer tray message loop exits cleanly
                    PostQuitMessage(msg.wParam.0 as i32);
                }
                break;
            }
            let _ = TranslateMessage(&msg);
            DispatchMessageW(&msg);
        }
    }

    ACTIVE_MENU.with(|cell| cell.borrow_mut().take().and_then(|s| s.chosen_id.get()))
}

impl MenuState {
    fn render_to_surface(&self, surface: &mut DibSurface) {
        let w = surface.width;
        let h = surface.height;
        let dpi = self.dpi;
        let s = |v: i32| scale_dpi(v, dpi);
        let sf = dpi as f32 / 96.0;
        let dark = self.dark;
        let hovered = self.hovered_id.get();

        let (
            bg_col,
            border_col,
            div_col,
            hover_bg,
            danger_hover_bg,
            txt_pri,
            txt_sec,
            txt_dis,
            txt_danger,
            txt_accent,
            icon_idle,
            keycap_bg,
            keycap_border,
            chip_bg,
            chip_active_bg,
        ) = if dark {
            (
                Color::from_rgba8(20, 29, 47, 255),
                Color::from_rgba8(255, 255, 255, 28),
                Color::from_rgba8(255, 255, 255, 16),
                Color::from_rgba8(255, 255, 255, 16),
                Color::from_rgba8(244, 63, 94, 38),
                (241, 245, 249),
                (148, 163, 184),
                (100, 116, 139),
                (251, 113, 133),
                (125, 211, 252),
                Color::from_rgba8(148, 163, 184, 255),
                Color::from_rgba8(255, 255, 255, 12),
                Color::from_rgba8(255, 255, 255, 22),
                Color::from_rgba8(255, 255, 255, 12),
                Color::from_rgba8(14, 165, 233, 235),
            )
        } else {
            (
                Color::from_rgba8(255, 255, 255, 255),
                Color::from_rgba8(15, 23, 42, 26),
                Color::from_rgba8(15, 23, 42, 16),
                Color::from_rgba8(15, 23, 42, 11),
                Color::from_rgba8(255, 228, 230, 255),
                (15, 23, 42),
                (100, 116, 139),
                (148, 163, 184),
                (225, 29, 72),
                (2, 132, 199),
                Color::from_rgba8(71, 85, 105, 255),
                Color::from_rgba8(241, 245, 249, 255),
                Color::from_rgba8(203, 213, 225, 255),
                Color::from_rgba8(241, 245, 249, 255),
                Color::from_rgba8(14, 165, 233, 255),
            )
        };

        let mut hits = Vec::new();
        // (text, rect, color, font_kind: 0=title, 1=item, 2=small, align)
        let mut texts: Vec<(String, RectI, (u8, u8, u8), u8, u32)> = Vec::new();

        if let Some(mut pm) = Pixmap::new(w as u32, h as u32) {
            fill_rect(&mut pm, 0.0, 0.0, w as f32, h as f32, bg_col);
            stroke_rrect(
                &mut pm,
                0.5 * sf,
                0.5 * sf,
                w as f32 - sf,
                h as f32 - sf,
                8.0 * sf,
                1.0 * sf,
                border_col,
            );

            let mut cy = s(6);
            let pad_x = s(6);

            for entry in &self.entries {
                match entry {
                    MenuEntry::Header { title, subtitle } => {
                        let row_h = s(42);
                        draw_rhpicture_emblem(
                            &mut pm,
                            (pad_x + s(8)) as f32,
                            (cy + s(9)) as f32,
                            24.0 * sf,
                        );
                        texts.push((
                            title.clone(),
                            RectI::new(pad_x + s(40), cy + s(4), w - pad_x - s(10), cy + s(23)),
                            txt_pri,
                            0,
                            DT_LEFT.0 | DT_END_ELLIPSIS.0,
                        ));
                        texts.push((
                            subtitle.clone(),
                            RectI::new(pad_x + s(40), cy + s(21), w - pad_x - s(10), cy + s(38)),
                            txt_sec,
                            2,
                            DT_LEFT.0 | DT_END_ELLIPSIS.0,
                        ));
                        cy += row_h;
                    }
                    MenuEntry::Separator => {
                        let div_y = cy + s(5);
                        fill_rect(
                            &mut pm,
                            (pad_x + s(8)) as f32,
                            div_y as f32,
                            (w - (pad_x + s(8)) * 2).max(0) as f32,
                            (1.0 * sf).max(1.0),
                            div_col,
                        );
                        cy += s(11);
                    }
                    MenuEntry::Action {
                        id,
                        icon,
                        label,
                        shortcut,
                        enabled,
                        checked,
                        danger,
                    } => {
                        let row_h = s(34);
                        let row_r = RectI::new(pad_x, cy, w - pad_x, cy + row_h);
                        hits.push(MenuHit {
                            rect: row_r,
                            id: *id,
                            enabled: *enabled,
                        });
                        let is_hov = *enabled && hovered == Some(*id);
                        if is_hov {
                            fill_rrect(
                                &mut pm,
                                row_r.left as f32,
                                row_r.top as f32 + 1.0 * sf,
                                row_r.width() as f32,
                                row_r.height() as f32 - 2.0 * sf,
                                6.0 * sf,
                                if *danger { danger_hover_bg } else { hover_bg },
                            );
                        }

                        // Icon or check mark on left
                        let icon_col = if !*enabled {
                            Color::from_rgba8(txt_dis.0, txt_dis.1, txt_dis.2, 200)
                        } else if *danger && is_hov {
                            Color::from_rgba8(txt_danger.0, txt_danger.1, txt_danger.2, 255)
                        } else if *checked {
                            Color::from_rgba8(txt_accent.0, txt_accent.1, txt_accent.2, 255)
                        } else {
                            icon_idle
                        };
                        let ix = (row_r.left + s(10)) as f32;
                        let iy = (row_r.top + (row_h - s(14)) / 2) as f32;
                        if *checked {
                            draw_menu_check(&mut pm, ix, iy, sf, icon_col);
                        } else {
                            draw_menu_icon(&mut pm, *icon, ix, iy, sf, icon_col);
                        }

                        // Right-aligned keycap shortcut pill (measured so never clipped!)
                        let mut label_right = row_r.right - s(10);
                        if !shortcut.is_empty() {
                            let sw = measure_text_px(surface.dc, self.font_small.0, shortcut);
                            let cap_w = (sw + s(14)).max(s(26));
                            let cap_h = s(20);
                            let cap_r = RectI::new(
                                row_r.right - s(8) - cap_w,
                                row_r.top + (row_h - cap_h) / 2,
                                row_r.right - s(8),
                                row_r.top + (row_h + cap_h) / 2,
                            );
                            fill_rrect(
                                &mut pm,
                                cap_r.left as f32,
                                cap_r.top as f32,
                                cap_r.width() as f32,
                                cap_r.height() as f32,
                                5.0 * sf,
                                keycap_bg,
                            );
                            stroke_rrect(
                                &mut pm,
                                cap_r.left as f32 + 0.5 * sf,
                                cap_r.top as f32 + 0.5 * sf,
                                cap_r.width() as f32 - sf,
                                cap_r.height() as f32 - sf,
                                4.5 * sf,
                                1.0 * sf,
                                keycap_border,
                            );
                            texts.push((
                                shortcut.clone(),
                                cap_r,
                                if *enabled { txt_sec } else { txt_dis },
                                2,
                                DT_CENTER.0,
                            ));
                            label_right = cap_r.left - s(8);
                        }

                        let fg = if !*enabled {
                            txt_dis
                        } else if *danger && is_hov {
                            txt_danger
                        } else if *checked {
                            txt_accent
                        } else {
                            txt_pri
                        };
                        texts.push((
                            label.clone(),
                            RectI::new(row_r.left + s(32), row_r.top, label_right, row_r.bottom),
                            fg,
                            1,
                            DT_LEFT.0 | DT_END_ELLIPSIS.0,
                        ));
                        cy += row_h;
                    }
                    MenuEntry::Chips { icon, label, items } => {
                        let row_h = s(56);
                        let ix = (pad_x + s(10)) as f32;
                        let iy = (cy + s(5)) as f32;
                        draw_menu_icon(&mut pm, *icon, ix, iy, sf, icon_idle);
                        texts.push((
                            label.clone(),
                            RectI::new(pad_x + s(32), cy + s(2), w - pad_x - s(8), cy + s(22)),
                            txt_sec,
                            2,
                            DT_LEFT.0,
                        ));

                        let n = items.len().max(1) as i32;
                        let gap = s(4);
                        let inner_l = pad_x + s(10);
                        let inner_w = (w - inner_l * 2).max(s(120));
                        let chip_w = (inner_w - gap * (n - 1)) / n;
                        let chip_y = cy + s(25);
                        let chip_h = s(24);
                        for (i, (cid, clabel, active)) in items.iter().enumerate() {
                            let cx = inner_l + (i as i32) * (chip_w + gap);
                            let cr = RectI::new(
                                cx,
                                chip_y,
                                if i + 1 == items.len() {
                                    w - inner_l
                                } else {
                                    cx + chip_w
                                },
                                chip_y + chip_h,
                            );
                            hits.push(MenuHit {
                                rect: cr,
                                id: *cid,
                                enabled: true,
                            });
                            let is_hov = hovered == Some(*cid);
                            let bg = if *active {
                                chip_active_bg
                            } else if is_hov {
                                hover_bg
                            } else {
                                chip_bg
                            };
                            fill_rrect(
                                &mut pm,
                                cr.left as f32,
                                cr.top as f32,
                                cr.width() as f32,
                                cr.height() as f32,
                                5.0 * sf,
                                bg,
                            );
                            if !*active {
                                stroke_rrect(
                                    &mut pm,
                                    cr.left as f32 + 0.5 * sf,
                                    cr.top as f32 + 0.5 * sf,
                                    cr.width() as f32 - sf,
                                    cr.height() as f32 - sf,
                                    4.5 * sf,
                                    1.0 * sf,
                                    keycap_border,
                                );
                            }
                            texts.push((
                                clabel.clone(),
                                cr,
                                if *active { (255, 255, 255) } else { txt_pri },
                                2,
                                DT_CENTER.0,
                            ));
                        }
                        cy += row_h;
                    }
                }
            }

            let src = pm.data();
            let dst = surface.pixels_mut();
            for (spx, dpx) in src.chunks_exact(4).zip(dst.chunks_exact_mut(4)) {
                dpx[0] = spx[2];
                dpx[1] = spx[1];
                dpx[2] = spx[0];
                dpx[3] = 255;
            }
        }

        for (txt, rect, col, fkind, align) in texts {
            let font = match fkind {
                0 => self.font_title.0,
                1 => self.font_item.0,
                _ => self.font_small.0,
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
                    &mut wide[..wide.len().saturating_sub(1)].to_vec(),
                    &mut r,
                    windows::Win32::Graphics::Gdi::DRAW_TEXT_FORMAT(
                        align | DT_VCENTER.0 | DT_SINGLELINE.0 | DT_NOCLIP.0,
                    ),
                );
                let _ = SelectObject(surface.dc, old);
            }
        }

        *self.hits.borrow_mut() = hits;
    }

    fn hit_at(&self, x: i32, y: i32) -> Option<MenuHit> {
        self.hits
            .borrow()
            .iter()
            .copied()
            .find(|h| h.rect.contains(x, y))
    }

    fn step_hover(&self, dir: i32) {
        let hits = self.hits.borrow();
        let enabled: Vec<u32> = hits.iter().filter(|h| h.enabled).map(|h| h.id).collect();
        if enabled.is_empty() {
            return;
        }
        let next = match self.hovered_id.get().and_then(|cur| enabled.iter().position(|&id| id == cur)) {
            Some(idx) => {
                let len = enabled.len() as i32;
                enabled[((idx as i32 + dir).rem_euclid(len)) as usize]
            }
            None => {
                if dir > 0 {
                    enabled[0]
                } else {
                    *enabled.last().unwrap()
                }
            }
        };
        self.hovered_id.set(Some(next));
    }
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

fn draw_menu_check(pm: &mut Pixmap, x: f32, y: f32, sf: f32, color: Color) {
    let mut paint = Paint::default();
    paint.set_color(color);
    paint.anti_alias = true;
    let st = Stroke {
        width: 1.8,
        line_cap: LineCap::Round,
        line_join: LineJoin::Round,
        ..Stroke::default()
    };
    let mut pb = PathBuilder::new();
    pb.move_to(2.0, 7.5);
    pb.line_to(5.5, 11.0);
    pb.line_to(12.2, 3.2);
    if let Some(path) = pb.finish() {
        let xf = Transform::from_translate(x, y).pre_scale(sf, sf);
        pm.stroke_path(&path, &paint, &st, xf, None);
    }
}

fn draw_menu_icon(pm: &mut Pixmap, icon: MenuIcon, x: f32, y: f32, sf: f32, color: Color) {
    if icon == MenuIcon::None {
        return;
    }
    let mut paint = Paint::default();
    paint.set_color(color);
    paint.anti_alias = true;
    let st = Stroke {
        width: 1.5,
        line_cap: LineCap::Round,
        line_join: LineJoin::Round,
        ..Stroke::default()
    };
    let mut pb = PathBuilder::new();
    match icon {
        MenuIcon::None => {}
        MenuIcon::Crop => {
            pb.move_to(2.0, 5.0);
            pb.line_to(2.0, 2.0);
            pb.line_to(5.0, 2.0);
            pb.move_to(9.0, 2.0);
            pb.line_to(12.0, 2.0);
            pb.line_to(12.0, 5.0);
            pb.move_to(2.0, 9.0);
            pb.line_to(2.0, 12.0);
            pb.line_to(5.0, 12.0);
            pb.move_to(9.0, 12.0);
            pb.line_to(12.0, 12.0);
            pb.line_to(12.0, 9.0);
        }
        MenuIcon::Pin => {
            pb.move_to(4.2, 2.0);
            pb.line_to(9.8, 2.0);
            pb.line_to(9.0, 6.2);
            pb.line_to(11.2, 8.2);
            pb.line_to(2.8, 8.2);
            pb.line_to(5.0, 6.2);
            pb.close();
            pb.move_to(7.0, 8.2);
            pb.line_to(7.0, 12.6);
        }
        MenuIcon::Eye => {
            pb.move_to(1.5, 7.0);
            pb.cubic_to(3.5, 3.4, 10.5, 3.4, 12.5, 7.0);
            pb.cubic_to(10.5, 10.6, 3.5, 10.6, 1.5, 7.0);
            pb.move_to(7.0, 5.4);
            pb.cubic_to(8.0, 5.4, 8.6, 6.1, 8.6, 7.0);
            pb.cubic_to(8.6, 7.9, 8.0, 8.6, 7.0, 8.6);
            pb.cubic_to(6.0, 8.6, 5.4, 7.9, 5.4, 7.0);
            pb.cubic_to(5.4, 6.1, 6.0, 5.4, 7.0, 5.4);
        }
        MenuIcon::EyeOff => {
            pb.move_to(1.5, 7.0);
            pb.cubic_to(3.5, 3.6, 10.5, 3.6, 12.5, 7.0);
            pb.cubic_to(10.5, 10.4, 3.5, 10.4, 1.5, 7.0);
            pb.move_to(2.0, 12.0);
            pb.line_to(12.0, 2.0);
        }
        MenuIcon::Pointer => {
            pb.move_to(3.0, 2.0);
            pb.line_to(3.0, 11.5);
            pb.line_to(5.8, 8.8);
            pb.line_to(8.2, 12.5);
            pb.line_to(9.8, 11.6);
            pb.line_to(7.4, 8.0);
            pb.line_to(11.0, 8.0);
            pb.close();
        }
        MenuIcon::Close => {
            pb.move_to(3.2, 3.2);
            pb.line_to(10.8, 10.8);
            pb.move_to(10.8, 3.2);
            pb.line_to(3.2, 10.8);
        }
        MenuIcon::Settings => {
            pb.move_to(2.0, 4.0);
            pb.line_to(12.0, 4.0);
            pb.move_to(2.0, 7.0);
            pb.line_to(12.0, 7.0);
            pb.move_to(2.0, 10.0);
            pb.line_to(12.0, 10.0);
            pb.move_to(5.0, 2.6);
            pb.line_to(5.0, 5.4);
            pb.move_to(9.5, 5.6);
            pb.line_to(9.5, 8.4);
            pb.move_to(6.0, 8.6);
            pb.line_to(6.0, 11.4);
        }
        MenuIcon::Update => {
            pb.move_to(11.6, 6.5);
            pb.cubic_to(11.6, 3.9, 9.6, 2.2, 7.0, 2.2);
            pb.cubic_to(4.9, 2.2, 3.2, 3.4, 2.6, 5.1);
            pb.move_to(2.4, 7.5);
            pb.cubic_to(2.4, 10.1, 4.4, 11.8, 7.0, 11.8);
            pb.cubic_to(9.1, 11.8, 10.8, 10.6, 11.4, 8.9);
            pb.move_to(9.8, 5.4);
            pb.line_to(11.8, 6.8);
            pb.line_to(12.9, 4.6);
        }
        MenuIcon::Power => {
            pb.move_to(7.0, 2.0);
            pb.line_to(7.0, 6.8);
            pb.move_to(4.1, 3.6);
            pb.cubic_to(1.8, 5.2, 1.8, 9.2, 4.2, 11.1);
            pb.cubic_to(6.0, 12.4, 8.0, 12.4, 9.8, 11.1);
            pb.cubic_to(12.2, 9.2, 12.2, 5.2, 9.9, 3.6);
        }
        MenuIcon::Copy => {
            pb.move_to(5.0, 4.5);
            pb.line_to(11.5, 4.5);
            pb.line_to(11.5, 12.0);
            pb.line_to(5.0, 12.0);
            pb.close();
            pb.move_to(9.0, 4.5);
            pb.line_to(9.0, 2.2);
            pb.line_to(2.5, 2.2);
            pb.line_to(2.5, 9.5);
            pb.line_to(5.0, 9.5);
        }
        MenuIcon::Save => {
            pb.move_to(7.0, 2.0);
            pb.line_to(7.0, 9.2);
            pb.move_to(4.2, 6.6);
            pb.line_to(7.0, 9.5);
            pb.line_to(9.8, 6.6);
            pb.move_to(2.2, 10.0);
            pb.line_to(2.2, 12.2);
            pb.line_to(11.8, 12.2);
            pb.line_to(11.8, 10.0);
        }
        MenuIcon::Reset => {
            pb.move_to(2.2, 5.5);
            pb.line_to(2.2, 2.2);
            pb.line_to(5.5, 2.2);
            pb.move_to(11.8, 8.5);
            pb.line_to(11.8, 11.8);
            pb.line_to(8.5, 11.8);
            pb.move_to(2.5, 2.5);
            pb.line_to(5.6, 5.6);
            pb.move_to(11.5, 11.5);
            pb.line_to(8.4, 8.4);
        }
        MenuIcon::Rotate => {
            pb.move_to(11.5, 7.0);
            pb.cubic_to(11.5, 4.2, 9.4, 2.2, 6.8, 2.2);
            pb.cubic_to(4.2, 2.2, 2.2, 4.3, 2.2, 7.0);
            pb.cubic_to(2.2, 9.7, 4.2, 11.8, 6.8, 11.8);
            pb.cubic_to(8.4, 11.8, 9.8, 11.0, 10.6, 9.8);
            pb.move_to(9.6, 5.6);
            pb.line_to(11.6, 7.2);
            pb.line_to(13.0, 5.0);
        }
        MenuIcon::Opacity => {
            pb.move_to(7.0, 1.8);
            pb.cubic_to(7.0, 1.8, 2.6, 6.5, 2.6, 8.8);
            pb.cubic_to(2.6, 11.0, 4.6, 12.5, 7.0, 12.5);
            pb.cubic_to(9.4, 12.5, 11.4, 11.0, 11.4, 8.8);
            pb.cubic_to(11.4, 6.5, 7.0, 1.8, 7.0, 1.8);
            pb.close();
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

unsafe extern "system" fn menu_wnd_proc(
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
            ACTIVE_MENU.with(|cell| {
                if let Ok(borrow) = cell.try_borrow() {
                    if let Some(m) = borrow.as_ref() {
                        if let Some(mut surface) = DibSurface::new(m.width, m.height) {
                            m.render_to_surface(&mut surface);
                            unsafe {
                                let _ = BitBlt(
                                    hdc,
                                    0,
                                    0,
                                    m.width,
                                    m.height,
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
        WM_SETCURSOR => {
            let hand = ACTIVE_MENU.with(|cell| {
                cell.try_borrow()
                    .ok()
                    .and_then(|b| b.as_ref().map(|m| m.hovered_id.get().is_some()))
                    .unwrap_or(false)
            });
            if hand {
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
            let (need_track, changed) = ACTIVE_MENU.with(|cell| {
                let Ok(borrow) = cell.try_borrow() else {
                    return (false, false);
                };
                let Some(m) = borrow.as_ref() else {
                    return (false, false);
                };
                let next = m.hit_at(x, y).filter(|h| h.enabled).map(|h| h.id);
                let changed = next != m.hovered_id.get();
                if changed {
                    m.hovered_id.set(next);
                }
                let track = if !m.tracking_leave.get() {
                    m.tracking_leave.set(true);
                    true
                } else {
                    false
                };
                (track, changed)
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
            if changed {
                unsafe {
                    let _ = InvalidateRect(Some(hwnd), None, false);
                }
            }
            return LRESULT(0);
        }
        WM_MOUSELEAVE => {
            let changed = ACTIVE_MENU.with(|cell| {
                cell.try_borrow()
                    .ok()
                    .and_then(|b| {
                        b.as_ref().map(|m| {
                            m.tracking_leave.set(false);
                            m.hovered_id.replace(None).is_some()
                        })
                    })
                    .unwrap_or(false)
            });
            if changed {
                unsafe {
                    let _ = InvalidateRect(Some(hwnd), None, false);
                }
            }
            return LRESULT(0);
        }
        WM_LBUTTONDOWN => {
            let (x, y) = lparam_xy(lparam);
            let hit = ACTIVE_MENU.with(|cell| {
                cell.try_borrow()
                    .ok()
                    .and_then(|b| b.as_ref().and_then(|m| m.hit_at(x, y)))
            });
            if let Some(h) = hit {
                if h.enabled {
                    ACTIVE_MENU.with(|cell| {
                        if let Some(m) = cell.borrow().as_ref() {
                            m.chosen_id.set(Some(h.id));
                        }
                    });
                    unsafe {
                        let _ = DestroyWindow(hwnd);
                    }
                }
            }
            return LRESULT(0);
        }
        WM_LBUTTONUP | WM_RBUTTONUP => return LRESULT(0),
        WM_KEYDOWN => {
            let vk = wparam.0 as u16;
            if vk == VK_ESCAPE.0 {
                unsafe {
                    let _ = DestroyWindow(hwnd);
                }
                return LRESULT(0);
            } else if vk == VK_DOWN.0 {
                ACTIVE_MENU.with(|cell| {
                    if let Some(m) = cell.borrow().as_ref() {
                        m.step_hover(1);
                    }
                });
                unsafe {
                    let _ = InvalidateRect(Some(hwnd), None, false);
                }
                return LRESULT(0);
            } else if vk == VK_UP.0 {
                ACTIVE_MENU.with(|cell| {
                    if let Some(m) = cell.borrow().as_ref() {
                        m.step_hover(-1);
                    }
                });
                unsafe {
                    let _ = InvalidateRect(Some(hwnd), None, false);
                }
                return LRESULT(0);
            } else if vk == VK_RETURN.0 {
                let has_choice = ACTIVE_MENU.with(|cell| {
                    if let Some(m) = cell.borrow().as_ref() {
                        if let Some(id) = m.hovered_id.get() {
                            m.chosen_id.set(Some(id));
                            return true;
                        }
                    }
                    false
                });
                if has_choice {
                    unsafe {
                        let _ = DestroyWindow(hwnd);
                    }
                }
                return LRESULT(0);
            }
        }
        WM_ACTIVATE => {
            if (wparam.0 & 0xFFFF) as u32 == WA_INACTIVE {
                unsafe {
                    let _ = DestroyWindow(hwnd);
                }
                return LRESULT(0);
            }
        }
        windows::Win32::UI::WindowsAndMessaging::WM_DESTROY => {
            ACTIVE_MENU.with(|cell| {
                if let Some(m) = cell.borrow().as_ref() {
                    m.closed.set(true);
                }
            });
            return LRESULT(0);
        }
        _ => {}
    }
    unsafe { DefWindowProcW(hwnd, msg, wparam, lparam) }
}

#[cfg(test)]
mod visual_tests {
    use super::*;
    use crate::config::SaveFormat;
    use std::path::PathBuf;

    fn save_menu_snap(entries: Vec<MenuEntry>, dark: bool, hovered: Option<u32>, name: &str) {
        let dpi = 216u32;
        let font_title = OwnedFont(create_ui_font(dpi, 9, true));
        let font_item = OwnedFont(create_ui_font(dpi, 9, false));
        let font_small = OwnedFont(create_ui_font(dpi, 8, false));
        let (w, h) = compute_menu_dimensions(&entries, dpi, &font_item, &font_small);
        let state = MenuState {
            entries,
            dark,
            dpi,
            width: w,
            height: h,
            hovered_id: Cell::new(hovered),
            tracking_leave: Cell::new(false),
            chosen_id: Cell::new(None),
            closed: Cell::new(false),
            hits: RefCell::new(Vec::new()),
            font_title,
            font_item,
            font_small,
        };
        let mut surf = DibSurface::new(w, h).unwrap();
        state.render_to_surface(&mut surf);
        let mut pix = surf.pixels_mut().to_vec();
        for px in pix.chunks_exact_mut(4) {
            px[3] = 255;
        }
        let out_dir = std::env::var("RHPICTURE_SNAPSHOT_DIR")
            .map(PathBuf::from)
            .unwrap_or_else(|_| std::env::temp_dir());
        let img = crate::image_ops::BgraImage::from_bgra(w as u32, h as u32, pix).unwrap();
        img.save_to_file(&out_dir.join(name), SaveFormat::Png)
            .unwrap();
    }

    #[test]
    #[ignore]
    fn render_popup_menu_visual_snapshots() {
        let tray_entries = vec![
            MenuEntry::Header {
                title: "RHPicture".into(),
                subtitle: "v0.1.3 · 零显存贴图".into(),
            },
            MenuEntry::Separator,
            MenuEntry::Action {
                id: 1,
                icon: MenuIcon::Crop,
                label: "开始截图".into(),
                shortcut: "Alt+X".into(),
                enabled: true,
                checked: false,
                danger: false,
            },
            MenuEntry::Action {
                id: 2,
                icon: MenuIcon::Pin,
                label: "贴图剪贴板图像".into(),
                shortcut: "Alt+C".into(),
                enabled: true,
                checked: false,
                danger: false,
            },
            MenuEntry::Separator,
            MenuEntry::Action {
                id: 3,
                icon: MenuIcon::Close,
                label: "关闭全部贴图 (0)".into(),
                shortcut: String::new(),
                enabled: false,
                checked: false,
                danger: false,
            },
            MenuEntry::Separator,
            MenuEntry::Action {
                id: 4,
                icon: MenuIcon::Settings,
                label: "设置与快捷键…".into(),
                shortcut: String::new(),
                enabled: true,
                checked: false,
                danger: false,
            },
            MenuEntry::Action {
                id: 5,
                icon: MenuIcon::Update,
                label: "检查更新…".into(),
                shortcut: String::new(),
                enabled: true,
                checked: false,
                danger: false,
            },
            MenuEntry::Separator,
            MenuEntry::Action {
                id: 6,
                icon: MenuIcon::Power,
                label: "退出 RHPicture".into(),
                shortcut: String::new(),
                enabled: true,
                checked: false,
                danger: true,
            },
        ];
        save_menu_snap(tray_entries.clone(), false, Some(1), "tray-menu-light.png");
        save_menu_snap(tray_entries, true, Some(6), "tray-menu-dark.png");

        let pin_entries = vec![
            MenuEntry::Action {
                id: 1,
                icon: MenuIcon::Copy,
                label: "复制图像".into(),
                shortcut: "Ctrl+C".into(),
                enabled: true,
                checked: false,
                danger: false,
            },
            MenuEntry::Action {
                id: 2,
                icon: MenuIcon::Save,
                label: "保存到本地…".into(),
                shortcut: "Ctrl+S".into(),
                enabled: true,
                checked: false,
                danger: false,
            },
            MenuEntry::Separator,
            MenuEntry::Action {
                id: 3,
                icon: MenuIcon::Reset,
                label: "重置为 100%".into(),
                shortcut: "0".into(),
                enabled: true,
                checked: false,
                danger: false,
            },
            MenuEntry::Action {
                id: 4,
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
                    (10, "100%".into(), true),
                    (11, "85%".into(), false),
                    (12, "70%".into(), false),
                    (13, "50%".into(), false),
                    (14, "30%".into(), false),
                ],
            },
            MenuEntry::Action {
                id: 5,
                icon: MenuIcon::Pointer,
                label: "鼠标穿透".into(),
                shortcut: String::new(),
                enabled: true,
                checked: false,
                danger: false,
            },
            MenuEntry::Separator,
            MenuEntry::Action {
                id: 6,
                icon: MenuIcon::EyeOff,
                label: "隐藏此贴图".into(),
                shortcut: "H".into(),
                enabled: true,
                checked: false,
                danger: false,
            },
            MenuEntry::Action {
                id: 7,
                icon: MenuIcon::Close,
                label: "关闭此贴图".into(),
                shortcut: "Esc".into(),
                enabled: true,
                checked: false,
                danger: true,
            },
        ];
        save_menu_snap(pin_entries, false, Some(2), "pin-menu-light.png");
    }
}
