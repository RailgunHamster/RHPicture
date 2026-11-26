//! Independent, zero-VRAM update transition splash window for Velopack replacement.
//! Modeled after RHRemote's safe out-of-install handoff protocol (temp copy outside install dir,
//! ready/cancel file handshake, Velopack stage detection), rendered purely in Rust with tiny-skia + GDI.

use crate::util::{
    DibSurface, OwnedFont, create_ui_font, current_hinstance, from_wide, rgb_colorref, scale_dpi,
    to_wide, window_dpi,
};
use std::cell::RefCell;
use std::fs;
use std::os::windows::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::Once;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};
use tiny_skia::{
    Color, FillRule, GradientStop, LineCap, LinearGradient, Paint, PathBuilder, Pixmap, Point,
    SpreadMode, Stroke, Transform,
};
use windows::Win32::Foundation::{
    CloseHandle, HWND, LPARAM, LRESULT, MAX_PATH, RECT, WPARAM,
};
use windows::Win32::Graphics::Dwm::{
    DWMWA_USE_IMMERSIVE_DARK_MODE, DWMWA_WINDOW_CORNER_PREFERENCE, DWMWCP_ROUND,
    DwmSetWindowAttribute,
};
use windows::Win32::Graphics::Gdi::{
    BeginPaint, BitBlt, DT_CENTER, DT_LEFT, DT_NOCLIP, DT_RIGHT, DT_SINGLELINE, DT_VCENTER,
    DrawTextW, EndPaint, GetMonitorInfoW, InvalidateRect, MONITOR_DEFAULTTONEAREST,
    MONITORINFOEXW, MonitorFromWindow, PAINTSTRUCT, SRCCOPY, SelectObject, SetBkMode,
    SetTextColor, TRANSPARENT,
};
use windows::Win32::System::Diagnostics::ToolHelp::{
    CreateToolhelp32Snapshot, PROCESSENTRY32W, Process32FirstW, Process32NextW, TH32CS_SNAPPROCESS,
};
use windows::Win32::System::Threading::{
    GetExitCodeProcess, OpenProcess, PROCESS_QUERY_LIMITED_INFORMATION, QueryFullProcessImageNameW,
};
use windows::Win32::UI::Input::KeyboardAndMouse::ReleaseCapture;
use windows::Win32::UI::WindowsAndMessaging::{
    CreateWindowExW, DefWindowProcW, DestroyWindow, DispatchMessageW, FindWindowExW,
    GetForegroundWindow, GetMessageW, GetSystemMetrics, GetWindowThreadProcessId, HTCAPTION,
    HWND_TOPMOST, IDC_ARROW, KillTimer, LoadCursorW, MSG, PostQuitMessage, RegisterClassExW,
    SM_CXSCREEN, SM_CYSCREEN, SW_SHOWNOACTIVATE, SWP_NOACTIVATE, SWP_SHOWWINDOW, SendMessageW,
    SetTimer, SetWindowPos, ShowWindow, TranslateMessage, WM_DESTROY, WM_ERASEBKGND,
    WM_LBUTTONDOWN, WM_NCLBUTTONDOWN, WM_PAINT, WM_TIMER, WNDCLASSEXW, WS_EX_TOOLWINDOW,
    WS_EX_TOPMOST, WS_POPUP, WS_VISIBLE,
};
use windows::core::{PCWSTR, PWSTR};

const SPLASH_CLASS: &str = "RHPictureUpdateSplashClass";
const TIMER_ANIM: usize = 1;
const TIMER_PROBE: usize = 2;
const CREATE_NO_WINDOW: u32 = 0x0800_0000;

/// RAII guard for the launched transition splash. If Velopack fails to start before `commit()`,
/// dropping this guard writes the cancel file so the splash exits cleanly while the old app stays alive.
pub struct Transition {
    cancel_file: PathBuf,
    committed: bool,
}

impl Transition {
    pub fn commit(mut self) {
        self.committed = true;
    }
}

impl Drop for Transition {
    fn drop(&mut self) {
        if !self.committed {
            let _ = fs::write(&self.cancel_file, b"cancel");
        }
    }
}

fn unique_token() -> String {
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0);
    format!("{:x}-{}", nanos, std::process::id())
}

fn transition_base_dir() -> PathBuf {
    std::env::temp_dir().join("RHPicture-UpdateTransition")
}

/// Remove stale extracted splash helpers from previous updates.
pub fn cleanup_stale_transitions() {
    std::thread::spawn(|| {
        let base = transition_base_dir();
        let Ok(entries) = fs::read_dir(&base) else {
            return;
        };
        let cutoff = SystemTime::now() - Duration::from_secs(300);
        for entry in entries.flatten() {
            let path = entry.path();
            let too_old = entry
                .metadata()
                .ok()
                .and_then(|m| m.modified().ok())
                .is_some_and(|t| t < cutoff);
            if too_old {
                let _ = fs::remove_dir_all(&path);
            }
        }
    });
}

fn is_path_inside(child: &Path, parent: &Path) -> bool {
    let c = fs::canonicalize(child)
        .unwrap_or_else(|_| child.to_path_buf())
        .to_string_lossy()
        .to_lowercase();
    let p = fs::canonicalize(parent)
        .unwrap_or_else(|_| parent.to_path_buf())
        .to_string_lossy()
        .to_lowercase();
    c == p || c.starts_with(&(p + "\\"))
}

