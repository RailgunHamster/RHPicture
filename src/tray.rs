use crate::config::AppConfig;
use crate::hotkey::{
    HOTKEY_ID_PIN_CLIPBOARD, HOTKEY_ID_SCREENSHOT, HOTKEY_ID_TOGGLE_PINS, register_from_config,
    unregister_all,
};
use crate::image_ops::{BgraImage, clipboard};
use crate::overlay::{OverlayOutcome, start_screenshot_overlay};
use crate::pin;
use crate::settings_ui::{self, SettingsCallbacks};
use crate::updater::UpdateService;
use crate::util::{
    current_hinstance, load_app_icon, save_file_dialog, timestamp_filename, to_wide,
    trim_working_set,
};
use std::cell::RefCell;
use std::sync::Arc;
use windows::Win32::Foundation::{HWND, LPARAM, LRESULT, POINT, WPARAM};
use windows::Win32::UI::Shell::{
    NIF_ICON, NIF_INFO, NIF_MESSAGE, NIF_TIP, NIIF_INFO, NIIF_WARNING, NIM_ADD, NIM_DELETE,
    NIM_MODIFY, NOTIFYICONDATAW, Shell_NotifyIconW,
};
use windows::Win32::UI::WindowsAndMessaging::{
    AppendMenuW, CreatePopupMenu, CreateWindowExW, DefWindowProcW, DestroyMenu, DestroyWindow,
    DispatchMessageW, GetCursorPos, GetMessageW, GetSystemMetrics, MF_GRAYED, MF_POPUP,
    MF_SEPARATOR, MF_STRING, MSG, PostMessageW, PostQuitMessage, RegisterClassExW,
    RegisterWindowMessageW, SM_CXSCREEN, SM_CYSCREEN, SetForegroundWindow, SetMenuDefaultItem,
    TPM_BOTTOMALIGN, TPM_LEFTALIGN, TPM_RETURNCMD, TrackPopupMenu, TranslateMessage, WM_APP,
    WM_DESTROY, WM_HOTKEY, WM_LBUTTONDBLCLK, WM_LBUTTONUP, WM_RBUTTONUP, WNDCLASSEXW,
    WS_OVERLAPPEDWINDOW,
};
use windows::core::PCWSTR;

pub const TRAY_CLASS: &str = "RHPictureTrayClass";
pub const WM_APP_TRAY: u32 = WM_APP + 1;
pub const WM_APP_SHOW_SETTINGS: u32 = WM_APP + 2;
pub const WM_APP_UPDATE_STATUS: u32 = WM_APP + 3;
pub const WM_APP_EXIT_FOR_UPDATE: u32 = WM_APP + 4;

const TRAY_UID: u32 = 1;

const MENU_SCREENSHOT: u32 = 1001;
const MENU_PIN_CLIPBOARD: u32 = 1002;
const MENU_HIDE_ALL_PINS: u32 = 1003;
const MENU_SHOW_ALL_PINS: u32 = 1004;
const MENU_DISABLE_CLICKTHROUGH: u32 = 1005;
const MENU_CLOSE_ALL_PINS: u32 = 1006;
const MENU_SETTINGS: u32 = 1007;
const MENU_CHECK_UPDATE: u32 = 1008;
const MENU_EXIT: u32 = 1009;
const MENU_RESTORE_PIN_BASE: u32 = 2000;

struct AppState {
    hwnd: HWND,
    config: AppConfig,
    updater: UpdateService,
    taskbar_created_msg: u32,
}

thread_local! {
    static APP: RefCell<Option<AppState>> = const { RefCell::new(None) };
}

fn fill_wide_array<const N: usize>(dst: &mut [u16; N], text: &str) {
    dst.fill(0);
    for (i, ch) in text.encode_utf16().take(N - 1).enumerate() {
        dst[i] = ch;
    }
}

fn add_tray_icon(hwnd: HWND, tip: &str) {
    let mut nid = NOTIFYICONDATAW {
        cbSize: std::mem::size_of::<NOTIFYICONDATAW>() as u32,
        hWnd: hwnd,
        uID: TRAY_UID,
        uFlags: NIF_ICON | NIF_MESSAGE | NIF_TIP,
        uCallbackMessage: WM_APP_TRAY,
        hIcon: load_app_icon(16).unwrap_or_default(),
        ..Default::default()
    };
    fill_wide_array(&mut nid.szTip, tip);
    unsafe {
        let _ = Shell_NotifyIconW(NIM_ADD, &nid);
    }
}

