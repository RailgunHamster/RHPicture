use crate::config::SaveFormat;
use image::{ImageFormat, RgbaImage};
use std::io::Cursor;
use std::path::Path;

/// Compact top-down 32-bit BGRA image buffer matching Win32 top-down DIBSection layout.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct BgraImage {
    pub width: u32,
    pub height: u32,
    pub data: Vec<u8>,
}

impl BgraImage {
    pub fn new_solid(width: u32, height: u32, b: u8, g: u8, r: u8, a: u8) -> Self {
        let count = (width as usize).saturating_mul(height as usize);
        let mut data = Vec::with_capacity(count * 4);
        for _ in 0..count {
            data.extend_from_slice(&[b, g, r, a]);
        }
        Self {
            width,
            height,
            data,
        }
    }

    pub fn from_bgra(width: u32, height: u32, data: Vec<u8>) -> Result<Self, String> {
        let expected = (width as usize)
            .checked_mul(height as usize)
            .and_then(|n| n.checked_mul(4))
            .ok_or_else(|| "图像尺寸溢出".to_string())?;
        if width == 0 || height == 0 || data.len() != expected {
            return Err("图像缓冲区大小不匹配".into());
        }
        Ok(Self {
            width,
            height,
            data,
        })
    }

    pub fn pixel_rgb(&self, x: i32, y: i32) -> Option<(u8, u8, u8)> {
        if x < 0 || y < 0 || x >= self.width as i32 || y >= self.height as i32 {
            return None;
        }
        let idx = ((y as u32 * self.width + x as u32) * 4) as usize;
        Some((self.data[idx + 2], self.data[idx + 1], self.data[idx]))
    }

    pub fn crop(&self, x: i32, y: i32, w: i32, h: i32) -> Option<Self> {
        if w <= 0 || h <= 0 {
            return None;
        }
        let x0 = x.clamp(0, self.width as i32) as u32;
        let y0 = y.clamp(0, self.height as i32) as u32;
        let x1 = (x + w).clamp(0, self.width as i32) as u32;
        let y1 = (y + h).clamp(0, self.height as i32) as u32;
        if x1 <= x0 || y1 <= y0 {
            return None;
        }
        let cw = x1 - x0;
        let ch = y1 - y0;
        let mut out = vec![0u8; (cw as usize) * (ch as usize) * 4];
        let src_stride = (self.width as usize) * 4;
        let dst_stride = (cw as usize) * 4;
        for row in 0..ch as usize {
            let src_off = (y0 as usize + row) * src_stride + (x0 as usize) * 4;
            let dst_off = row * dst_stride;
            out[dst_off..dst_off + dst_stride]
                .copy_from_slice(&self.data[src_off..src_off + dst_stride]);
        }
        Some(Self {
            width: cw,
            height: ch,
            data: out,
        })
    }

    pub fn rotate_90_cw(&self) -> Self {
        let new_w = self.height;
        let new_h = self.width;
        let mut out = vec![0u8; self.data.len()];
        for y in 0..self.height {
            for x in 0..self.width {
                let src = ((y * self.width + x) * 4) as usize;
                let nx = self.height - 1 - y;
                let ny = x;
                let dst = ((ny * new_w + nx) * 4) as usize;
                out[dst..dst + 4].copy_from_slice(&self.data[src..src + 4]);
            }
        }
        Self {
            width: new_w,
            height: new_h,
            data: out,
        }
    }

    pub fn to_rgba_image(&self) -> RgbaImage {
        let mut rgba = vec![0u8; self.data.len()];
        for (src, dst) in self.data.chunks_exact(4).zip(rgba.chunks_exact_mut(4)) {
            dst[0] = src[2];
            dst[1] = src[1];
            dst[2] = src[0];
            dst[3] = src[3];
        }
        RgbaImage::from_raw(self.width, self.height, rgba)
            .expect("BgraImage dimensions always match buffer size")
    }

    pub fn from_rgba_image(img: RgbaImage) -> Self {
        let width = img.width();
        let height = img.height();
        let raw = img.into_raw();
        let mut bgra = vec![0u8; raw.len()];
        for (src, dst) in raw.chunks_exact(4).zip(bgra.chunks_exact_mut(4)) {
            dst[0] = src[2];
            dst[1] = src[1];
            dst[2] = src[0];
            dst[3] = src[3];
        }
        Self {
            width,
            height,
            data: bgra,
        }
    }

    pub fn to_png_bytes(&self) -> Result<Vec<u8>, String> {
        let rgba = self.to_rgba_image();
        let mut out = Cursor::new(Vec::new());
        image::DynamicImage::ImageRgba8(rgba)
            .write_to(&mut out, ImageFormat::Png)
            .map_err(|e| format!("PNG 编码失败：{e}"))?;
        Ok(out.into_inner())
    }

