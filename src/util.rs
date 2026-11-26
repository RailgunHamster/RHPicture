#[cfg(windows)]
use crate::config::SaveFormat;
#[cfg(windows)]
use std::path::{Path, PathBuf};
#[cfg(windows)]
use windows::Win32::Foundation::{COLORREF, HINSTANCE, HWND, MAX_PATH};
#[cfg(windows)]
use windows::Win32::Graphics::Gdi::{
    BI_RGB, BITMAPINFO, BITMAPINFOHEADER, CreateCompatibleDC, CreateDIBSection, CreateFontW,
    CreateSolidBrush, DIB_RGB_COLORS, DeleteDC, DeleteObject, FONT_CHARSET, FONT_CLIP_PRECISION,
    FONT_OUTPUT_PRECISION, FONT_QUALITY, HBITMAP, HBRUSH, HDC, HFONT, HGDIOBJ, SelectObject,
};
#[cfg(windows)]
use windows::Win32::System::Com::{
    CLSCTX_INPROC_SERVER, COINIT_APARTMENTTHREADED, CoCreateInstance, CoInitializeEx, CoTaskMemFree,
};
#[cfg(windows)]
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
#[cfg(windows)]
use windows::Win32::System::ProcessStatus::{
    K32EmptyWorkingSet, K32GetProcessMemoryInfo, PROCESS_MEMORY_COUNTERS,
};
#[cfg(windows)]
use windows::Win32::System::Registry::{
    HKEY, HKEY_CURRENT_USER, KEY_READ, KEY_WRITE, REG_SZ, RegCloseKey, RegDeleteValueW,
    RegOpenKeyExW, RegQueryValueExW, RegSetValueExW,
};
#[cfg(windows)]
use windows::Win32::System::SystemInformation::GetLocalTime;
#[cfg(windows)]
use windows::Win32::System::Threading::GetCurrentProcess;
#[cfg(windows)]
use windows::Win32::UI::Controls::Dialogs::{
    GetSaveFileNameW, OFN_NOCHANGEDIR, OFN_OVERWRITEPROMPT, OFN_PATHMUSTEXIST, OPENFILENAMEW,
};
#[cfg(windows)]
use windows::Win32::UI::HiDpi::GetDpiForWindow;
#[cfg(windows)]
use windows::Win32::UI::Shell::{
    FOS_FORCEFILESYSTEM, FOS_PICKFOLDERS, FileOpenDialog, IFileOpenDialog, SIGDN_FILESYSPATH,
    ShellExecuteW,
};
#[cfg(windows)]
use windows::Win32::UI::WindowsAndMessaging::{
    HICON, IMAGE_ICON, LR_DEFAULTSIZE, LR_SHARED, LoadImageW, SW_SHOWNORMAL,
};
#[cfg(windows)]
use windows::core::{PCWSTR, PWSTR};

pub fn to_wide(s: &str) -> Vec<u16> {
    s.encode_utf16().chain(std::iter::once(0)).collect()
}

pub fn from_wide(buf: &[u16]) -> String {
    let len = buf.iter().position(|&c| c == 0).unwrap_or(buf.len());
    String::from_utf16_lossy(&buf[..len])
}

#[cfg(windows)]
pub fn current_hinstance() -> HINSTANCE {
    unsafe { GetModuleHandleW(None).map(HINSTANCE::from).unwrap_or_default() }
}

#[cfg(windows)]
pub fn load_app_icon(size: i32) -> Option<HICON> {
    unsafe {
        let hinst = current_hinstance();
        let res = LoadImageW(
            Some(hinst),
            PCWSTR(1usize as *const u16),
            IMAGE_ICON,
            size,
            size,
            if size == 0 {
                LR_DEFAULTSIZE | LR_SHARED
            } else {
                LR_SHARED
            },
        )
        .ok()?;
        Some(HICON(res.0))
    }
}