fn remove_tray_icon(hwnd: HWND) {
    let nid = NOTIFYICONDATAW {
        cbSize: std::mem::size_of::<NOTIFYICONDATAW>() as u32,
        hWnd: hwnd,
        uID: TRAY_UID,
        ..Default::default()
    };
    unsafe {
        let _ = Shell_NotifyIconW(NIM_DELETE, &nid);
    }
}

pub fn show_balloon(hwnd: HWND, title: &str, body: &str, warning: bool) {
    let mut nid = NOTIFYICONDATAW {
        cbSize: std::mem::size_of::<NOTIFYICONDATAW>() as u32,
        hWnd: hwnd,
        uID: TRAY_UID,
        uFlags: NIF_INFO,
        dwInfoFlags: if warning { NIIF_WARNING } else { NIIF_INFO },
        ..Default::default()
    };
    fill_wide_array(&mut nid.szInfoTitle, title);
    fill_wide_array(&mut nid.szInfo, body);
    unsafe {
        let _ = Shell_NotifyIconW(NIM_MODIFY, &nid);
    }
}

fn tray_tooltip(cfg: &AppConfig) -> String {
    if cfg.screenshot_hotkey.trim().is_empty() {
        "RHPicture（点击打开设置）".into()
    } else {
        format!("RHPicture — 截图：{}", cfg.screenshot_hotkey)
    }
}

fn update_tray_tooltip(hwnd: HWND, cfg: &AppConfig) {
    let mut nid = NOTIFYICONDATAW {
        cbSize: std::mem::size_of::<NOTIFYICONDATAW>() as u32,
        hWnd: hwnd,
        uID: TRAY_UID,
        uFlags: NIF_TIP,
        ..Default::default()
    };
    fill_wide_array(&mut nid.szTip, &tray_tooltip(cfg));
    unsafe {
        let _ = Shell_NotifyIconW(NIM_MODIFY, &nid);
    }
}

fn handle_save_image(owner: HWND, cfg: &AppConfig, img: &BgraImage) {
    if cfg.quick_save_no_dialog {
        let dir = cfg.resolved_save_dir();
        let path = dir.join(timestamp_filename(cfg.save_format.extension()));
        match img.save_to_file(&path, cfg.save_format) {
            Ok(()) => show_balloon(
                owner,
                "截图已保存",
                &format!("已保存到：{}", path.display()),
                false,
            ),
            Err(err) => show_balloon(owner, "保存截图失败", &err, true),
        }
    } else if let Some((path, fmt)) =
        save_file_dialog(Some(owner), &cfg.resolved_save_dir(), cfg.save_format)
    {
        match img.save_to_file(&path, fmt) {
            Ok(()) => show_balloon(
                owner,
                "截图已保存",
                &format!("已保存到：{}", path.display()),
                false,
            ),
            Err(err) => show_balloon(owner, "保存截图失败", &err, true),
        }
    }
}

fn trigger_screenshot() {
    let (hwnd, smart, mag) = APP.with(|cell| {
        let borrow = cell.borrow();
        let Some(app) = borrow.as_ref() else {
            return (HWND::default(), true, true);
        };
        (
            app.hwnd,
            app.config.smart_window_detect,
            app.config.show_magnifier,
        )
    });
    if hwnd.is_invalid() {
        return;
    }

    if let Err(err) = start_screenshot_overlay(smart, mag, move |outcome| match outcome {
        OverlayOutcome::Copy(img) => {
            if let Err(e) = clipboard::copy_image(&img) {
                show_balloon(hwnd, "复制失败", &e, true);
            }
        }
        OverlayOutcome::Save(img) => {
            let cfg = APP.with(|c| {
                c.borrow()
                    .as_ref()
                    .map(|a| a.config.clone())
                    .unwrap_or_default()
            });
            handle_save_image(hwnd, &cfg, &img);
        }
        OverlayOutcome::Pin {
            image,
            screen_x,
            screen_y,
        } => {
            if let Err(e) = pin::create_pin(image, screen_x, screen_y) {
                show_balloon(hwnd, "贴图失败", &e, true);
            }
        }
        OverlayOutcome::CopyColor(hex) => {
            if clipboard::copy_text(&hex).is_ok() {
                show_balloon(hwnd, "已复制颜色", &format!("色值 {hex} 已复制到剪贴板"), false);
            }
        }
        OverlayOutcome::Cancel => {}
    }) {
        show_balloon(hwnd, "截图失败", &err, true);
    }
}