    pub fn from_encoded_bytes(bytes: &[u8]) -> Result<Self, String> {
        let decoded = image::load_from_memory(bytes)
            .map_err(|e| format!("图像解码失败：{e}"))?
            .to_rgba8();
        Ok(Self::from_rgba_image(decoded))
    }

    pub fn save_to_file(&self, path: &Path, format: SaveFormat) -> Result<(), String> {
        if let Some(parent) = path.parent() {
            if !parent.as_os_str().is_empty() {
                std::fs::create_dir_all(parent).map_err(|e| format!("无法创建保存目录：{e}"))?;
            }
        }
        let rgba = self.to_rgba_image();
        match format {
            SaveFormat::Png => image::DynamicImage::ImageRgba8(rgba)
                .save_with_format(path, ImageFormat::Png)
                .map_err(|e| format!("保存 PNG 失败：{e}")),
            SaveFormat::Jpeg => {
                let rgb = image::DynamicImage::ImageRgba8(rgba).into_rgb8();
                image::DynamicImage::ImageRgb8(rgb)
                    .save_with_format(path, ImageFormat::Jpeg)
                    .map_err(|e| format!("保存 JPEG 失败：{e}"))
            }
            SaveFormat::Bmp => image::DynamicImage::ImageRgba8(rgba)
                .save_with_format(path, ImageFormat::Bmp)
                .map_err(|e| format!("保存 BMP 失败：{e}")),
        }
    }

    /// Builds a bottom-up 32-bit BI_RGB DIB payload (BITMAPINFOHEADER + pixels) for CF_DIB.
    pub fn to_cf_dib_bytes(&self) -> Vec<u8> {
        let pixel_bytes = self.data.len();
        let mut out = Vec::with_capacity(40 + pixel_bytes);
        out.extend_from_slice(&40u32.to_le_bytes()); // biSize
        out.extend_from_slice(&(self.width as i32).to_le_bytes());
        out.extend_from_slice(&(self.height as i32).to_le_bytes()); // positive = bottom-up
        out.extend_from_slice(&1u16.to_le_bytes()); // planes
        out.extend_from_slice(&32u16.to_le_bytes()); // bit count
        out.extend_from_slice(&0u32.to_le_bytes()); // BI_RGB
        out.extend_from_slice(&(pixel_bytes as u32).to_le_bytes());
        out.extend_from_slice(&0i32.to_le_bytes());
        out.extend_from_slice(&0i32.to_le_bytes());
        out.extend_from_slice(&0u32.to_le_bytes());
        out.extend_from_slice(&0u32.to_le_bytes());

        let stride = (self.width as usize) * 4;
        for row in (0..self.height as usize).rev() {
            let start = row * stride;
            out.extend_from_slice(&self.data[start..start + stride]);
        }
        out
    }