#[cfg(windows)]
pub fn window_dpi(hwnd: HWND) -> u32 {
    let dpi = unsafe { GetDpiForWindow(hwnd) };
    if dpi == 0 { 96 } else { dpi }
}

#[cfg(windows)]
pub fn scale_dpi(px: i32, dpi: u32) -> i32 {
    (((px as i64 * dpi as i64) + 48) / 96) as i32
}

#[cfg(windows)]
pub fn create_ui_font(dpi: u32, pt: i32, bold: bool) -> HFONT {
    let height = -((pt * dpi as i32) / 72);
    let face = to_wide("Microsoft YaHei UI");
    let mut lf_face = [0u16; 32];
    let n = face.len().min(32);
    lf_face[..n].copy_from_slice(&face[..n]);
    unsafe {
        CreateFontW(
            height,
            0,
            0,
            0,
            if bold { 600 } else { 400 },
            0,
            0,
            0,
            FONT_CHARSET(1), // DEFAULT_CHARSET
            FONT_OUTPUT_PRECISION(0),
            FONT_CLIP_PRECISION(0),
            FONT_QUALITY(5), // CLEARTYPE_QUALITY
            0,
            PCWSTR(lf_face.as_ptr()),
        )
    }
}

#[cfg(windows)]
pub fn rgb_colorref(r: u8, g: u8, b: u8) -> COLORREF {
    COLORREF((r as u32) | ((g as u32) << 8) | ((b as u32) << 16))
}

#[cfg(windows)]
pub struct DibSurface {
    pub dc: HDC,
    pub bitmap: HBITMAP,
    old: HGDIOBJ,
    pub ptr: *mut u8,
    pub width: i32,
    pub height: i32,
}

#[cfg(windows)]
impl DibSurface {
    pub fn new(width: i32, height: i32) -> Option<Self> {
        if width <= 0 || height <= 0 {
            return None;
        }
        unsafe {
            let dc = CreateCompatibleDC(None);
            if dc.is_invalid() {
                return None;
            }
            let bmi = BITMAPINFO {
                bmiHeader: BITMAPINFOHEADER {
                    biSize: std::mem::size_of::<BITMAPINFOHEADER>() as u32,
                    biWidth: width,
                    biHeight: -height, // top-down BGRA
                    biPlanes: 1,
                    biBitCount: 32,
                    biCompression: BI_RGB.0,
                    ..Default::default()
                },
                ..Default::default()
            };
            let mut bits = std::ptr::null_mut();
            let bitmap = CreateDIBSection(Some(dc), &bmi, DIB_RGB_COLORS, &mut bits, None, 0).ok()?;
            if bitmap.is_invalid() || bits.is_null() {
                let _ = DeleteDC(dc);
                return None;
            }
            let old = SelectObject(dc, HGDIOBJ(bitmap.0));
            Some(Self {
                dc,
                bitmap,
                old,
                ptr: bits as *mut u8,
                width,
                height,
            })
        }
    }

    pub fn pixels_mut(&mut self) -> &mut [u8] {
        let len = (self.width as usize) * (self.height as usize) * 4;
        unsafe { std::slice::from_raw_parts_mut(self.ptr, len) }
    }
}

#[cfg(windows)]
impl Drop for DibSurface {
    fn drop(&mut self) {
        unsafe {
            if !self.dc.is_invalid() {
                let _ = SelectObject(self.dc, self.old);
            }
            if !self.bitmap.is_invalid() {
                let _ = DeleteObject(HGDIOBJ(self.bitmap.0));
            }
            if !self.dc.is_invalid() {
                let _ = DeleteDC(self.dc);
            }
        }
    }
}

#[cfg(windows)]
pub struct OwnedBrush(pub HBRUSH);
#[cfg(windows)]
impl OwnedBrush {
    pub fn solid(r: u8, g: u8, b: u8) -> Self {
        Self(unsafe { CreateSolidBrush(rgb_colorref(r, g, b)) })
    }
}
#[cfg(windows)]
impl Drop for OwnedBrush {
    fn drop(&mut self) {
        unsafe {
            if !self.0.is_invalid() {
                let _ = DeleteObject(HGDIOBJ(self.0.0));
            }
        }
    }
}

