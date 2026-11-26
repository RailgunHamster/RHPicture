#[cfg(windows)]
use windows::Win32::Foundation::HWND;
#[cfg(windows)]
use windows::Win32::UI::Input::KeyboardAndMouse::{
    HOT_KEY_MODIFIERS, MOD_ALT, MOD_CONTROL, MOD_NOREPEAT, MOD_SHIFT, MOD_WIN, RegisterHotKey,
    UnregisterHotKey,
};

pub const HOTKEY_ID_SCREENSHOT: i32 = 1001;
pub const HOTKEY_ID_PIN_CLIPBOARD: i32 = 1002;
pub const HOTKEY_ID_TOGGLE_PINS: i32 = 1003;

pub const MOD_FLAG_ALT: u32 = 0x0001;
pub const MOD_FLAG_CTRL: u32 = 0x0002;
pub const MOD_FLAG_SHIFT: u32 = 0x0004;
pub const MOD_FLAG_WIN: u32 = 0x0008;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct HotkeySpec {
    pub modifiers: u32,
    pub vk: u32,
}

impl HotkeySpec {
    pub fn parse(input: &str) -> Result<Option<Self>, String> {
        let trimmed = input.trim();
        if trimmed.is_empty() || trimmed.eq_ignore_ascii_case("none") || trimmed == "无" {
            return Ok(None);
        }

        let mut modifiers = 0u32;
        let mut key_token: Option<&str> = None;

        for raw in trimmed.split('+') {
            let part = raw.trim();
            if part.is_empty() {
                return Err(format!("无效的快捷键格式：{input}"));
            }
            match part.to_ascii_lowercase().as_str() {
                "ctrl" | "control" => modifiers |= MOD_FLAG_CTRL,
                "alt" => modifiers |= MOD_FLAG_ALT,
                "shift" => modifiers |= MOD_FLAG_SHIFT,
                "win" | "super" | "meta" => modifiers |= MOD_FLAG_WIN,
                _ => {
                    if key_token.is_some() {
                        return Err(format!("快捷键只能包含一个主键：{input}"));
                    }
                    key_token = Some(part);
                }
            }
        }

        let key = key_token.ok_or_else(|| format!("快捷键缺少主键：{input}"))?;
        let vk = key_name_to_vk(key).ok_or_else(|| format!("不支持的按键：{key}"))?;

        Ok(Some(Self { modifiers, vk }))
    }

    pub fn format(self) -> String {
        let mut parts = Vec::new();
        if self.modifiers & MOD_FLAG_CTRL != 0 {
            parts.push("Ctrl");
        }
        if self.modifiers & MOD_FLAG_ALT != 0 {
            parts.push("Alt");
        }
        if self.modifiers & MOD_FLAG_SHIFT != 0 {
            parts.push("Shift");
        }
        if self.modifiers & MOD_FLAG_WIN != 0 {
            parts.push("Win");
        }
        let key = vk_to_key_name(self.vk).unwrap_or_else(|| format!("VK_{:02X}", self.vk));
        let mut out = parts.join("+");
        if !out.is_empty() {
            out.push('+');
        }
        out.push_str(&key);
        out
    }

    pub fn is_modifier_only_vk(vk: u32) -> bool {
        matches!(
            vk,
            0x10 | 0x11 | 0x12 | 0xA0..=0xA5 | 0x5B | 0x5C
        )
    }
}

pub fn key_name_to_vk(name: &str) -> Option<u32> {
    let upper = name.trim().to_ascii_uppercase();
    if upper.len() == 1 {
        let b = upper.as_bytes()[0];
        if b.is_ascii_uppercase() || b.is_ascii_digit() {
            return Some(b as u32);
        }
        return match b {
            b'-' => Some(0xBD),
            b'=' => Some(0xBB),
            b'[' => Some(0xDB),
            b']' => Some(0xDD),
            b';' => Some(0xBA),
            b'\'' => Some(0xDE),
            b',' => Some(0xBC),
            b'.' => Some(0xBE),
            b'/' => Some(0xBF),
            b'`' => Some(0xC0),
            b'\\' => Some(0xDC),
            _ => None,
        };
    }

    if let Some(num) = upper.strip_prefix('F') {
        if let Ok(n) = num.parse::<u32>() {
            if (1..=24).contains(&n) {
                return Some(0x70 + (n - 1));
            }
        }
    }
    if let Some(num) = upper.strip_prefix("NUMPAD") {
        if let Ok(n) = num.parse::<u32>() {
            if n <= 9 {
                return Some(0x60 + n);
            }
        }
    }

    match upper.as_str() {
        "PRINTSCREEN" | "PRTSC" | "SNAPSHOT" => Some(0x2C),
        "INSERT" | "INS" => Some(0x2D),
        "DELETE" | "DEL" => Some(0x2E),
        "HOME" => Some(0x24),
        "END" => Some(0x23),
        "PAGEUP" | "PGUP" => Some(0x21),
        "PAGEDOWN" | "PGDN" => Some(0x22),
        "SPACE" => Some(0x20),
        "TAB" => Some(0x09),
        "ENTER" | "RETURN" => Some(0x0D),
        "BACKSPACE" | "BS" => Some(0x08),
        "PAUSE" => Some(0x13),
        "UP" => Some(0x26),
        "DOWN" => Some(0x28),
        "LEFT" => Some(0x25),
        "RIGHT" => Some(0x27),
        _ => None,
    }
}