    /// Parses raw CF_DIB / CF_DIBV5 bytes into top-down 32-bit BGRA.
    pub fn from_cf_dib_bytes(bytes: &[u8]) -> Result<Self, String> {
        if bytes.len() < 40 {
            return Err("剪贴板位图数据无效".into());
        }
        let header_size = u32::from_le_bytes(bytes[0..4].try_into().unwrap()) as usize;
        if header_size < 40 || header_size > bytes.len() {
            return Err("剪贴板位图头无效".into());
        }
        let width_i = i32::from_le_bytes(bytes[4..8].try_into().unwrap());
        let height_i = i32::from_le_bytes(bytes[8..12].try_into().unwrap());
        let bpp = u16::from_le_bytes(bytes[14..16].try_into().unwrap());
        let compression = u32::from_le_bytes(bytes[16..20].try_into().unwrap());
        let clr_used = u32::from_le_bytes(bytes[32..36].try_into().unwrap()) as usize;

        if width_i <= 0 || height_i == 0 || width_i > 32768 || height_i.unsigned_abs() > 32768 {
            return Err("剪贴板图像尺寸超出范围".into());
        }
        let width = width_i as u32;
        let height = height_i.unsigned_abs();
        let top_down = height_i < 0;

        // Determine offset to pixel array
        let mut pixel_offset = header_size;
        if compression == 3 && header_size == 40 {
            // BI_BITFIELDS appends 3 DWORD masks after a 40-byte BITMAPINFOHEADER
            pixel_offset += 12;
        }
        if bpp <= 8 {
            let palette_entries = if clr_used > 0 { clr_used } else { 1usize << bpp };
            pixel_offset += palette_entries * 4;
        }
        if pixel_offset >= bytes.len() {
            return Err("剪贴板位图像素数据缺失".into());
        }
        let pixels = &bytes[pixel_offset..];

        match bpp {
            32 => {
                let row_bytes = (width as usize) * 4;
                let needed = row_bytes.saturating_mul(height as usize);
                if pixels.len() < needed {
                    return Err("剪贴板 32 位图像数据不完整".into());
                }
                let mut out = vec![0u8; needed];
                let mut has_nonzero_alpha = false;
                for y in 0..height as usize {
                    let src_y = if top_down { y } else { height as usize - 1 - y };
                    let src_off = src_y * row_bytes;
                    let dst_off = y * row_bytes;
                    out[dst_off..dst_off + row_bytes]
                        .copy_from_slice(&pixels[src_off..src_off + row_bytes]);
                    if !has_nonzero_alpha {
                        for px in out[dst_off..dst_off + row_bytes].chunks_exact(4) {
                            if px[3] != 0 {
                                has_nonzero_alpha = true;
                                break;
                            }
                        }
                    }
                }
                if !has_nonzero_alpha {
                    for px in out.chunks_exact_mut(4) {
                        px[3] = 255;
                    }
                }
                Ok(Self {
                    width,
                    height,
                    data: out,
                })
            }
            24 => {
                let src_stride = ((width as usize) * 3).div_ceil(4) * 4;
                let needed = src_stride.saturating_mul(height as usize);
                if pixels.len() < needed {
                    return Err("剪贴板 24 位图像数据不完整".into());
                }
                let mut out = vec![0u8; (width as usize) * (height as usize) * 4];
                for y in 0..height as usize {
                    let src_y = if top_down { y } else { height as usize - 1 - y };
                    let src_row = &pixels[src_y * src_stride..src_y * src_stride + (width as usize) * 3];
                    let dst_row = &mut out[y * (width as usize) * 4..(y + 1) * (width as usize) * 4];
                    for (src, dst) in src_row.chunks_exact(3).zip(dst_row.chunks_exact_mut(4)) {
                        dst[0] = src[0];
                        dst[1] = src[1];
                        dst[2] = src[2];
                        dst[3] = 255;
                    }
                }
                Ok(Self {
                    width,
                    height,
                    data: out,
                })
            }
            _ => Err(format!("暂不支持 {bpp} 位色深的剪贴板位图")),
        }
    }
}

#[cfg(windows)]
pub mod clipboard {
    use super::BgraImage;
    use crate::util::to_wide;
    use windows::Win32::Foundation::{HANDLE, HGLOBAL};
    use windows::Win32::System::DataExchange::{
        CloseClipboard, EmptyClipboard, GetClipboardData, IsClipboardFormatAvailable,
        OpenClipboard, RegisterClipboardFormatW, SetClipboardData,
    };
    use windows::Win32::System::Memory::{
        GMEM_MOVEABLE, GlobalAlloc, GlobalLock, GlobalSize, GlobalUnlock,
    };
    use windows::core::PCWSTR;

    const CF_UNICODETEXT: u32 = 13;
    const CF_DIB: u32 = 8;
    const CF_DIBV5: u32 = 17;

    struct ClipboardGuard;
    impl ClipboardGuard {
        fn open() -> Result<Self, String> {
            // Brief retry in case another process holds the clipboard momentarily.
            for _ in 0..10 {
                if unsafe { OpenClipboard(None) }.is_ok() {
                    return Ok(Self);
                }
                std::thread::sleep(std::time::Duration::from_millis(12));
            }
            Err("无法打开系统剪贴板".into())
        }
    }
    impl Drop for ClipboardGuard {
        fn drop(&mut self) {
            unsafe {
                let _ = CloseClipboard();
            }
        }
    }

    unsafe fn alloc_global_copy(bytes: &[u8]) -> Result<HGLOBAL, String> {
        let hmem = unsafe { GlobalAlloc(GMEM_MOVEABLE, bytes.len()) }
            .map_err(|_| "剪贴板内存分配失败".to_string())?;
        let ptr = unsafe { GlobalLock(hmem) } as *mut u8;
        if ptr.is_null() {
            return Err("剪贴板内存锁定失败".into());
        }
        unsafe {
            std::ptr::copy_nonoverlapping(bytes.as_ptr(), ptr, bytes.len());
            let _ = GlobalUnlock(hmem);
        }
        Ok(hmem)
    }