fn trigger_pin_from_clipboard() {
    let hwnd = APP.with(|c| c.borrow().as_ref().map(|a| a.hwnd).unwrap_or_default());
    match clipboard::read_image() {
        Ok(Some(img)) => {
            let mut pt = POINT::default();
            unsafe {
                let _ = GetCursorPos(&mut pt);
            }
            let sw = unsafe { GetSystemMetrics(SM_CXSCREEN) };
            let sh = unsafe { GetSystemMetrics(SM_CYSCREEN) };
            let x = (pt.x - (img.width as i32) / 2).clamp(20, (sw - 100).max(20));
            let y = (pt.y - (img.height as i32) / 2).clamp(20, (sh - 100).max(20));
            if let Err(err) = pin::create_pin(img, x, y) {
                show_balloon(hwnd, "贴图失败", &err, true);
            }
        }
        Ok(None) => {
            show_balloon(hwnd, "剪贴板为空", "当前剪贴板中没有可贴图的图像", false);
        }
        Err(err) => {
            show_balloon(hwnd, "读取剪贴板失败", &err, true);
        }
    }
}

fn open_settings_window(initial_tab: Option<usize>) {
    let extracted = APP.with(|cell| {
        let borrow = cell.borrow();
        let app = borrow.as_ref()?;
        Some((app.hwnd, app.config.clone(), app.updater.clone()))
    });
    let Some((tray_hwnd, cfg, updater)) = extracted else {
        return;
    };

    let tray_raw = tray_hwnd.0 as isize;
    let callbacks = SettingsCallbacks {
        on_config_saved: Box::new(move |new_cfg| {
            let warnings = register_from_config(tray_hwnd, &new_cfg);
            update_tray_tooltip(tray_hwnd, &new_cfg);
            pin::sync_config(&new_cfg);
            APP.with(|cell| {
                if let Some(app) = cell.borrow_mut().as_mut() {
                    app.config = new_cfg;
                }
            });
            warnings
        }),
        on_suspend_hotkeys: Box::new(move |suspend| {
            if suspend {
                unregister_all(tray_hwnd);
            } else {
                APP.with(|cell| {
                    if let Some(app) = cell.borrow().as_ref() {
                        let _ = register_from_config(tray_hwnd, &app.config);
                    }
                });
            }
        }),
        on_exit_for_update: Arc::new(move || unsafe {
            let _ = PostMessageW(
                Some(HWND(tray_raw as *mut _)),
                WM_APP_EXIT_FOR_UPDATE,
                WPARAM(0),
                LPARAM(0),
            );
        }),
    };

    settings_ui::open_settings(&cfg, updater, initial_tab, callbacks);
}

