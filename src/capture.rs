use crate::image_ops::BgraImage;

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct RectI {
    pub left: i32,
    pub top: i32,
    pub right: i32,
    pub bottom: i32,
}

impl RectI {
    pub fn new(left: i32, top: i32, right: i32, bottom: i32) -> Self {
        Self {
            left,
            top,
            right,
            bottom,
        }
    }

    pub fn normalized(self) -> Self {
        Self {
            left: self.left.min(self.right),
            top: self.top.min(self.bottom),
            right: self.left.max(self.right),
            bottom: self.top.max(self.bottom),
        }
    }

    pub fn width(self) -> i32 {
        (self.right - self.left).max(0)
    }

    pub fn height(self) -> i32 {
        (self.bottom - self.top).max(0)
    }

    pub fn is_empty(self) -> bool {
        self.width() <= 0 || self.height() <= 0
    }

    pub fn contains(self, x: i32, y: i32) -> bool {
        x >= self.left && x < self.right && y >= self.top && y < self.bottom
    }

    pub fn intersect(self, other: Self) -> Self {
        Self {
            left: self.left.max(other.left),
            top: self.top.max(other.top),
            right: self.right.min(other.right),
            bottom: self.bottom.min(other.bottom),
        }
    }

    pub fn offset(self, dx: i32, dy: i32) -> Self {
        Self {
            left: self.left + dx,
            top: self.top + dy,
            right: self.right + dx,
            bottom: self.bottom + dy,
        }
    }
}

pub struct ScreenCapture {
    pub origin_x: i32,
    pub origin_y: i32,
    pub image: BgraImage,
    pub monitors: Vec<RectI>,
    pub window_rects: Vec<RectI>,
}

