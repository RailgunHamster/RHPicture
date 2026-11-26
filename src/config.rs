use serde::{Deserialize, Serialize};
use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ThemeMode {
    Light,
    System,
    Dark,
}

impl Default for ThemeMode {
    fn default() -> Self {
        ThemeMode::Light
    }
}

impl ThemeMode {
    pub fn index(self) -> usize {
        match self {
            ThemeMode::Light => 0,
            ThemeMode::System => 1,
            ThemeMode::Dark => 2,
        }
    }

    pub fn from_index(idx: usize) -> Self {
        match idx {
            1 => ThemeMode::System,
            2 => ThemeMode::Dark,
            _ => ThemeMode::Light,
        }
    }

    pub fn is_dark(self) -> bool {
        match self {
            ThemeMode::Light => false,
            ThemeMode::Dark => true,
            ThemeMode::System => crate::util::system_prefers_dark_mode(),
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum SaveFormat {
    Png,
    Jpeg,
    Bmp,
}

impl SaveFormat {
    pub fn extension(self) -> &'static str {
        match self {
            SaveFormat::Png => "png",
            SaveFormat::Jpeg => "jpg",
            SaveFormat::Bmp => "bmp",
        }
    }

    pub fn index(self) -> usize {
        match self {
            SaveFormat::Png => 0,
            SaveFormat::Jpeg => 1,
            SaveFormat::Bmp => 2,
        }
    }

    pub fn from_index(idx: usize) -> Self {
        match idx {
            1 => SaveFormat::Jpeg,
            2 => SaveFormat::Bmp,
            _ => SaveFormat::Png,
        }
    }

    pub fn from_path(path: &Path, fallback: Self) -> Self {
        match path
            .extension()
            .and_then(|s| s.to_str())
            .map(|s| s.to_ascii_lowercase())
            .as_deref()
        {
            Some("png") => SaveFormat::Png,
            Some("jpg" | "jpeg") => SaveFormat::Jpeg,
            Some("bmp") => SaveFormat::Bmp,
            _ => fallback,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum PinDoubleClickAction {
    Hide,
    ResetZoom,
    Close,
}

impl PinDoubleClickAction {
    pub fn index(self) -> usize {
        match self {
            PinDoubleClickAction::Hide => 0,
            PinDoubleClickAction::ResetZoom => 1,
            PinDoubleClickAction::Close => 2,
        }
    }

    pub fn from_index(idx: usize) -> Self {
        match idx {
            1 => PinDoubleClickAction::ResetZoom,
            2 => PinDoubleClickAction::Close,
            _ => PinDoubleClickAction::Hide,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum UpdateProvider {
    Cnb,
    Github,
}

impl UpdateProvider {
    pub fn as_str(self) -> &'static str {
        match self {
            UpdateProvider::Cnb => "cnb",
            UpdateProvider::Github => "github",
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            UpdateProvider::Cnb => "CNB",
            UpdateProvider::Github => "GitHub",
        }
    }

    pub fn index(self) -> usize {
        match self {
            UpdateProvider::Cnb => 0,
            UpdateProvider::Github => 1,
        }
    }

    pub fn from_index(idx: usize) -> Self {
        match idx {
            1 => UpdateProvider::Github,
            _ => UpdateProvider::Cnb,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct ProxyConfig {
    pub enabled: bool,
    pub protocol: String,
    pub host: String,
    pub port: u16,
}

impl Default for ProxyConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            protocol: "http".into(),
            host: "127.0.0.1".into(),
            port: 7890,
        }
    }
}

impl ProxyConfig {
    pub fn validate(&self) -> Result<(), String> {
        if !self.enabled {
            return Ok(());
        }
        if !matches!(self.protocol.as_str(), "http" | "https") {
            return Err("代理协议仅支持 http 或 https".into());
        }
        if self.host.is_empty()
            || self.host.len() > 253
            || self.host != self.host.trim()
            || self.host.bytes().any(|b| {
                b <= 32
                    || b >= 127
                    || matches!(b, b'/' | b'\\' | b'@' | b'?' | b'#' | b'%' | b'[' | b']')
            })
        {
            return Err("代理地址请只填写主机名或 IP，不含协议和端口".into());
        }
        if self.port == 0 {
            return Err("代理端口应为 1–65535".into());
        }
        if self.host.contains(':') && self.host.parse::<std::net::Ipv6Addr>().is_err() {
            return Err("代理地址中不能包含端口".into());
        }
        let addr = self.address();
        let parsed = url::Url::parse(&addr).map_err(|_| "代理地址无效".to_string())?;
        if parsed.host_str().is_none() {
            return Err("代理地址无效".into());
        }
        Ok(())
    }

    pub fn address(&self) -> String {
        let host = if self.host.contains(':') {
            format!("[{}]", self.host)
        } else {
            self.host.clone()
        };
        format!("{}://{}:{}", self.protocol, host, self.port)
    }

    pub fn active_address(&self) -> Option<String> {
        self.enabled.then(|| self.address())
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct UpdateConfig {
    pub preferred_source: UpdateProvider,
    pub cnb_url: String,
    pub github_url: String,
    pub auto_check_on_start: bool,
    pub proxy: ProxyConfig,
}

impl Default for UpdateConfig {
    fn default() -> Self {
        Self {
            preferred_source: UpdateProvider::Cnb,
            cnb_url: "https://cnb.cool/railgunhamster/RHPicture".into(),
            github_url: "https://github.com/RailgunHamster/RHPicture".into(),
            auto_check_on_start: false,
            proxy: ProxyConfig::default(),
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct AppConfig {
    pub schema_version: u32,
    pub theme: ThemeMode,
    pub screenshot_hotkey: String,
    pub pin_clipboard_hotkey: String,
    pub toggle_pins_hotkey: String,
    pub auto_start: bool,
    pub smart_window_detect: bool,
    pub show_magnifier: bool,
    pub save_dir: String,
    pub save_format: SaveFormat,
    pub quick_save_no_dialog: bool,
    pub pin_double_click: PinDoubleClickAction,
    pub pin_show_border: bool,
    pub update: UpdateConfig,
}

impl Default for AppConfig {
    fn default() -> Self {
        Self {
            schema_version: 1,
            theme: ThemeMode::Light,
            screenshot_hotkey: "F1".into(),
            pin_clipboard_hotkey: "F3".into(),
            toggle_pins_hotkey: "Shift+F3".into(),
            auto_start: false,
            smart_window_detect: true,
            show_magnifier: true,
            save_dir: default_save_dir().to_string_lossy().into_owned(),
            save_format: SaveFormat::Png,
            quick_save_no_dialog: false,
            pin_double_click: PinDoubleClickAction::Hide,
            pin_show_border: true,
            update: UpdateConfig::default(),
        }
    }
}

pub fn data_dir() -> PathBuf {
    if let Some(override_dir) = std::env::var_os("RHPICTURE_DATA_DIR") {
        if !override_dir.is_empty() {
            return PathBuf::from(override_dir);
        }
    }
    if let Some(appdata) = std::env::var_os("APPDATA") {
        return PathBuf::from(appdata).join("RHPicture");
    }
    std::env::current_exe()
        .ok()
        .and_then(|p| p.parent().map(Path::to_path_buf))
        .unwrap_or_else(|| PathBuf::from("."))
        .join("data")
}

pub fn default_save_dir() -> PathBuf {
    if let Some(profile) = std::env::var_os("USERPROFILE") {
        return PathBuf::from(profile).join("Pictures").join("RHPicture");
    }
    data_dir().join("Screenshots")
}

impl AppConfig {
    pub fn config_path(dir: &Path) -> PathBuf {
        dir.join("config.json")
    }

    pub fn load_from(dir: &Path) -> Self {
        let path = Self::config_path(dir);
        let Ok(meta) = fs::symlink_metadata(&path) else {
            return Self::default();
        };
        if !meta.file_type().is_file() || meta.len() > 256 * 1024 {
            return Self::default();
        }
        fs::read(&path)
            .ok()
            .and_then(|bytes| serde_json::from_slice::<Self>(&bytes).ok())
            .unwrap_or_default()
    }

    pub fn load() -> Self {
        Self::load_from(&data_dir())
    }

    pub fn save_to(&self, dir: &Path) -> Result<(), String> {
        self.update.proxy.validate()?;
        fs::create_dir_all(dir).map_err(|e| format!("无法创建配置目录：{e}"))?;
        let target = Self::config_path(dir);
        let tmp = dir.join("config.json.tmp");
        let bytes =
            serde_json::to_vec_pretty(self).map_err(|e| format!("配置序列化失败：{e}"))?;
        let mut file = fs::File::create(&tmp).map_err(|e| format!("无法写入配置文件：{e}"))?;
        file.write_all(&bytes)
            .and_then(|_| file.sync_all())
            .map_err(|e| format!("无法写入配置文件：{e}"))?;
        drop(file);
        fs::rename(&tmp, &target).map_err(|e| format!("无法保存配置文件：{e}"))?;
        Ok(())
    }

    pub fn save(&self) -> Result<(), String> {
        self.save_to(&data_dir())
    }

    pub fn resolved_save_dir(&self) -> PathBuf {
        let trimmed = self.save_dir.trim();
        if trimmed.is_empty() {
            default_save_dir()
        } else {
            PathBuf::from(trimmed)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn config_round_trip_and_atomic_save() {
        let dir = std::env::temp_dir().join(format!(
            "rhpicture-config-test-{}",
            std::process::id()
        ));
        let _ = fs::remove_dir_all(&dir);

        let mut cfg = AppConfig::default();
        cfg.screenshot_hotkey = "Ctrl+Alt+A".into();
        cfg.update.preferred_source = UpdateProvider::Github;
        cfg.update.cnb_url = "https://cnb.cool/myorg/RHPicture".into();
        cfg.update.github_url = "https://github.com/myorg/RHPicture".into();
        cfg.update.proxy = ProxyConfig {
            enabled: true,
            protocol: "http".into(),
            host: "127.0.0.1".into(),
            port: 10809,
        };

        cfg.save_to(&dir).unwrap();
        let loaded = AppConfig::load_from(&dir);
        assert_eq!(loaded, cfg);

        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn proxy_validation_rejects_bad_values() {
        for host in ["http://127.0.0.1", "a:7890", "a/b", "u@host", ""] {
            let p = ProxyConfig {
                enabled: true,
                protocol: "http".into(),
                host: host.into(),
                port: 7890,
            };
            assert!(p.validate().is_err(), "expected invalid host: {host}");
        }
        let zero_port = ProxyConfig {
            enabled: true,
            protocol: "http".into(),
            host: "127.0.0.1".into(),
            port: 0,
        };
        assert!(zero_port.validate().is_err());

        let disabled_bad = ProxyConfig {
            enabled: false,
            protocol: "ftp".into(),
            host: "bad/host".into(),
            port: 0,
        };
        assert!(disabled_bad.validate().is_ok());
    }

    #[test]
    fn theme_mode_defaults_to_light_and_roundtrips() {
        let def = AppConfig::default();
        assert_eq!(def.theme, ThemeMode::Light);
        assert!(!ThemeMode::Light.is_dark());
        assert!(ThemeMode::Dark.is_dark());

        for (idx, mode) in [ThemeMode::Light, ThemeMode::System, ThemeMode::Dark]
            .into_iter()
            .enumerate()
        {
            assert_eq!(mode.index(), idx);
            assert_eq!(ThemeMode::from_index(idx), mode);
            let json = serde_json::to_string(&mode).unwrap();
            let parsed: ThemeMode = serde_json::from_str(&json).unwrap();
            assert_eq!(parsed, mode);
        }

        // Legacy config JSON without "theme" field must default cleanly to Light
        let legacy: AppConfig = serde_json::from_str("{}").unwrap();
        assert_eq!(legacy.theme, ThemeMode::Light);
    }
}