fn show_tray_menu(hwnd: HWND) {
    let cfg = APP.with(|c| {
        c.borrow()
            .as_ref()
            .map(|a| a.config.clone())
            .unwrap_or_default()
    });
    let pins = pin::list_pins();
    let visible_count = pins.iter().filter(|p| !p.hidden).count();
    let hidden_pins: Vec<_> = pins.iter().filter(|p| p.hidden).cloned().collect();
    let has_click_through = pins.iter().any(|p| p.click_through);

    let cmd = unsafe {
        let Ok(menu) = CreatePopupMenu() else {
            return;
        };
        let add = |hmenu, id: u32, text: &str, disabled: bool| {
            let w = to_wide(text);
            let flags = if disabled {
                MF_STRING | MF_GRAYED
            } else {
                MF_STRING
            };
            let _ = AppendMenuW(hmenu, flags, id as usize, PCWSTR(w.as_ptr()));
        };

        let shot_label = if cfg.screenshot_hotkey.is_empty() {
            "开始截图".to_string()
        } else {
            format!("开始截图\t{}", cfg.screenshot_hotkey)
        };
        let pin_label = if cfg.pin_clipboard_hotkey.is_empty() {
            "贴图剪贴板图像".to_string()
        } else {
            format!("贴图剪贴板图像\t{}", cfg.pin_clipboard_hotkey)
        };

        add(menu, MENU_SCREENSHOT, &shot_label, false);
        add(menu, MENU_PIN_CLIPBOARD, &pin_label, false);
        let _ = AppendMenuW(menu, MF_SEPARATOR, 0, PCWSTR::null());

        if visible_count > 0 {
            add(
                menu,
                MENU_HIDE_ALL_PINS,
                &format!("隐藏全部贴图（{visible_count}）"),
                false,
            );
        }
        if !hidden_pins.is_empty() {
            add(
                menu,
                MENU_SHOW_ALL_PINS,
                &format!("显示全部已隐藏贴图（{}）", hidden_pins.len()),
                false,
            );
            if let Ok(sub) = CreatePopupMenu() {
                for (idx, p) in hidden_pins.iter().enumerate() {
                    let label = format!("贴图 #{}（{} × {}）", p.id, p.width, p.height);
                    add(sub, MENU_RESTORE_PIN_BASE + idx as u32, &label, false);
                }
                let sub_title = to_wide("恢复单个隐藏贴图");
                let _ = AppendMenuW(
                    menu,
                    MF_POPUP,
                    sub.0 as usize,
                    PCWSTR(sub_title.as_ptr()),
                );
            }
        }
        if has_click_through {
            add(menu, MENU_DISABLE_CLICKTHROUGH, "取消全部鼠标穿透", false);
        }
        add(
            menu,
            MENU_CLOSE_ALL_PINS,
            &format!("关闭全部贴图（{}）", pins.len()),
            pins.is_empty(),
        );

        let _ = AppendMenuW(menu, MF_SEPARATOR, 0, PCWSTR::null());
        add(menu, MENU_SETTINGS, "设置与快捷键…", false);
        add(menu, MENU_CHECK_UPDATE, "检查更新…", false);
        let _ = AppendMenuW(menu, MF_SEPARATOR, 0, PCWSTR::null());
        add(menu, MENU_EXIT, "退出 RHPicture", false);

        let _ = SetMenuDefaultItem(menu, MENU_SETTINGS, 0);

        let mut pt = POINT::default();
        let _ = GetCursorPos(&mut pt);
        let _ = SetForegroundWindow(hwnd);
        let chosen = TrackPopupMenu(
            menu,
            TPM_LEFTALIGN | TPM_BOTTOMALIGN | TPM_RETURNCMD,
            pt.x,
            pt.y,
            Some(0),
            hwnd,
            None,
        )
        .0 as u32;
        let _ = DestroyMenu(menu);
        chosen
    };

    match cmd {
        MENU_SCREENSHOT => trigger_screenshot(),
        MENU_PIN_CLIPBOARD => trigger_pin_from_clipboard(),
        MENU_HIDE_ALL_PINS => pin::hide_all_pins(),
        MENU_SHOW_ALL_PINS => pin::show_all_pins(),
        MENU_DISABLE_CLICKTHROUGH => pin::disable_all_click_through(),
        MENU_CLOSE_ALL_PINS => pin::close_all_pins(),
        MENU_SETTINGS => open_settings_window(Some(0)),
        MENU_CHECK_UPDATE => {
            open_settings_window(Some(2));
            APP.with(|cell| {
                if let Some(app) = cell.borrow().as_ref() {
                    app.updater.start_check_async();
                }
            });
        }
        MENU_EXIT => {
            shutdown_app(hwnd);
        }
        id if id >= MENU_RESTORE_PIN_BASE => {
            let idx = (id - MENU_RESTORE_PIN_BASE) as usize;
            if let Some(p) = hidden_pins.get(idx) {
                pin::show_pin_by_id(p.id);
            }
        }
        _ => {}
    }
}

fn shutdown_app(hwnd: HWND) {
    settings_ui::close_settings_if_open();
    pin::close_all_pins();
    unregister_all(hwnd);
    remove_tray_icon(hwnd);
    unsafe {
        let _ = DestroyWindow(hwnd);
    }
}