#[cfg(windows)]
pub fn capture_virtual_screen(detect_windows: bool) -> Result<ScreenCapture, String> {
    use crate::util::DibSurface;
    use windows::Win32::Foundation::{HWND, LPARAM, RECT};
    use windows::core::BOOL;
    use windows::Win32::Graphics::Dwm::{
        DWMWA_CLOAKED, DWMWA_EXTENDED_FRAME_BOUNDS, DwmGetWindowAttribute,
    };
    use windows::Win32::Graphics::Gdi::{
        BitBlt, CAPTUREBLT, EnumDisplayMonitors, GetDC, GetMonitorInfoW, HDC, HMONITOR,
        MONITORINFO, ReleaseDC, SRCCOPY,
    };
    use windows::Win32::UI::WindowsAndMessaging::{
        EnumWindows, GetSystemMetrics, GetWindowLongW, GetWindowRect, GWL_EXSTYLE, IsIconic,
        IsWindowVisible, SM_CXVIRTUALSCREEN, SM_CYVIRTUALSCREEN, SM_XVIRTUALSCREEN,
        SM_YVIRTUALSCREEN, WS_EX_TOOLWINDOW,
    };

    unsafe {
        let origin_x = GetSystemMetrics(SM_XVIRTUALSCREEN);
        let origin_y = GetSystemMetrics(SM_YVIRTUALSCREEN);
        let width = GetSystemMetrics(SM_CXVIRTUALSCREEN);
        let height = GetSystemMetrics(SM_CYVIRTUALSCREEN);

        if width <= 0 || height <= 0 {
            return Err("无法获取屏幕尺寸".into());
        }

        let mut surface =
            DibSurface::new(width, height).ok_or_else(|| "无法分配截屏缓冲区".to_string())?;

        let screen_dc = GetDC(None);
        if screen_dc.is_invalid() {
            return Err("无法获取屏幕设备上下文".into());
        }

        let blit_res = BitBlt(
            surface.dc,
            0,
            0,
            width,
            height,
            Some(screen_dc),
            origin_x,
            origin_y,
            SRCCOPY | CAPTUREBLT,
        );
        let _ = ReleaseDC(None, screen_dc);
        blit_res.map_err(|e| format!("屏幕捕获失败：{e}"))?;

        let mut data = surface.pixels_mut().to_vec();
        // Force opaque alpha on captured desktop pixels
        for px in data.chunks_exact_mut(4) {
            px[3] = 255;
        }
        drop(surface);

        let bounds = RectI::new(0, 0, width, height);

        // Enumerate monitors in overlay-local coordinates
        struct MonCtx {
            origin_x: i32,
            origin_y: i32,
            monitors: Vec<RectI>,
        }
        unsafe extern "system" fn enum_mon_proc(
            hmon: HMONITOR,
            _hdc: HDC,
            _rect: *mut RECT,
            lparam: LPARAM,
        ) -> BOOL {
            let ctx = unsafe { &mut *(lparam.0 as *mut MonCtx) };
            let mut info = MONITORINFO {
                cbSize: std::mem::size_of::<MONITORINFO>() as u32,
                ..Default::default()
            };
            if unsafe { GetMonitorInfoW(hmon, &mut info) }.as_bool() {
                let r = info.rcMonitor;
                ctx.monitors.push(RectI::new(
                    r.left - ctx.origin_x,
                    r.top - ctx.origin_y,
                    r.right - ctx.origin_x,
                    r.bottom - ctx.origin_y,
                ));
            }
            BOOL(1)
        }

        let mut mon_ctx = MonCtx {
            origin_x,
            origin_y,
            monitors: Vec::new(),
        };
        let _ = EnumDisplayMonitors(
            None,
            None,
            Some(enum_mon_proc),
            LPARAM(&mut mon_ctx as *mut MonCtx as isize),
        );
        if mon_ctx.monitors.is_empty() {
            mon_ctx.monitors.push(bounds);
        }

        // Enumerate visible windows in Z-order (front-to-back) for smart hover detection
        let mut window_rects = Vec::new();
        if detect_windows {
            struct WinCtx {
                origin_x: i32,
                origin_y: i32,
                bounds: RectI,
                rects: Vec<RectI>,
            }
            unsafe extern "system" fn enum_win_proc(hwnd: HWND, lparam: LPARAM) -> BOOL {
                let ctx = unsafe { &mut *(lparam.0 as *mut WinCtx) };
                if !unsafe { IsWindowVisible(hwnd) }.as_bool() {
                    return BOOL(1);
                }
                if unsafe { IsIconic(hwnd) }.as_bool() {
                    return BOOL(1);
                }
                let ex_style = unsafe { GetWindowLongW(hwnd, GWL_EXSTYLE) } as u32;
                if ex_style & WS_EX_TOOLWINDOW.0 != 0 {
                    return BOOL(1);
                }
                let mut cloaked: u32 = 0;
                if unsafe {
                    DwmGetWindowAttribute(
                        hwnd,
                        DWMWA_CLOAKED,
                        &mut cloaked as *mut u32 as *mut _,
                        std::mem::size_of::<u32>() as u32,
                    )
                }
                .is_ok()
                    && cloaked != 0
                {
                    return BOOL(1);
                }

                let mut r = RECT::default();
                let got_dwm = unsafe {
                    DwmGetWindowAttribute(
                        hwnd,
                        DWMWA_EXTENDED_FRAME_BOUNDS,
                        &mut r as *mut RECT as *mut _,
                        std::mem::size_of::<RECT>() as u32,
                    )
                }
                .is_ok();
                if !got_dwm && unsafe { GetWindowRect(hwnd, &mut r) }.is_err() {
                    return BOOL(1);
                }

                let local = RectI::new(
                    r.left - ctx.origin_x,
                    r.top - ctx.origin_y,
                    r.right - ctx.origin_x,
                    r.bottom - ctx.origin_y,
                )
                .intersect(ctx.bounds);

                if local.width() >= 24 && local.height() >= 24 {
                    ctx.rects.push(local);
                }
                BOOL(1)
            }

            let mut win_ctx = WinCtx {
                origin_x,
                origin_y,
                bounds,
                rects: Vec::new(),
            };
            let _ = EnumWindows(
                Some(enum_win_proc),
                LPARAM(&mut win_ctx as *mut WinCtx as isize),
            );
            window_rects = win_ctx.rects;
        }

        let image = BgraImage::from_bgra(width as u32, height as u32, data)?;
        Ok(ScreenCapture {
            origin_x,
            origin_y,
            image,
            monitors: mon_ctx.monitors,
            window_rects,
        })
    }
}