fn copy_self_outside(root: &Path) -> Result<(PathBuf, PathBuf), String> {
    let base = transition_base_dir();
    fs::create_dir_all(&base).map_err(|e| e.to_string())?;
    let dir = base.join(unique_token());
    fs::create_dir(&dir).map_err(|e| e.to_string())?;
    if !root.as_os_str().is_empty() && is_path_inside(&dir, root) {
        let _ = fs::remove_dir_all(&dir);
        return Err("Transition directory overlaps installation root".into());
    }
    let current = std::env::current_exe().map_err(|e| e.to_string())?;
    let helper = dir.join("RHPictureSplash.exe");
    fs::copy(&current, &helper).map_err(|e| e.to_string())?;
    Ok((dir, helper))
}

fn foreground_monitor_work_area() -> Option<RECT> {
    unsafe {
        let fg = GetForegroundWindow();
        if fg.is_invalid() {
            return None;
        }
        let mut pid = 0u32;
        GetWindowThreadProcessId(fg, Some(&mut pid));
        if pid != std::process::id() {
            return None;
        }
        let hmon = MonitorFromWindow(fg, MONITOR_DEFAULTTONEAREST);
        let mut info = MONITORINFOEXW::default();
        info.monitorInfo.cbSize = std::mem::size_of::<MONITORINFOEXW>() as u32;
        if GetMonitorInfoW(hmon, &mut info.monitorInfo).as_bool() {
            Some(info.monitorInfo.rcWork)
        } else {
            None
        }
    }
}

/// Launch the independent update transition splash outside the Velopack installation directory.
pub fn launch(from_version: &str, to_version: &str, root_dir: &Path) -> Option<Transition> {
    if cfg!(test) {
        return None;
    }
    let (dir, helper) = copy_self_outside(root_dir).ok()?;
    let cancel_file = dir.join("cancel.flag");
    let ready_file = dir.join("ready.flag");

    let mut cmd = Command::new(&helper);
    cmd.current_dir(&dir)
        .args([
            "--update-splash",
            "--from-version",
            from_version,
            "--to-version",
            to_version,
            "--wait-pid",
            &std::process::id().to_string(),
        ])
        .arg("--root-dir")
        .arg(root_dir)
        .arg("--cancel-file")
        .arg(&cancel_file)
        .arg("--ready-file")
        .arg(&ready_file)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .creation_flags(CREATE_NO_WINDOW);

    if let Some(work) = foreground_monitor_work_area() {
        cmd.args([
            "--work-rect",
            &format!("{},{},{},{}", work.left, work.top, work.right, work.bottom),
        ]);
    }

    let mut child = cmd.spawn().ok()?;
    let transition = Transition {
        cancel_file,
        committed: false,
    };

    let deadline = Instant::now() + Duration::from_secs(2);
    while Instant::now() < deadline {
        if ready_file.is_file() {
            return Some(transition);
        }
        if child.try_wait().ok().flatten().is_some() {
            return None;
        }
        std::thread::sleep(Duration::from_millis(12));
    }
    None
}

/// Parsed CLI options for `--update-splash` mode.
#[derive(Clone, Debug)]
pub struct SplashArgs {
    pub from_version: String,
    pub to_version: String,
    pub wait_pid: u32,
    pub root_dir: PathBuf,
    pub cancel_file: Option<PathBuf>,
    pub ready_file: Option<PathBuf>,
    pub work_rect: Option<RECT>,
    pub demo_ms: u64,
    pub demo_stage: usize,
    pub snapshot_path: Option<PathBuf>,
}

impl SplashArgs {
    pub fn parse_from_env() -> Option<Self> {
        let args: Vec<String> = std::env::args().collect();
        if !args.iter().any(|a| a == "--update-splash") {
            return None;
        }
        let mut out = Self {
            from_version: String::new(),
            to_version: String::new(),
            wait_pid: 0,
            root_dir: PathBuf::new(),
            cancel_file: None,
            ready_file: None,
            work_rect: None,
            demo_ms: 0,
            demo_stage: 2,
            snapshot_path: None,
        };
        let mut i = 1;
        while i < args.len() {
            let next = args.get(i + 1).cloned().unwrap_or_default();
            match args[i].as_str() {
                "--from-version" => {
                    out.from_version = next;
                    i += 2;
                }
                "--to-version" => {
                    out.to_version = next;
                    i += 2;
                }
                "--wait-pid" => {
                    out.wait_pid = next.parse().unwrap_or(0);
                    i += 2;
                }
                "--root-dir" => {
                    out.root_dir = PathBuf::from(next);
                    i += 2;
                }
                "--cancel-file" => {
                    if !next.is_empty() {
                        out.cancel_file = Some(PathBuf::from(next));
                    }
                    i += 2;
                }
                "--ready-file" => {
                    if !next.is_empty() {
                        out.ready_file = Some(PathBuf::from(next));
                    }
                    i += 2;
                }
                "--work-rect" => {
                    let nums: Vec<i32> = next.split(',').filter_map(|p| p.parse().ok()).collect();
                    if nums.len() == 4 {
                        out.work_rect = Some(RECT {
                            left: nums[0],
                            top: nums[1],
                            right: nums[2],
                            bottom: nums[3],
                        });
                    }
                    i += 2;
                }
                "--demo-ms" => {
                    out.demo_ms = next.parse().unwrap_or(0);
                    i += 2;
                }
                "--demo-stage" => {
                    out.demo_stage = next.parse().unwrap_or(2);
                    i += 2;
                }
                "--snapshot" => {
                    if !next.is_empty() {
                        out.snapshot_path = Some(PathBuf::from(next));
                    }
                    i += 2;
                }
                _ => i += 1,
            }
        }
        Some(out)
    }
}