unsafe extern "system" fn tray_wnd_proc(
    hwnd: HWND,
    msg: u32,
    wparam: WPARAM,
    lparam: LPARAM,
) -> LRESULT {
    let taskbar_msg = APP.with(|c| c.borrow().as_ref().map(|a| a.taskbar_created_msg).unwrap_or(0));
    if taskbar_msg != 0 && msg == taskbar_msg {
        let tip = APP.with(|c| {
            c.borrow()
                .as_ref()
                .map(|a| tray_tooltip(&a.config))
                .unwrap_or_else(|| "RHPicture".into())
        });
        add_tray_icon(hwnd, &tip);
        return LRESULT(0);
    }

    match msg {
        WM_HOTKEY => {
            match wparam.0 as i32 {
                HOTKEY_ID_SCREENSHOT => trigger_screenshot(),
                HOTKEY_ID_PIN_CLIPBOARD => trigger_pin_from_clipboard(),
                HOTKEY_ID_TOGGLE_PINS => pin::toggle_all_pins(),
                _ => {}
            }
            return LRESULT(0);
        }
        WM_APP_TRAY => {
            let event = (lparam.0 & 0xFFFF) as u32;
            match event {
                WM_RBUTTONUP => show_tray_menu(hwnd),
                WM_LBUTTONDBLCLK | WM_LBUTTONUP => open_settings_window(None),
                _ => {}
            }
            return LRESULT(0);
        }
        WM_APP_SHOW_SETTINGS => {
            open_settings_window(None);
            return LRESULT(0);
        }
        WM_APP_UPDATE_STATUS => {
            settings_ui::notify_update_changed();
            return LRESULT(0);
        }
        WM_APP_EXIT_FOR_UPDATE => {
            shutdown_app(hwnd);
            return LRESULT(0);
        }
        WM_DESTROY => {
            unsafe {
                PostQuitMessage(0);
            }
            return LRESULT(0);
        }
        _ => {}
    }
    unsafe { DefWindowProcW(hwnd, msg, wparam, lparam) }
}

pub fn run_app(open_settings_on_start: bool, post_update: bool) -> Result<(), String> {
    let first_run = !AppConfig::config_path(&crate::config::data_dir()).exists();
    let config = AppConfig::load();
    if first_run {
        let _ = config.save();
    }
    pin::sync_config(&config);

    let class_w = to_wide(TRAY_CLASS);
    let title_w = to_wide("RHPicture");
    unsafe {
        let wc = WNDCLASSEXW {
            cbSize: std::mem::size_of::<WNDCLASSEXW>() as u32,
            lpfnWndProc: Some(tray_wnd_proc),
            hInstance: current_hinstance(),
            lpszClassName: PCWSTR(class_w.as_ptr()),
            ..Default::default()
        };
        let _ = RegisterClassExW(&wc);
    }

    let hwnd = unsafe {
        CreateWindowExW(
            Default::default(),
            PCWSTR(class_w.as_ptr()),
            PCWSTR(title_w.as_ptr()),
            WS_OVERLAPPEDWINDOW,
            0,
            0,
            0,
            0,
            None,
            None,
            Some(current_hinstance()),
            None,
        )
    }
    .map_err(|e| format!("创建主消息窗口失败：{e}"))?;

    let taskbar_w = to_wide("TaskbarCreated");
    let taskbar_created_msg = unsafe { RegisterWindowMessageW(PCWSTR(taskbar_w.as_ptr())) };

    let hwnd_raw = hwnd.0 as isize;
    let notify = Arc::new(move || unsafe {
        let _ = PostMessageW(
            Some(HWND(hwnd_raw as *mut _)),
            WM_APP_UPDATE_STATUS,
            WPARAM(0),
            LPARAM(0),
        );
    });
    let updater = UpdateService::new(config.update.clone(), Some(notify));

    add_tray_icon(hwnd, &tray_tooltip(&config));
    let warnings = register_from_config(hwnd, &config);

    let auto_check = config.update.auto_check_on_start;
    let tip_hk = config.screenshot_hotkey.clone();

    APP.with(|cell| {
        *cell.borrow_mut() = Some(AppState {
            hwnd,
            config,
            updater: updater.clone(),
            taskbar_created_msg,
        });
    });

    if !warnings.is_empty() {
        show_balloon(hwnd, "快捷键提示", &warnings.join("；"), true);
    } else if post_update {
        show_balloon(
            hwnd,
            "RHPicture 更新完成",
            &format!("已升级至 v{}，快捷键与设置已自动保留", crate::updater::APP_VERSION),
            false,
        );
    } else if first_run {
        show_balloon(
            hwnd,
            "RHPicture 已启动",
            &format!("按 {} 开始截图；点击托盘图标可打开设置", if tip_hk.is_empty() { "托盘菜单" } else { &tip_hk }),
            false,
        );
    }

    if open_settings_on_start || first_run {
        open_settings_window(Some(0));
    }
    if auto_check {
        updater.start_check_async();
    }

    trim_working_set();

    let mut msg = MSG::default();
    unsafe {
        while GetMessageW(&mut msg, None, 0, 0).into() {
            let _ = TranslateMessage(&msg);
            DispatchMessageW(&msg);
        }
    }

    APP.with(|cell| {
        *cell.borrow_mut() = None;
    });
    Ok(())
}