#[cfg(windows)]
pub struct OwnedFont(pub HFONT);
#[cfg(windows)]
impl Drop for OwnedFont {
    fn drop(&mut self) {
        unsafe {
            if !self.0.is_invalid() {
                let _ = DeleteObject(HGDIOBJ(self.0.0));
            }
        }
    }
}

#[cfg(windows)]
pub fn process_memory_mb() -> f64 {
    unsafe {
        let mut counters = PROCESS_MEMORY_COUNTERS::default();
        counters.cb = std::mem::size_of::<PROCESS_MEMORY_COUNTERS>() as u32;
        if K32GetProcessMemoryInfo(GetCurrentProcess(), &mut counters, counters.cb).as_bool() {
            return counters.WorkingSetSize as f64 / (1024.0 * 1024.0);
        }
    }
    0.0
}

#[cfg(windows)]
pub fn trim_working_set() {
    unsafe {
        let _ = K32EmptyWorkingSet(GetCurrentProcess());
    }
}

#[cfg(windows)]
pub fn timestamp_filename(ext: &str) -> String {
    let st = unsafe { GetLocalTime() };
    format!(
        "RHPicture_{:04}{:02}{:02}_{:02}{:02}{:02}.{}",
        st.wYear, st.wMonth, st.wDay, st.wHour, st.wMinute, st.wSecond, ext
    )
}

#[cfg(windows)]
pub fn set_auto_start(enabled: bool) -> Result<(), String> {
    const RUN_KEY: &str = "Software\\Microsoft\\Windows\\CurrentVersion\\Run";
    const APP_NAME: &str = "RHPicture";
    let key_w = to_wide(RUN_KEY);
    let name_w = to_wide(APP_NAME);
    unsafe {
        let mut hkey = HKEY::default();
        RegOpenKeyExW(
            HKEY_CURRENT_USER,
            PCWSTR(key_w.as_ptr()),
            Some(0),
            KEY_WRITE,
            &mut hkey,
        )
        .ok()
        .map_err(|e| format!("无法打开启动项注册表：{e}"))?;

        let result = if enabled {
            let exe = std::env::current_exe().map_err(|e| format!("获取程序路径失败：{e}"))?;
            let val = format!("\"{}\"", exe.display());
            let val_w = to_wide(&val);
            let bytes =
                std::slice::from_raw_parts(val_w.as_ptr() as *const u8, val_w.len() * 2);
            RegSetValueExW(hkey, PCWSTR(name_w.as_ptr()), Some(0), REG_SZ, Some(bytes))
                .ok()
                .map_err(|e| format!("写入开机自启失败：{e}"))
        } else {
            let _ = RegDeleteValueW(hkey, PCWSTR(name_w.as_ptr()));
            Ok(())
        };
        let _ = RegCloseKey(hkey);
        result
    }
}

#[cfg(windows)]
pub fn get_auto_start() -> bool {
    const RUN_KEY: &str = "Software\\Microsoft\\Windows\\CurrentVersion\\Run";
    const APP_NAME: &str = "RHPicture";
    let key_w = to_wide(RUN_KEY);
    let name_w = to_wide(APP_NAME);
    unsafe {
        let mut hkey = HKEY::default();
        if RegOpenKeyExW(
            HKEY_CURRENT_USER,
            PCWSTR(key_w.as_ptr()),
            Some(0),
            KEY_READ,
            &mut hkey,
        )
        .is_err()
        {
            return false;
        }
        let mut size = 0u32;
        let exists = RegQueryValueExW(
            hkey,
            PCWSTR(name_w.as_ptr()),
            None,
            None,
            None,
            Some(&mut size),
        )
        .is_ok();
        let _ = RegCloseKey(hkey);
        exists
    }
}