struct SplashState {
    args: SplashArgs,
    dpi: u32,
    width: i32,
    height: i32,
    started: Instant,
    stage: usize, // 0: closing old, 1: unpacking, 2: swapping files, 3: launching new, 4: done
    done_at: Option<Instant>,
    target_progress: f32,
    display_progress: f32,
    shimmer_phase: f32,
    pulse_phase: f32,
    saw_temp_dir: bool,
    saw_swap: bool,
    snapshot_saved: bool,
    font_title: OwnedFont,
    font_badge: OwnedFont,
    font_status: OwnedFont,
    font_small: OwnedFont,
}

thread_local! {
    static SPLASH: RefCell<Option<SplashState>> = const { RefCell::new(None) };
}

static REGISTER_SPLASH: Once = Once::new();

pub fn run_splash_process(args: SplashArgs) {
    REGISTER_SPLASH.call_once(|| unsafe {
        let class_w = to_wide(SPLASH_CLASS);
        let wc = WNDCLASSEXW {
            cbSize: std::mem::size_of::<WNDCLASSEXW>() as u32,
            lpfnWndProc: Some(splash_wnd_proc),
            hInstance: current_hinstance(),
            hCursor: LoadCursorW(None, IDC_ARROW).unwrap_or_default(),
            lpszClassName: PCWSTR(class_w.as_ptr()),
            ..Default::default()
        };
        let _ = RegisterClassExW(&wc);
    });

    let class_w = to_wide(SPLASH_CLASS);
    let title_w = to_wide("RHPicture · 正在更新");
    let Ok(hwnd) = (unsafe {
        CreateWindowExW(
            WS_EX_TOPMOST | WS_EX_TOOLWINDOW,
            PCWSTR(class_w.as_ptr()),
            PCWSTR(title_w.as_ptr()),
            WS_POPUP | WS_VISIBLE,
            0,
            0,
            440,
            196,
            None,
            None,
            Some(current_hinstance()),
            None,
        )
    }) else {
        return;
    };

    // Enable Windows 11 native DWM rounded corners & dark mode border
    unsafe {
        let dark: i32 = 1;
        let _ = DwmSetWindowAttribute(
            hwnd,
            DWMWA_USE_IMMERSIVE_DARK_MODE,
            &dark as *const i32 as *const _,
            std::mem::size_of::<i32>() as u32,
        );
        let corner = DWMWCP_ROUND;
        let _ = DwmSetWindowAttribute(
            hwnd,
            DWMWA_WINDOW_CORNER_PREFERENCE,
            &corner as *const _ as *const _,
            std::mem::size_of_val(&corner) as u32,
        );
    }

    let dpi = window_dpi(hwnd);
    let width = scale_dpi(440, dpi);
    let height = scale_dpi(196, dpi);
    let (x, y) = if let Some(work) = args.work_rect {
        (
            work.left + ((work.right - work.left) - width) / 2,
            work.top + ((work.bottom - work.top) - height) / 2,
        )
    } else {
        let sx = unsafe { GetSystemMetrics(SM_CXSCREEN) };
        let sy = unsafe { GetSystemMetrics(SM_CYSCREEN) };
        ((sx - width) / 2, (sy - height) / 2)
    };

    unsafe {
        let _ = SetWindowPos(
            hwnd,
            Some(HWND_TOPMOST),
            x,
            y,
            width,
            height,
            SWP_NOACTIVATE | SWP_SHOWWINDOW,
        );
    }

    let initial_stage = if args.snapshot_path.is_some() {
        args.demo_stage.min(4)
    } else if args.demo_ms > 0 {
        1
    } else {
        0
    };
    let initial_prog = stage_base_progress(initial_stage);

    let ready_file = args.ready_file.clone();
    SPLASH.with(|cell| {
        *cell.borrow_mut() = Some(SplashState {
            args,
            dpi,
            width,
            height,
            started: Instant::now(),
            stage: initial_stage,
            done_at: None,
            target_progress: if initial_stage > 0 { initial_prog + 0.08 } else { initial_prog },
            display_progress: if initial_stage > 0 { initial_prog + 0.08 } else { 0.08 },
            shimmer_phase: 0.35,
            pulse_phase: 0.0,
            saw_temp_dir: false,
            saw_swap: false,
            snapshot_saved: false,
            font_title: OwnedFont(create_ui_font(dpi, 13, true)),
            font_badge: OwnedFont(create_ui_font(dpi, 8, true)),
            font_status: OwnedFont(create_ui_font(dpi, 9, false)),
            font_small: OwnedFont(create_ui_font(dpi, 8, false)),
        });
    });

    unsafe {
        let _ = ShowWindow(hwnd, SW_SHOWNOACTIVATE);
        let _ = InvalidateRect(Some(hwnd), None, false);
        let _ = SetTimer(Some(hwnd), TIMER_ANIM, 16, None);
        let _ = SetTimer(Some(hwnd), TIMER_PROBE, 100, None);
    }

    if let Some(ref ready) = ready_file {
        let _ = fs::write(ready, std::process::id().to_string());
    }

    let mut msg = MSG::default();
    unsafe {
        while GetMessageW(&mut msg, None, 0, 0).into() {
            let _ = TranslateMessage(&msg);
            DispatchMessageW(&msg);
        }
    }
}