pub fn vk_to_key_name(vk: u32) -> Option<String> {
    match vk {
        0x30..=0x39 | 0x41..=0x5A => Some((vk as u8 as char).to_string()),
        0x70..=0x87 => Some(format!("F{}", vk - 0x70 + 1)),
        0x60..=0x69 => Some(format!("Numpad{}", vk - 0x60)),
        0x2C => Some("PrintScreen".into()),
        0x2D => Some("Insert".into()),
        0x2E => Some("Delete".into()),
        0x24 => Some("Home".into()),
        0x23 => Some("End".into()),
        0x21 => Some("PageUp".into()),
        0x22 => Some("PageDown".into()),
        0x20 => Some("Space".into()),
        0x09 => Some("Tab".into()),
        0x0D => Some("Enter".into()),
        0x08 => Some("Backspace".into()),
        0x13 => Some("Pause".into()),
        0x26 => Some("Up".into()),
        0x28 => Some("Down".into()),
        0x25 => Some("Left".into()),
        0x27 => Some("Right".into()),
        0xBD => Some("-".into()),
        0xBB => Some("=".into()),
        0xDB => Some("[".into()),
        0xDD => Some("]".into()),
        0xBA => Some(";".into()),
        0xDE => Some("'".into()),
        0xBC => Some(",".into()),
        0xBE => Some(".".into()),
        0xBF => Some("/".into()),
        0xC0 => Some("`".into()),
        0xDC => Some("\\".into()),
        _ => None,
    }
}

#[cfg(windows)]
fn to_win32_modifiers(flags: u32) -> HOT_KEY_MODIFIERS {
    let mut m = MOD_NOREPEAT;
    if flags & MOD_FLAG_ALT != 0 {
        m |= MOD_ALT;
    }
    if flags & MOD_FLAG_CTRL != 0 {
        m |= MOD_CONTROL;
    }
    if flags & MOD_FLAG_SHIFT != 0 {
        m |= MOD_SHIFT;
    }
    if flags & MOD_FLAG_WIN != 0 {
        m |= MOD_WIN;
    }
    m
}

#[cfg(windows)]
pub fn unregister_all(hwnd: HWND) {
    unsafe {
        let _ = UnregisterHotKey(Some(hwnd), HOTKEY_ID_SCREENSHOT);
        let _ = UnregisterHotKey(Some(hwnd), HOTKEY_ID_PIN_CLIPBOARD);
        let _ = UnregisterHotKey(Some(hwnd), HOTKEY_ID_TOGGLE_PINS);
    }
}

#[cfg(windows)]
pub fn register_from_config(hwnd: HWND, cfg: &crate::config::AppConfig) -> Vec<String> {
    unregister_all(hwnd);
    let mut warnings = Vec::new();
    let slots = [
        (HOTKEY_ID_SCREENSHOT, "截图", &cfg.screenshot_hotkey),
        (HOTKEY_ID_PIN_CLIPBOARD, "剪贴板贴图", &cfg.pin_clipboard_hotkey),
        (HOTKEY_ID_TOGGLE_PINS, "隐藏/显示贴图", &cfg.toggle_pins_hotkey),
    ];

    for (id, label, text) in slots {
        match HotkeySpec::parse(text) {
            Ok(Some(spec)) => {
                let res = unsafe {
                    RegisterHotKey(Some(hwnd), id, to_win32_modifiers(spec.modifiers), spec.vk)
                };
                if res.is_err() {
                    warnings.push(format!(
                        "{label}快捷键「{}」注册失败（可能已被其他程序占用）",
                        spec.format()
                    ));
                }
            }
            Ok(None) => {}
            Err(err) => warnings.push(format!("{label}快捷键无效：{err}")),
        }
    }
    warnings
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_and_format_hotkeys() {
        let spec = HotkeySpec::parse("ctrl + alt + a").unwrap().unwrap();
        assert_eq!(spec.modifiers, MOD_FLAG_CTRL | MOD_FLAG_ALT);
        assert_eq!(spec.vk, b'A' as u32);
        assert_eq!(spec.format(), "Ctrl+Alt+A");

        let f1 = HotkeySpec::parse("F1").unwrap().unwrap();
        assert_eq!(f1.modifiers, 0);
        assert_eq!(f1.vk, 0x70);
        assert_eq!(f1.format(), "F1");

        let shift_f3 = HotkeySpec::parse("Shift+F3").unwrap().unwrap();
        assert_eq!(shift_f3.format(), "Shift+F3");

        assert_eq!(HotkeySpec::parse("").unwrap(), None);
        assert_eq!(HotkeySpec::parse("无").unwrap(), None);
        assert!(HotkeySpec::parse("Ctrl+Alt").is_err());
        assert!(HotkeySpec::parse("Ctrl+A+B").is_err());
    }
}