    pub fn copy_image(img: &BgraImage) -> Result<(), String> {
        let dib_bytes = img.to_cf_dib_bytes();
        let png_bytes = img.to_png_bytes().ok();
        let png_fmt_name = to_wide("PNG");
        let png_fmt = unsafe { RegisterClipboardFormatW(PCWSTR(png_fmt_name.as_ptr())) };

        let _guard = ClipboardGuard::open()?;
        unsafe {
            EmptyClipboard().map_err(|_| "清空剪贴板失败".to_string())?;
            let h_dib = alloc_global_copy(&dib_bytes)?;
            SetClipboardData(CF_DIB, Some(HANDLE(h_dib.0)))
                .map_err(|_| "写入剪贴板位图失败".to_string())?;

            if png_fmt != 0 {
                if let Some(png) = png_bytes {
                    if let Ok(h_png) = alloc_global_copy(&png) {
                        let _ = SetClipboardData(png_fmt, Some(HANDLE(h_png.0)));
                    }
                }
            }
        }
        Ok(())
    }

    pub fn copy_text(text: &str) -> Result<(), String> {
        let wide = to_wide(text);
        let byte_slice =
            unsafe { std::slice::from_raw_parts(wide.as_ptr() as *const u8, wide.len() * 2) };
        let _guard = ClipboardGuard::open()?;
        unsafe {
            EmptyClipboard().map_err(|_| "清空剪贴板失败".to_string())?;
            let h_mem = alloc_global_copy(byte_slice)?;
            SetClipboardData(CF_UNICODETEXT, Some(HANDLE(h_mem.0)))
                .map_err(|_| "写入剪贴板文本失败".to_string())?;
        }
        Ok(())
    }

    unsafe fn read_global_bytes(format: u32) -> Option<Vec<u8>> {
        if unsafe { IsClipboardFormatAvailable(format) }.is_err() {
            return None;
        }
        let handle = unsafe { GetClipboardData(format) }.ok()?;
        let hglobal = HGLOBAL(handle.0);
        let size = unsafe { GlobalSize(hglobal) };
        if size == 0 || size > 256 * 1024 * 1024 {
            return None;
        }
        let ptr = unsafe { GlobalLock(hglobal) } as *const u8;
        if ptr.is_null() {
            return None;
        }
        let mut out = vec![0u8; size];
        unsafe {
            std::ptr::copy_nonoverlapping(ptr, out.as_mut_ptr(), size);
            let _ = GlobalUnlock(hglobal);
        }
        Some(out)
    }

    pub fn read_image() -> Result<Option<BgraImage>, String> {
        let png_fmt_name = to_wide("PNG");
        let png_fmt = unsafe { RegisterClipboardFormatW(PCWSTR(png_fmt_name.as_ptr())) };

        let _guard = ClipboardGuard::open()?;
        unsafe {
            if png_fmt != 0 {
                if let Some(bytes) = read_global_bytes(png_fmt) {
                    if let Ok(img) = BgraImage::from_encoded_bytes(&bytes) {
                        return Ok(Some(img));
                    }
                }
            }
            if let Some(bytes) = read_global_bytes(CF_DIBV5) {
                if let Ok(img) = BgraImage::from_cf_dib_bytes(&bytes) {
                    return Ok(Some(img));
                }
            }
            if let Some(bytes) = read_global_bytes(CF_DIB) {
                return BgraImage::from_cf_dib_bytes(&bytes).map(Some);
            }
        }
        Ok(None)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn crop_rotate_and_codecs_roundtrip() {
        let mut img = BgraImage::new_solid(4, 3, 10, 20, 30, 255);
        // Set pixel (1, 1) to RGB(200, 100, 50) -> BGRA(50, 100, 200, 255)
        let idx = ((1 * 4 + 1) * 4) as usize;
        img.data[idx..idx + 4].copy_from_slice(&[50, 100, 200, 255]);
        assert_eq!(img.pixel_rgb(1, 1), Some((200, 100, 50)));

        let cropped = img.crop(1, 1, 2, 2).unwrap();
        assert_eq!(cropped.width, 2);
        assert_eq!(cropped.height, 2);
        assert_eq!(cropped.pixel_rgb(0, 0), Some((200, 100, 50)));

        let rot = cropped.rotate_90_cw();
        assert_eq!(rot.width, 2);
        assert_eq!(rot.height, 2);
        assert_eq!(rot.pixel_rgb(1, 0), Some((200, 100, 50)));

        let png = cropped.to_png_bytes().unwrap();
        let decoded = BgraImage::from_encoded_bytes(&png).unwrap();
        assert_eq!(decoded, cropped);

        let dib = cropped.to_cf_dib_bytes();
        let from_dib = BgraImage::from_cf_dib_bytes(&dib).unwrap();
        assert_eq!(from_dib, cropped);
    }
}