fn stage_base_progress(stage: usize) -> f32 {
    match stage {
        0 => 0.14,
        1 => 0.38,
        2 => 0.68,
        3 => 0.88,
        _ => 1.0,
    }
}

fn stage_max_progress(stage: usize) -> f32 {
    match stage {
        0 => 0.28,
        1 => 0.62,
        2 => 0.84,
        3 => 0.96,
        _ => 1.0,
    }
}

fn stage_subtitle(stage: usize, to_ver: &str) -> String {
    match stage {
        0 => "正在安全退出旧版本…".into(),
        1 => "正在解压并校验增量更新包…".into(),
        2 => "正在替换程序核心文件…".into(),
        3 => "正在启动新版 RHPicture…".into(),
        _ => {
            if to_ver.is_empty() {
                "更新完成，已就绪".into()
            } else {
                format!("已成功更新至 v{to_ver}")
            }
        }
    }
}

fn is_pid_alive(pid: u32) -> bool {
    if pid == 0 {
        return false;
    }
    unsafe {
        let Ok(h) = OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, false, pid) else {
            return false;
        };
        let mut code = 0u32;
        const STILL_ACTIVE_CODE: u32 = 259;
        let alive = GetExitCodeProcess(h, &mut code).is_ok() && code == STILL_ACTIVE_CODE;
        let _ = CloseHandle(h);
        alive
    }
}

fn query_process_path(pid: u32) -> Option<PathBuf> {
    unsafe {
        let h = OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, false, pid).ok()?;
        let mut buf = vec![0u16; MAX_PATH as usize * 2];
        let mut len = buf.len() as u32;
        let ok = QueryFullProcessImageNameW(
            h,
            windows::Win32::System::Threading::PROCESS_NAME_WIN32,
            PWSTR(buf.as_mut_ptr()),
            &mut len,
        )
        .is_ok();
        let _ = CloseHandle(h);
        if ok && len > 0 {
            Some(PathBuf::from(from_wide(&buf[..len as usize])))
        } else {
            None
        }
    }
}

pub fn expected_main_path(candidate: &Path, root: &Path) -> bool {
    if root.as_os_str().is_empty() {
        return false;
    }
    let expected = root.join("current").join("RHPicture.exe");
    let c_str = fs::canonicalize(candidate)
        .unwrap_or_else(|_| candidate.to_path_buf())
        .to_string_lossy()
        .to_lowercase();
    let e_str = fs::canonicalize(&expected)
        .unwrap_or(expected)
        .to_string_lossy()
        .to_lowercase();
    c_str == e_str
}

fn is_update_exe_running(root: &Path) -> bool {
    if root.as_os_str().is_empty() {
        return false;
    }
    let expected = root
        .join("Update.exe")
        .to_string_lossy()
        .to_lowercase();
    unsafe {
        let Ok(snap) = CreateToolhelp32Snapshot(TH32CS_SNAPPROCESS, 0) else {
            return false;
        };
        let mut entry = PROCESSENTRY32W {
            dwSize: std::mem::size_of::<PROCESSENTRY32W>() as u32,
            ..Default::default()
        };
        let mut found = false;
        if Process32FirstW(snap, &mut entry).is_ok() {
            loop {
                let name = from_wide(&entry.szExeFile);
                if name.eq_ignore_ascii_case("Update.exe") {
                    if let Some(p) = query_process_path(entry.th32ProcessID) {
                        if p.to_string_lossy().to_lowercase() == expected {
                            found = true;
                            break;
                        }
                    }
                }
                if Process32NextW(snap, &mut entry).is_err() {
                    break;
                }
            }
        }
        let _ = CloseHandle(snap);
        found
    }
}

fn is_new_rhpicture_ready(root: &Path, wait_pid: u32) -> bool {
    let self_pid = std::process::id();
    let tray_class = to_wide(crate::tray::TRAY_CLASS);
    unsafe {
        let mut cur = HWND::default();
        loop {
            let Ok(next) = FindWindowExW(
                None,
                if cur.is_invalid() { None } else { Some(cur) },
                PCWSTR(tray_class.as_ptr()),
                PCWSTR::null(),
            ) else {
                break;
            };
            if next.is_invalid() {
                break;
            }
            let mut pid = 0u32;
            GetWindowThreadProcessId(next, Some(&mut pid));
            if pid != 0 && pid != wait_pid && pid != self_pid {
                if let Some(path) = query_process_path(pid) {
                    if expected_main_path(&path, root) {
                        return true;
                    }
                }
            }
            cur = next;
        }
    }
    false
}

impl SplashState {
    fn version_badge(&self) -> String {
        if !self.args.from_version.is_empty()
            && !self.args.to_version.is_empty()
            && self.args.from_version != self.args.to_version
        {
            format!("v{} → v{}", self.args.from_version, self.args.to_version)
        } else if !self.args.to_version.is_empty() {
            format!("v{}", self.args.to_version)
        } else {
            "增量更新".into()
        }
    }

