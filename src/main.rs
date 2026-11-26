#![cfg_attr(target_os = "windows", windows_subsystem = "windows")]

#[cfg(windows)]
fn acquire_single_instance() -> Option<windows::Win32::Foundation::HANDLE> {
    use rhpicture::tray::{TRAY_CLASS, WM_APP_SHOW_SETTINGS};
    use rhpicture::util::to_wide;
    use windows::Win32::Foundation::{ERROR_ALREADY_EXISTS, GetLastError, LPARAM, WPARAM};
    use windows::Win32::System::Threading::CreateMutexW;
    use windows::Win32::UI::WindowsAndMessaging::{FindWindowW, PostMessageW};
    use windows::core::PCWSTR;

    let name = to_wide("Local\\RHPicture_SingleInstance_Mutex");
    let handle = unsafe { CreateMutexW(None, false, PCWSTR(name.as_ptr())) }.ok()?;
    if unsafe { GetLastError() } == ERROR_ALREADY_EXISTS {
        let class_w = to_wide(TRAY_CLASS);
        if let Ok(existing) = unsafe { FindWindowW(PCWSTR(class_w.as_ptr()), PCWSTR::null()) } {
            if !existing.is_invalid() {
                unsafe {
                    let _ = PostMessageW(Some(existing), WM_APP_SHOW_SETTINGS, WPARAM(0), LPARAM(0));
                }
            }
        }
        unsafe {
            let _ = windows::Win32::Foundation::CloseHandle(handle);
        }
        return None;
    }
    Some(handle)
}

#[cfg(windows)]
fn main() {
    use windows::Win32::Foundation::CloseHandle;
    use windows::Win32::UI::HiDpi::{
        DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2, SetProcessDpiAwarenessContext,
    };

    unsafe {
        let _ = SetProcessDpiAwarenessContext(DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2);
    }

    // Independent update transition splash runs outside the installation root and bypasses Velopack/mutex.
    if let Some(splash_args) = rhpicture::update_splash::SplashArgs::parse_from_env() {
        rhpicture::update_splash::run_splash_process(splash_args);
        return;
    }

    // Velopack hooks must execute before any application initialization.
    velopack::VelopackApp::build().run();
    rhpicture::update_splash::cleanup_stale_transitions();

    let Some(mutex) = acquire_single_instance() else {
        return;
    };

    let open_settings = std::env::args().any(|a| a == "--settings" || a == "-s");
    let post_update = std::env::args().any(|a| a == "--post-update");
    if let Err(err) = rhpicture::tray::run_app(open_settings, post_update) {
        eprintln!("RHPicture error: {err}");
    }

    unsafe {
        let _ = CloseHandle(mutex);
    }
}

#[cfg(not(windows))]
fn main() {
    eprintln!("RHPicture currently targets Windows.");
}