#[cfg(windows)]
pub fn save_file_dialog(
    owner: Option<HWND>,
    default_dir: &Path,
    default_format: SaveFormat,
) -> Option<(PathBuf, SaveFormat)> {
    let _ = std::fs::create_dir_all(default_dir);
    let default_name = timestamp_filename(default_format.extension());

    let mut file_buf = vec![0u16; MAX_PATH as usize * 4];
    let name_w = to_wide(&default_name);
    let copy_len = name_w.len().min(file_buf.len() - 1);
    file_buf[..copy_len].copy_from_slice(&name_w[..copy_len]);

    let filter: Vec<u16> = "PNG 图像 (*.png)\0*.png\0JPEG 图像 (*.jpg)\0*.jpg;*.jpeg\0BMP 位图 (*.bmp)\0*.bmp\0\0"
        .encode_utf16()
        .collect();
    let dir_w = to_wide(&default_dir.to_string_lossy());
    let def_ext_w = to_wide(default_format.extension());
    let title_w = to_wide("保存截图");

    let mut ofn = OPENFILENAMEW {
        lStructSize: std::mem::size_of::<OPENFILENAMEW>() as u32,
        hwndOwner: owner.unwrap_or_default(),
        lpstrFilter: PCWSTR(filter.as_ptr()),
        nFilterIndex: (default_format.index() + 1) as u32,
        lpstrFile: PWSTR(file_buf.as_mut_ptr()),
        nMaxFile: file_buf.len() as u32,
        lpstrInitialDir: PCWSTR(dir_w.as_ptr()),
        lpstrTitle: PCWSTR(title_w.as_ptr()),
        Flags: OFN_OVERWRITEPROMPT | OFN_PATHMUSTEXIST | OFN_NOCHANGEDIR,
        lpstrDefExt: PCWSTR(def_ext_w.as_ptr()),
        ..Default::default()
    };

    if unsafe { GetSaveFileNameW(&mut ofn) }.as_bool() {
        let chosen = PathBuf::from(from_wide(&file_buf));
        let filter_fmt = SaveFormat::from_index(ofn.nFilterIndex.saturating_sub(1) as usize);
        let fmt = SaveFormat::from_path(&chosen, filter_fmt);
        Some((chosen, fmt))
    } else {
        None
    }
}

#[cfg(windows)]
pub fn browse_folder_dialog(owner: Option<HWND>) -> Option<PathBuf> {
    unsafe {
        let _ = CoInitializeEx(None, COINIT_APARTMENTTHREADED);
        let dialog: IFileOpenDialog =
            CoCreateInstance(&FileOpenDialog, None, CLSCTX_INPROC_SERVER).ok()?;
        let opts = dialog.GetOptions().ok()?;
        dialog
            .SetOptions(opts | FOS_PICKFOLDERS | FOS_FORCEFILESYSTEM)
            .ok()?;
        let title = to_wide("选择截图默认保存文件夹");
        let _ = dialog.SetTitle(PCWSTR(title.as_ptr()));
        dialog.Show(owner).ok()?;
        let item = dialog.GetResult().ok()?;
        let raw = item.GetDisplayName(SIGDN_FILESYSPATH).ok()?;
        let path_str = raw.to_string().ok();
        CoTaskMemFree(Some(raw.0 as *const _));
        path_str.map(PathBuf::from)
    }
}

#[cfg(windows)]
pub fn open_https_url(url_str: &str) {
    let Ok(parsed) = url::Url::parse(url_str) else {
        return;
    };
    if parsed.scheme() != "https" {
        return;
    }
    let verb = to_wide("open");
    let target = to_wide(parsed.as_str());
    unsafe {
        ShellExecuteW(
            None,
            PCWSTR(verb.as_ptr()),
            PCWSTR(target.as_ptr()),
            PCWSTR::null(),
            PCWSTR::null(),
            SW_SHOWNORMAL,
        );
    }
}