    fn render_to_surface(&self, surface: &mut DibSurface) {
        let w = surface.width;
        let h = surface.height;
        let dpi = self.dpi;
        let s = |v: i32| scale_dpi(v, dpi);
        let sf = dpi as f32 / 96.0;

        if let Some(mut pm) = Pixmap::new(w as u32, h as u32) {
            // 1. Sleek obsidian-slate backdrop with subtle top-left radial/linear sheen
            if let Some(grad) = LinearGradient::new(
                Point::from_xy(0.0, 0.0),
                Point::from_xy(w as f32, h as f32),
                vec![
                    GradientStop::new(0.0, Color::from_rgba8(15, 23, 42, 255)),
                    GradientStop::new(0.55, Color::from_rgba8(17, 24, 39, 255)),
                    GradientStop::new(1.0, Color::from_rgba8(11, 17, 32, 255)),
                ],
                SpreadMode::Pad,
                Transform::identity(),
            ) {
                let mut paint = Paint::default();
                paint.shader = grad;
                pm.fill_rect(
                    tiny_skia::Rect::from_xywh(0.0, 0.0, w as f32, h as f32).unwrap(),
                    &paint,
                    Transform::identity(),
                    None,
                );
            }

            // Subtle ambient top accent glow bar
            if let Some(glow) = LinearGradient::new(
                Point::from_xy(0.0, 0.0),
                Point::from_xy(w as f32, 0.0),
                vec![
                    GradientStop::new(0.0, Color::from_rgba8(14, 165, 233, 0)),
                    GradientStop::new(0.35, Color::from_rgba8(56, 189, 248, 170)),
                    GradientStop::new(0.65, Color::from_rgba8(52, 211, 153, 170)),
                    GradientStop::new(1.0, Color::from_rgba8(52, 211, 153, 0)),
                ],
                SpreadMode::Pad,
                Transform::identity(),
            ) {
                let mut paint = Paint::default();
                paint.shader = glow;
                pm.fill_rect(
                    tiny_skia::Rect::from_xywh(0.0, 0.0, w as f32, (2.0 * sf).max(2.0)).unwrap(),
                    &paint,
                    Transform::identity(),
                    None,
                );
            }

            // Inner glass card border
            draw_rounded_stroke(
                &mut pm,
                0.5 * sf,
                0.5 * sf,
                w as f32 - sf,
                h as f32 - sf,
                10.0 * sf,
                1.0 * sf,
                Color::from_rgba8(255, 255, 255, 24),
            );

            // 2. Modern RHPicture vector icon badge on left with animated subtle ring
            let icon_x = 24.0 * sf;
            let icon_y = 24.0 * sf;
            let icon_sz = 46.0 * sf;
            let pulse_alpha = (((self.pulse_phase.sin() * 0.5 + 0.5) * 70.0) + 30.0) as u8;
            draw_rounded_stroke(
                &mut pm,
                icon_x - 3.0 * sf,
                icon_y - 3.0 * sf,
                icon_sz + 6.0 * sf,
                icon_sz + 6.0 * sf,
                14.0 * sf,
                1.6 * sf,
                Color::from_rgba8(56, 189, 248, pulse_alpha),
            );
            draw_rhpicture_emblem(&mut pm, icon_x, icon_y, icon_sz);

            // 3. Version pill badge right of title
            let badge_x = 180.0 * sf;
            let badge_y = 26.0 * sf;
            let badge_text = self.version_badge();
            let badge_w = ((badge_text.chars().count() as f32) * 7.2 + 20.0).max(76.0) * sf;
            let badge_h = 22.0 * sf;
            draw_rounded_fill(
                &mut pm,
                badge_x,
                badge_y,
                badge_w,
                badge_h,
                11.0 * sf,
                Color::from_rgba8(14, 165, 233, 34),
            );
            draw_rounded_stroke(
                &mut pm,
                badge_x,
                badge_y,
                badge_w,
                badge_h,
                11.0 * sf,
                1.0 * sf,
                Color::from_rgba8(56, 189, 248, 95),
            );

            // 4. Minimalist 3-step indicator pills
            let current_step = match self.stage {
                0 | 1 => 0usize,
                2 => 1,
                3 => 2,
                _ => 3,
            };
            let step_y = 92.0 * sf;
            let step_w = (w as f32 - 48.0 * sf - 16.0 * sf) / 3.0;
            let step_h = 24.0 * sf;
            for i in 0..3 {
                let sx = 24.0 * sf + (i as f32) * (step_w + 8.0 * sf);
                let done = i < current_step;
                let active = i == current_step;
                let fill = if done {
                    Color::from_rgba8(16, 185, 129, 32)
                } else if active {
                    Color::from_rgba8(14, 165, 233, 38)
                } else {
                    Color::from_rgba8(255, 255, 255, 10)
                };
                draw_rounded_fill(&mut pm, sx, step_y, step_w, step_h, 6.0 * sf, fill);
                if active {
                    draw_rounded_stroke(
                        &mut pm,
                        sx,
                        step_y,
                        step_w,
                        step_h,
                        6.0 * sf,
                        1.0 * sf,
                        Color::from_rgba8(56, 189, 248, 110),
                    );
                }
            }

            // 5. Sleek luminous cyan->emerald progress bar + moving specular highlight
            let bar_x = 24.0 * sf;
            let bar_y = 132.0 * sf;
            let bar_w = w as f32 - 48.0 * sf;
            let bar_h = 7.0 * sf;
            draw_rounded_fill(
                &mut pm,
                bar_x,
                bar_y,
                bar_w,
                bar_h,
                bar_h * 0.5,
                Color::from_rgba8(30, 41, 59, 255),
            );
            let fill_w = (bar_w * self.display_progress.clamp(0.06, 1.0)).max(bar_h * 2.0);
            if let Some(bar_path) = rounded_path(bar_x, bar_y, fill_w, bar_h, bar_h * 0.5) {
                if let Some(grad) = LinearGradient::new(
                    Point::from_xy(bar_x, bar_y),
                    Point::from_xy(bar_x + fill_w, bar_y),
                    vec![
                        GradientStop::new(0.0, Color::from_rgba8(14, 165, 233, 255)),
                        GradientStop::new(1.0, Color::from_rgba8(52, 211, 153, 255)),
                    ],
                    SpreadMode::Pad,
                    Transform::identity(),
                ) {
                    let mut paint = Paint::default();
                    paint.shader = grad;
                    paint.anti_alias = true;
                    pm.fill_path(&bar_path, &paint, FillRule::Winding, Transform::identity(), None);
                }

                // Specular shimmer sweep
                let sh_cx = bar_x + fill_w * self.shimmer_phase.clamp(0.0, 1.0);
                let sh_half = (28.0 * sf).max(fill_w * 0.24);
                if let Some(sh_grad) = LinearGradient::new(
                    Point::from_xy(sh_cx - sh_half, bar_y),
                    Point::from_xy(sh_cx + sh_half, bar_y),
                    vec![
                        GradientStop::new(0.0, Color::from_rgba8(255, 255, 255, 0)),
                        GradientStop::new(0.5, Color::from_rgba8(255, 255, 255, 130)),
                        GradientStop::new(1.0, Color::from_rgba8(255, 255, 255, 0)),
                    ],
                    SpreadMode::Pad,
                    Transform::identity(),
                ) {
                    let mut sh_paint = Paint::default();
                    sh_paint.shader = sh_grad;
                    sh_paint.anti_alias = true;
                    pm.fill_path(
                        &bar_path,
                        &sh_paint,
                        FillRule::Winding,
                        Transform::identity(),
                        None,
                    );
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

        // 6. Crisp ClearType GDI typography on top
        let draw_txt = |text: &str, rect: RECT, color: (u8, u8, u8), font: &OwnedFont, align: u32| {
            let wide = to_wide(text);
            let mut r = rect;
            unsafe {
                let old = SelectObject(
                    surface.dc,
                    windows::Win32::Graphics::Gdi::HGDIOBJ(font.0.0),
                );
                SetBkMode(surface.dc, TRANSPARENT);
                SetTextColor(surface.dc, rgb_colorref(color.0, color.1, color.2));
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
        };

        // Title & version pill
        draw_txt(
            "RHPicture",
            RECT {
                left: s(84),
                top: s(20),
                right: s(178),
                bottom: s(48),
            },
            (248, 250, 252),
            &self.font_title,
            DT_LEFT.0,
        );
        let badge_text = self.version_badge();
        let badge_w = ((badge_text.chars().count() as f32) * 7.2 + 20.0).max(76.0);
        draw_txt(
            &badge_text,
            RECT {
                left: s(180),
                top: s(26),
                right: s(180 + badge_w.round() as i32),
                bottom: s(48),
            },
            (125, 211, 252),
            &self.font_badge,
            DT_CENTER.0,
        );

        // Status subtitle
        let sub = stage_subtitle(self.stage, &self.args.to_version);
        draw_txt(
            &sub,
            RECT {
                left: s(84),
                top: s(50),
                right: w - s(24),
                bottom: s(74),
            },
            (148, 163, 184),
            &self.font_status,
            DT_LEFT.0,
        );

        // 3 step labels
        let current_step = match self.stage {
            0 | 1 => 0usize,
            2 => 1,
            3 => 2,
            _ => 3,
        };
        let step_labels = ["准备与校验", "替换文件", "启动新版"];
        let step_w = (w - s(48) - s(16)) / 3;
        for (i, label) in step_labels.iter().enumerate() {
            let sx = s(24) + (i as i32) * (step_w + s(8));
            let done = i < current_step;
            let active = i == current_step;
            let prefix = if done { "✓ " } else if active { "● " } else { "○ " };
            let col = if done {
                (52, 211, 153)
            } else if active {
                (125, 211, 252)
            } else {
                (100, 116, 139)
            };
            draw_txt(
                &format!("{prefix}{label}"),
                RECT {
                    left: sx,
                    top: s(92),
                    right: sx + step_w,
                    bottom: s(116),
                },
                col,
                &self.font_small,
                DT_CENTER.0,
            );
        }

        // Footer row
        draw_txt(
            "个人快捷键与配置已自动保留 · 零显存更新中",
            RECT {
                left: s(24),
                top: s(152),
                right: w - s(100),
                bottom: s(176),
            },
            (100, 116, 139),
            &self.font_small,
            DT_LEFT.0,
        );
        let pct = (self.display_progress * 100.0).round() as i32;
        let secs = self.started.elapsed().as_secs();
        draw_txt(
            &format!("{pct}% · {:02}:{:02}", secs / 60, secs % 60),
            RECT {
                left: w - s(110),
                top: s(152),
                right: w - s(24),
                bottom: s(176),
            },
            (148, 163, 184),
            &self.font_small,
            DT_RIGHT.0,
        );
    }

    fn save_snapshot(&self, path: &Path) {
        if let Some(mut surface) = DibSurface::new(self.width, self.height) {
            self.render_to_surface(&mut surface);
            let mut pixels = surface.pixels_mut().to_vec();
            for px in pixels.chunks_exact_mut(4) {
                px[3] = 255;
            }
            if let Ok(img) =
                crate::image_ops::BgraImage::from_bgra(self.width as u32, self.height as u32, pixels)
            {
                let _ = img.save_to_file(path, crate::config::SaveFormat::Png);
            }
        }
    }
}

fn rounded_path(x: f32, y: f32, w: f32, h: f32, r: f32) -> Option<tiny_skia::Path> {
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

fn draw_rounded_fill(pm: &mut Pixmap, x: f32, y: f32, w: f32, h: f32, r: f32, color: Color) {
    if let Some(path) = rounded_path(x, y, w, h, r) {
        let mut paint = Paint::default();
        paint.set_color(color);
        paint.anti_alias = true;
        pm.fill_path(&path, &paint, FillRule::Winding, Transform::identity(), None);
    }
}

fn draw_rounded_stroke(
    pm: &mut Pixmap,
    x: f32,
    y: f32,
    w: f32,
    h: f32,
    r: f32,
    width: f32,
    color: Color,
) {
    if let Some(path) = rounded_path(x, y, w, h, r) {
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

pub fn draw_rhpicture_emblem(pm: &mut Pixmap, x: f32, y: f32, size: f32) {
    if let Some(bg_path) = rounded_path(x, y, size, size, size * 0.24) {
        if let Some(grad) = LinearGradient::new(
            Point::from_xy(x, y),
            Point::from_xy(x + size, y + size),
            vec![
                GradientStop::new(0.0, Color::from_rgba8(14, 165, 233, 255)),
                GradientStop::new(1.0, Color::from_rgba8(37, 99, 235, 255)),
            ],
            SpreadMode::Pad,
            Transform::identity(),
        ) {
            let mut paint = Paint::default();
            paint.shader = grad;
            paint.anti_alias = true;
            pm.fill_path(&bg_path, &paint, FillRule::Winding, Transform::identity(), None);
        }
    }

    // Crisp white viewfinder brackets + center pin needle
    let mut paint = Paint::default();
    paint.set_color(Color::from_rgba8(255, 255, 255, 245));
    paint.anti_alias = true;
    let stroke = Stroke {
        width: (size * 0.075).max(1.6),
        line_cap: LineCap::Round,
        ..Stroke::default()
    };
    let m = size * 0.23;
    let arm = size * 0.17;
    let mut pb = PathBuilder::new();
    // TL
    pb.move_to(x + m, y + m + arm);
    pb.line_to(x + m, y + m);
    pb.line_to(x + m + arm, y + m);
    // TR
    pb.move_to(x + size - m - arm, y + m);
    pb.line_to(x + size - m, y + m);
    pb.line_to(x + size - m, y + m + arm);
    // BL
    pb.move_to(x + m, y + size - m - arm);
    pb.line_to(x + m, y + size - m);
    pb.line_to(x + m + arm, y + size - m);
    // BR
    pb.move_to(x + size - m - arm, y + size - m);
    pb.line_to(x + size - m, y + size - m);
    pb.line_to(x + size - m, y + size - m - arm);
    // Diagonal pin needle
    pb.move_to(x + size * 0.50, y + size * 0.50);
    pb.line_to(x + size * 0.67, y + size * 0.67);
    if let Some(path) = pb.finish() {
        pm.stroke_path(&path, &paint, &stroke, Transform::identity(), None);
    }

    // Center pin head circle
    if let Some(dot) = PathBuilder::from_circle(x + size * 0.49, y + size * 0.49, size * 0.115) {
        pm.fill_path(&dot, &paint, FillRule::Winding, Transform::identity(), None);
    }
}

unsafe extern "system" fn splash_wnd_proc(
    hwnd: HWND,
    msg: u32,
    wparam: WPARAM,
    lparam: LPARAM,
) -> LRESULT {
    match msg {
        WM_ERASEBKGND => return LRESULT(1),
        WM_LBUTTONDOWN => {
            unsafe {
                let _ = ReleaseCapture();
                let _ = SendMessageW(hwnd, WM_NCLBUTTONDOWN, Some(WPARAM(HTCAPTION as usize)), Some(LPARAM(0)));
            }
            return LRESULT(0);
        }
        WM_PAINT => {
            let mut ps = PAINTSTRUCT::default();
            let hdc = unsafe { BeginPaint(hwnd, &mut ps) };
            SPLASH.with(|cell| {
                if let Some(s) = cell.borrow().as_ref() {
                    if let Some(mut surface) = DibSurface::new(s.width, s.height) {
                        s.render_to_surface(&mut surface);
                        unsafe {
                            let _ = BitBlt(
                                hdc,
                                0,
                                0,
                                s.width,
                                s.height,
                                Some(surface.dc),
                                0,
                                0,
                                SRCCOPY,
                            );
                        }
                    }
                }
            });
            unsafe {
                let _ = EndPaint(hwnd, &ps);
            }
            return LRESULT(0);
        }
        WM_TIMER => {
            let mut should_close = false;
            if wparam.0 == TIMER_ANIM {
                SPLASH.with(|cell| {
                    let mut borrow = cell.borrow_mut();
                    let Some(s) = borrow.as_mut() else { return };
                    s.shimmer_phase += 0.025;
                    if s.shimmer_phase > 1.35 {
                        s.shimmer_phase = -0.35;
                    }
                    s.pulse_phase += 0.09;

                    if let Some(ref snap_path) = s.args.snapshot_path {
                        if !s.snapshot_saved && s.started.elapsed() >= Duration::from_millis(80) {
                            s.snapshot_saved = true;
                            s.save_snapshot(snap_path);
                            if s.args.demo_ms == 0 {
                                should_close = true;
                            }
                        }
                    } else if s.args.demo_ms > 0 {
                        let ratio = (s.started.elapsed().as_millis() as f32
                            / s.args.demo_ms.max(800) as f32)
                            .clamp(0.0, 1.0);
                        s.stage = if ratio < 0.35 {
                            1
                        } else if ratio < 0.70 {
                            2
                        } else if ratio < 0.92 {
                            3
                        } else {
                            4
                        };
                        s.target_progress = (0.12 + ratio * 0.88).min(1.0);
                        s.display_progress += (s.target_progress - s.display_progress) * 0.14;
                        if s.started.elapsed().as_millis() >= s.args.demo_ms as u128 {
                            should_close = true;
                        }
                    } else {
                        let cap = stage_max_progress(s.stage);
                        if s.target_progress < cap {
                            s.target_progress = (s.target_progress + 0.002).min(cap);
                        }
                        s.display_progress += (s.target_progress - s.display_progress) * 0.14;
                        if let Some(done_at) = s.done_at {
                            if done_at.elapsed() >= Duration::from_millis(550) {
                                should_close = true;
                            }
                        }
                    }
                });
                if should_close {
                    unsafe {
                        let _ = DestroyWindow(hwnd);
                    }
                } else {
                    unsafe {
                        let _ = InvalidateRect(Some(hwnd), None, false);
                    }
                }
                return LRESULT(0);
            } else if wparam.0 == TIMER_PROBE {
                SPLASH.with(|cell| {
                    let mut borrow = cell.borrow_mut();
                    let Some(s) = borrow.as_mut() else { return };
                    if s.args.demo_ms > 0 || s.args.snapshot_path.is_some() {
                        return;
                    }
                    if s.started.elapsed() > Duration::from_secs(90)
                        || s.args.cancel_file.as_ref().is_some_and(|p| p.exists())
                    {
                        should_close = true;
                        return;
                    }
                    if s.done_at.is_some() {
                        return;
                    }

                    if is_pid_alive(s.args.wait_pid) {
                        s.stage = 0;
                        return;
                    }
                    if s.stage < 1 {
                        s.stage = 1;
                        s.target_progress = s.target_progress.max(stage_base_progress(1));
                    }

                    if !s.args.root_dir.as_os_str().is_empty() {
                        let velo_temp = s.args.root_dir.join("packages").join("VelopackTemp");
                        if velo_temp.is_dir() {
                            let sub_count = fs::read_dir(&velo_temp)
                                .ok()
                                .map(|it| {
                                    it.flatten()
                                        .filter(|e| e.file_type().is_ok_and(|t| t.is_dir()))
                                        .count()
                                })
                                .unwrap_or(0);
                            if sub_count >= 1 {
                                s.saw_temp_dir = true;
                            }
                            if sub_count >= 2 || (s.saw_temp_dir && sub_count == 0) {
                                s.saw_swap = true;
                            }
                        } else if s.saw_temp_dir {
                            s.saw_swap = true;
                        }
                    }

                    if s.saw_swap && s.stage < 2 {
                        s.stage = 2;
                        s.target_progress = s.target_progress.max(stage_base_progress(2));
                    }

                    if !is_update_exe_running(&s.args.root_dir) && s.stage >= 1 && s.stage < 3 {
                        s.stage = 3;
                        s.target_progress = s.target_progress.max(stage_base_progress(3));
                    }

                    if is_new_rhpicture_ready(&s.args.root_dir, s.args.wait_pid) {
                        s.stage = 4;
                        s.target_progress = 1.0;
                        s.display_progress = 1.0;
                        s.done_at = Some(Instant::now());
                    }
                });
                if should_close {
                    unsafe {
                        let _ = DestroyWindow(hwnd);
                    }
                }
                return LRESULT(0);
            }
        }
        WM_DESTROY => {
            unsafe {
                let _ = KillTimer(Some(hwnd), TIMER_ANIM);
                let _ = KillTimer(Some(hwnd), TIMER_PROBE);
                PostQuitMessage(0);
            }
            return LRESULT(0);
        }
        _ => {}
    }
    unsafe { DefWindowProcW(hwnd, msg, wparam, lparam) }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn transition_guard_cancels_on_drop_unless_committed() {
        let dir = std::env::temp_dir().join(format!("rhpicture-splash-test-{}", unique_token()));
        fs::create_dir_all(&dir).unwrap();

        let failed = dir.join("cancel-failed");
        {
            let _t = Transition {
                cancel_file: failed.clone(),
                committed: false,
            };
        }
        assert!(failed.is_file());

        let ok = dir.join("cancel-ok");
        {
            let t = Transition {
                cancel_file: ok.clone(),
                committed: false,
            };
            t.commit();
        }
        assert!(!ok.exists());
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn expected_main_path_rejects_unrelated_executables() {
        let root = PathBuf::from(r"C:\Apps\RHPicture");
        assert!(expected_main_path(
            &root.join("current").join("RHPicture.exe"),
            &root
        ));
        assert!(!expected_main_path(
            &root.join("RHPicture.exe"),
            &root
        ));
        assert!(!expected_main_path(
            &PathBuf::from(r"C:\Apps\RHPicture-other\current\RHPicture.exe"),
            &root
        ));
    }
}
