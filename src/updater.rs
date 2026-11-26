//! Velopack-based incremental (delta) and full updater with user-configurable CNB & GitHub mirrors.
//! Modeled after D:\git\RHRemote's hardened mirror fallback, SHA256 verification, and cancellation flow,
//! extended with runtime-configurable CNB/GitHub URLs and standalone-mode update checking.

use crate::config::{UpdateConfig, UpdateProvider};
use serde::{Deserialize, Serialize};
use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::{
    Arc, Mutex, OnceLock,
    mpsc::{self, Sender},
};
use std::time::Duration;
use tokio_util::sync::CancellationToken;
use velopack::{
    Error, UpdateCheck, UpdateInfo, UpdateManager, VelopackAsset, VelopackAssetFeed,
    bundle::Manifest, sources::UpdateSource,
};

pub const APP_VERSION: &str = env!("CARGO_PKG_VERSION");

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum UpdatePhase {
    Idle,
    Checking,
    Current,
    Available,
    Downloading,
    Verifying,
    Downloaded,
    Applying,
    Cancelling,
    Cancelled,
    Error,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct UpdateSnapshot {
    pub managed: bool,
    pub phase: UpdatePhase,
    pub status: String,
    pub current_version: String,
    pub installed_version: Option<String>,
    pub available_version: Option<String>,
    pub provider: String,
    pub progress: i16,
    pub pending_restart: bool,
    pub is_delta: bool,
    pub delta_count: usize,
    pub download_bytes: u64,
    pub full_bytes: u64,
    pub notes: String,
    pub release_page_url: Option<String>,
    pub busy: bool,
    pub cancellable: bool,
    pub failed_action: String,
}

impl Default for UpdateSnapshot {
    fn default() -> Self {
        Self {
            managed: false,
            phase: UpdatePhase::Idle,
            status: "尚未检查更新".into(),
            current_version: APP_VERSION.into(),
            installed_version: None,
            available_version: None,
            provider: String::new(),
            progress: 0,
            pending_restart: false,
            is_delta: false,
            delta_count: 0,
            download_bytes: 0,
            full_bytes: 0,
            notes: String::new(),
            release_page_url: None,
            busy: false,
            cancellable: false,
            failed_action: String::new(),
        }
    }
}

impl UpdateSnapshot {
    pub fn primary_button_label(&self) -> String {
        if self.busy {
            return match self.phase {
                UpdatePhase::Cancelling => "正在取消…".into(),
                UpdatePhase::Checking => "检查中…".into(),
                UpdatePhase::Downloading => format!("下载中 {}%", self.progress.clamp(0, 100)),
                UpdatePhase::Verifying => "正在校验…".into(),
                UpdatePhase::Applying => "正在安装…".into(),
                _ => "请稍候…".into(),
            };
        }
        if !self.managed {
            if self.available_version.is_some() {
                return "打开发布页下载".into();
            }
            return if self.phase == UpdatePhase::Error {
                "重新检查".into()
            } else {
                "检查更新".into()
            };
        }
        if self.phase == UpdatePhase::Error && self.failed_action == "check" {
            return "重新检查".into();
        }
        if self.pending_restart {
            return if self.phase == UpdatePhase::Error {
                "重试安装".into()
            } else {
                "重启并安装".into()
            };
        }
        if self.available_version.is_some() {
            return if self.phase == UpdatePhase::Error {
                "重试下载并安装".into()
            } else if self.is_delta {
                format!("增量更新 ({})", format_bytes(self.download_bytes))
            } else {
                "下载并安装".into()
            };
        }
        if self.phase == UpdatePhase::Error {
            "重新检查".into()
        } else {
            "检查更新".into()
        }
    }
}

pub fn format_bytes(bytes: u64) -> String {
    if bytes >= 1024 * 1024 {
        format!("{:.1} MB", bytes as f64 / (1024.0 * 1024.0))
    } else if bytes >= 1024 {
        format!("{:.0} KB", bytes as f64 / 1024.0)
    } else {
        format!("{bytes} B")
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum NormalizedMirror {
    /// GitHub / CNB release base ending with `/` (e.g. `https://cnb.cool/u/r/-/releases/` or `https://github.com/u/r/releases/`)
    HostedReleases {
        provider_name: &'static str,
        releases_base: String,
    },
    /// Generic HTTPS static directory base ending with `/`
    DirectHttps {
        provider_name: &'static str,
        base: String,
    },
}

impl NormalizedMirror {
    pub fn provider_name(&self) -> &'static str {
        match self {
            NormalizedMirror::HostedReleases { provider_name, .. }
            | NormalizedMirror::DirectHttps { provider_name, .. } => provider_name,
        }
    }

    pub fn feed_url(&self, feed_name: &str) -> String {
        match self {
            NormalizedMirror::HostedReleases { releases_base, .. } => {
                format!("{releases_base}latest/download/{feed_name}")
            }
            NormalizedMirror::DirectHttps { base, .. } => format!("{base}{feed_name}"),
        }
    }

    pub fn asset_urls(&self, asset: &VelopackAsset) -> Result<Vec<String>, Error> {
        validate_filename(&asset.Version)?;
        validate_filename(&asset.FileName)?;
        Ok(match self {
            NormalizedMirror::HostedReleases { releases_base, .. } => vec![
                format!("{releases_base}download/v{}/{}", asset.Version, asset.FileName),
                format!("{releases_base}latest/download/{}", asset.FileName),
            ],
            NormalizedMirror::DirectHttps { base, .. } => {
                vec![format!("{base}{}", asset.FileName)]
            }
        })
    }

    pub fn release_page_url(&self) -> String {
        match self {
            NormalizedMirror::HostedReleases { releases_base, .. } => releases_base.clone(),
            NormalizedMirror::DirectHttps { base, .. } => base.clone(),
        }
    }
}

pub fn normalize_mirror_url(
    raw: &str,
    provider_hint: UpdateProvider,
    allow_loopback_http: bool,
) -> Result<NormalizedMirror, String> {
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        return Err("更新地址不能为空".into());
    }
    let parsed = url::Url::parse(trimmed).map_err(|_| format!("无效的更新地址：{trimmed}"))?;
    let is_loopback = matches!(parsed.host_str(), Some("127.0.0.1" | "[::1]" | "localhost"));
    if parsed.scheme() != "https" && !(allow_loopback_http && parsed.scheme() == "http" && is_loopback)
    {
        return Err(format!("更新地址必须使用 HTTPS：{trimmed}"));
    }
    if !parsed.username().is_empty() || parsed.password().is_some() || parsed.query().is_some() || parsed.fragment().is_some() {
        return Err(format!("更新地址不能包含账号密码、查询参数或锚点：{trimmed}"));
    }

    let host = parsed
        .host_str()
        .ok_or_else(|| format!("更新地址缺少主机名：{trimmed}"))?
        .to_ascii_lowercase();
    let mut path = parsed.path().trim_end_matches('/').to_string();
    if let Some(stripped) = path.strip_suffix(".git") {
        path = stripped.to_string();
    }

    if host == "cnb.cool" {
        // Accept https://cnb.cool/owner/repo or https://cnb.cool/owner/repo/-/releases[/latest/download]
        let base_path = if let Some(idx) = path.find("/-/releases") {
            format!("{}/-/releases/", &path[..idx])
        } else {
            let parts: Vec<&str> = path.split('/').filter(|s| !s.is_empty()).collect();
            if parts.len() < 2 {
                return Err("CNB 地址请填写完整仓库地址，例如 https://cnb.cool/用户名/仓库名".into());
            }
            format!("/{}/-/releases/", parts.join("/"))
        };
        return Ok(NormalizedMirror::HostedReleases {
            provider_name: "CNB",
            releases_base: format!("https://cnb.cool{base_path}"),
        });
    }

    if host == "github.com" {
        let base_path = if let Some(idx) = path.find("/releases") {
            format!("{}/releases/", &path[..idx])
        } else {
            let parts: Vec<&str> = path.split('/').filter(|s| !s.is_empty()).collect();
            if parts.len() < 2 {
                return Err("GitHub 地址请填写完整仓库地址，例如 https://github.com/用户名/仓库名".into());
            }
            format!("/{}/{}/releases/", parts[0], parts[1])
        };
        return Ok(NormalizedMirror::HostedReleases {
            provider_name: "GitHub",
            releases_base: format!("https://github.com{base_path}"),
        });
    }

    // Direct static HTTPS mirror or loopback test server
    let origin = match parsed.port() {
        Some(p) => format!("{}://{}:{}", parsed.scheme(), host, p),
        None => format!("{}://{}", parsed.scheme(), host),
    };
    let clean_path = if path.is_empty() {
        "/".to_string()
    } else {
        format!("{path}/")
    };
    Ok(NormalizedMirror::DirectHttps {
        provider_name: provider_hint.label(),
        base: format!("{origin}{clean_path}"),
    })
}

fn validate_filename(name: &str) -> Result<(), Error> {
    if name.is_empty()
        || name == "."
        || name == ".."
        || name.contains(['/', '\\', ':', '\0'])
    {
        Err(Error::Other("Invalid update asset name".into()))
    } else {
        Ok(())
    }
}

fn cached_path(directory: &Path, asset: &VelopackAsset) -> Result<PathBuf, String> {
    validate_filename(&asset.FileName).map_err(|e| e.to_string())?;
    if !asset.FileName.ends_with(".nupkg") {
        return Err("更新包文件名无效".into());
    }
    Ok(directory.join(&asset.FileName))
}

pub fn verify_cached(directory: &Path, asset: &VelopackAsset) -> Result<bool, String> {
    let digest = if !asset.SHA256.is_empty() {
        (&asset.SHA256, 64)
    } else {
        (&asset.SHA1, 40)
    };
    if asset.Size == 0
        || asset.Size > 2 * 1024 * 1024 * 1024
        || digest.0.len() != digest.1
        || !digest.0.bytes().all(|b| b.is_ascii_hexdigit())
    {
        return Err("更新包缺少有效的校验信息".into());
    }
    let path = cached_path(directory, asset)?;
    let meta = match fs::symlink_metadata(&path) {
        Ok(m) => m,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(false),
        Err(e) => return Err(e.to_string()),
    };
    if !meta.file_type().is_file() {
        return Err("更新缓存不是普通文件".into());
    }
    if meta.len() != asset.Size {
        return Ok(false);
    }
    let (sha1, sha256) =
        velopack::misc::calculate_sha1_sha256(&path).map_err(|e| e.to_string())?;
    if asset.SHA256.len() == 64 {
        Ok(sha256.eq_ignore_ascii_case(&asset.SHA256))
    } else if asset.SHA256.is_empty() && asset.SHA1.len() == 40 {
        Ok(sha1.eq_ignore_ascii_case(&asset.SHA1))
    } else {
        Err("更新包缺少有效的校验信息".into())
    }
}

fn run_cancellable<T>(
    token: &CancellationToken,
    future: impl std::future::Future<Output = Result<T, Error>>,
) -> Result<T, Error> {
    if token.is_cancelled() {
        return Err(Error::Other("更新操作已取消".into()));
    }
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()?;
    let res = runtime.block_on(async {
        tokio::select! {
            biased;
            _ = token.cancelled() => Err(Error::Other("更新操作已取消".into())),
            val = future => val,
        }
    });
    runtime.shutdown_background();
    res
}

struct SharedState {
    config: UpdateConfig,
    snapshot: UpdateSnapshot,
    selected_mirror: Option<NormalizedMirror>,
    cancel_token: CancellationToken,
    allow_loopback_http: bool,
    notify: Option<Arc<dyn Fn() + Send + Sync>>,
}

impl SharedState {
    fn notify_ui(&self) {
        if let Some(cb) = &self.notify {
            cb();
        }
    }

    fn ordered_mirrors(&self) -> Result<Vec<NormalizedMirror>, String> {
        let mut candidates = Vec::new();
        let pairs = match self.config.preferred_source {
            UpdateProvider::Cnb => [
                (UpdateProvider::Cnb, self.config.cnb_url.as_str()),
                (UpdateProvider::Github, self.config.github_url.as_str()),
            ],
            UpdateProvider::Github => [
                (UpdateProvider::Github, self.config.github_url.as_str()),
                (UpdateProvider::Cnb, self.config.cnb_url.as_str()),
            ],
        };

        let mut last_err = None;
        for (prov, raw) in pairs {
            if raw.trim().is_empty() {
                continue;
            }
            match normalize_mirror_url(raw, prov, self.allow_loopback_http) {
                Ok(m) => candidates.push(m),
                Err(e) => last_err = Some(e),
            }
        }
        if candidates.is_empty() {
            return Err(last_err.unwrap_or_else(|| "请先配置有效的 CNB 或 GitHub 更新地址".into()));
        }
        Ok(candidates)
    }
}

#[derive(Clone)]
struct Mirrors {
    shared: Arc<Mutex<SharedState>>,
}

impl Mirrors {
    fn build_client(&self, timeout_secs: u64) -> Result<reqwest::Client, Error> {
        if rustls::crypto::CryptoProvider::get_default().is_none() {
            let _ = rustls::crypto::ring::default_provider().install_default();
        }
        let (proxy_cfg, allow_http) = {
            let s = self.shared.lock().unwrap();
            (s.config.proxy.clone(), s.allow_loopback_http)
        };
        proxy_cfg.validate().map_err(Error::Other)?;
        let mut builder = reqwest::Client::builder()
            .no_proxy()
            .https_only(!allow_http)
            .timeout(Duration::from_secs(timeout_secs))
            .user_agent(concat!("RHPicture/", env!("CARGO_PKG_VERSION")));
        if let Some(addr) = proxy_cfg.active_address() {
            let proxy =
                reqwest::Proxy::all(&addr).map_err(|_| Error::Other("更新代理地址无效".into()))?;
            builder = builder.proxy(proxy);
        }
        builder
            .build()
            .map_err(|_| Error::Other("更新网络初始化失败".into()))
    }

    fn fetch_feed_internal(&self, channel: &str) -> Result<VelopackAssetFeed, Error> {
        let feed_name = format!("releases.{channel}.json");
        validate_filename(&feed_name)?;

        let mirrors = {
            let s = self.shared.lock().unwrap();
            s.ordered_mirrors().map_err(Error::Other)?
        };

        let mut errors = Vec::new();
        for mirror in mirrors {
            let token = {
                let mut s = self.shared.lock().unwrap();
                if s.cancel_token.is_cancelled() {
                    return Err(Error::Other("更新操作已取消".into()));
                }
                s.snapshot.provider = mirror.provider_name().into();
                s.snapshot.status = format!("正在从 {} 检查更新…", mirror.provider_name());
                s.notify_ui();
                s.cancel_token.clone()
            };

            let url = mirror.feed_url(&feed_name);
            let res = run_cancellable(&token, async {
                let mut resp = self
                    .build_client(15)?
                    .get(&url)
                    .send()
                    .await
                    .and_then(reqwest::Response::error_for_status)
                    .map_err(|e| Error::Other(format!("请求失败：{e}")))?;
                let mut bytes = Vec::new();
                while let Some(chunk) = resp
                    .chunk()
                    .await
                    .map_err(|_| Error::Other("读取更新信息失败".into()))?
                {
                    if bytes.len().saturating_add(chunk.len()) > 2 * 1024 * 1024 {
                        return Err(Error::Other("更新清单过大".into()));
                    }
                    bytes.extend_from_slice(&chunk);
                }
                serde_json::from_slice::<VelopackAssetFeed>(&bytes)
                    .map_err(|_| Error::Other("更新清单格式无效".into()))
            });

            if token.is_cancelled() {
                return Err(Error::Other("更新操作已取消".into()));
            }

            match res {
                Ok(feed) => {
                    let mut s = self.shared.lock().unwrap();
                    s.selected_mirror = Some(mirror.clone());
                    s.snapshot.provider = mirror.provider_name().into();
                    s.snapshot.release_page_url = Some(mirror.release_page_url());
                    s.notify_ui();
                    return Ok(feed);
                }
                Err(e) => {
                    errors.push(format!("{}({})", mirror.provider_name(), e));
                }
            }
        }
        Err(Error::Other(format!(
            "所有已配置更新源均不可用：{}",
            errors.join("；")
        )))
    }
}

impl UpdateSource for Mirrors {
    fn get_release_feed(
        &self,
        channel: &str,
        _app: &Manifest,
        _staged_user_id: &str,
    ) -> Result<VelopackAssetFeed, Error> {
        self.fetch_feed_internal(channel)
    }

    fn download_release_entry(
        &self,
        asset: &VelopackAsset,
        local_file: &Path,
        progress: Option<Sender<i16>>,
    ) -> Result<(), Error> {
        if asset.Size == 0 || asset.Size > 2 * 1024 * 1024 * 1024 {
            return Err(Error::Other("更新包体积异常".into()));
        }

        let mirrors = {
            let s = self.shared.lock().unwrap();
            let mut list = s.ordered_mirrors().map_err(Error::Other)?;
            if let Some(sel) = &s.selected_mirror {
                if let Some(pos) = list.iter().position(|m| m == sel) {
                    let chosen = list.remove(pos);
                    list.insert(0, chosen);
                }
            }
            list
        };

        for mirror in mirrors {
            let token = {
                let mut s = self.shared.lock().unwrap();
                if s.cancel_token.is_cancelled() {
                    return Err(Error::Other("更新操作已取消".into()));
                }
                s.snapshot.provider = mirror.provider_name().into();
                s.notify_ui();
                s.cancel_token.clone()
            };

            let urls = mirror.asset_urls(asset)?;
            for address in urls {
                let res = run_cancellable(&token, async {
                    let mut resp = self
                        .build_client(180)?
                        .get(&address)
                        .send()
                        .await
                        .and_then(reqwest::Response::error_for_status)
                        .map_err(|_| Error::Other("更新包下载失败".into()))?;
                    let mut file = fs::File::create(local_file)?;
                    let mut total = 0u64;
                    while let Some(chunk) = resp
                        .chunk()
                        .await
                        .map_err(|_| Error::Other("更新包读取失败".into()))?
                    {
                        total = total.saturating_add(chunk.len() as u64);
                        if total > asset.Size {
                            return Err(Error::Other("更新包长度超出预期".into()));
                        }
                        file.write_all(&chunk)?;
                        if let Some(tx) = &progress {
                            let pct = ((total * 100 / asset.Size).min(100)) as i16;
                            let _ = tx.send(pct);
                        }
                    }
                    if total != asset.Size {
                        return Err(Error::Other("更新包下载不完整".into()));
                    }
                    Ok(())
                });

                if res.is_ok() {
                    return Ok(());
                }

                let _ = fs::remove_file(local_file);
                if token.is_cancelled() {
                    return res;
                }
            }
        }
        Err(Error::Other("所有更新源均下载失败".into()))
    }
}

struct UpdaterInner {
    manager: Option<UpdateManager>,
    mirrors: Mirrors,
    available: Option<UpdateInfo>,
    verified_sha: Option<String>,
    packages_dir: OnceLock<PathBuf>,
}

#[derive(Clone)]
pub struct UpdateService {
    shared: Arc<Mutex<SharedState>>,
    inner: Arc<Mutex<UpdaterInner>>,
}

impl UpdateService {
    pub fn new(config: UpdateConfig, notify: Option<Arc<dyn Fn() + Send + Sync>>) -> Self {
        let shared = Arc::new(Mutex::new(SharedState {
            config,
            snapshot: UpdateSnapshot::default(),
            selected_mirror: None,
            cancel_token: CancellationToken::new(),
            allow_loopback_http: false,
            notify,
        }));
        let mirrors = Mirrors {
            shared: shared.clone(),
        };
        let manager = UpdateManager::new(mirrors.clone(), None, None).ok();

        let service = Self {
            shared,
            inner: Arc::new(Mutex::new(UpdaterInner {
                manager,
                mirrors,
                available: None,
                verified_sha: None,
                packages_dir: OnceLock::new(),
            })),
        };
        service.refresh_snapshot();
        {
            let mut s = service.shared.lock().unwrap();
            if s.snapshot.pending_restart {
                s.snapshot.phase = UpdatePhase::Downloaded;
                s.snapshot.status = "更新已下载完成，可立即重启并安装".into();
            }
        }
        service
    }

    pub fn update_config(&self, config: UpdateConfig) {
        let mut s = self.shared.lock().unwrap();
        s.config = config;
        s.selected_mirror = None;
        s.notify_ui();
    }

    pub fn snapshot(&self) -> UpdateSnapshot {
        self.shared.lock().unwrap().snapshot.clone()
    }

    pub fn cancel(&self) {
        let mut s = self.shared.lock().unwrap();
        if s.snapshot.busy && s.snapshot.cancellable {
            s.cancel_token.cancel();
            s.snapshot.phase = UpdatePhase::Cancelling;
            s.snapshot.status = "正在取消更新…".into();
            s.notify_ui();
        }
    }

    fn refresh_snapshot(&self) {
        let inner = self.inner.lock().unwrap();
        let pending = inner
            .manager
            .as_ref()
            .and_then(|m| m.get_update_pending_restart());
        let mut s = self.shared.lock().unwrap();
        s.snapshot.managed = inner.manager.is_some();
        s.snapshot.current_version = APP_VERSION.into();
        s.snapshot.installed_version = inner
            .manager
            .as_ref()
            .map(|m| m.get_current_version_as_string());

        if let Some(info) = &inner.available {
            s.snapshot.available_version = Some(info.TargetFullRelease.Version.clone());
            s.snapshot.notes = info.TargetFullRelease.NotesMarkdown.clone();
            s.snapshot.full_bytes = info.TargetFullRelease.Size;
            s.snapshot.delta_count = info.DeltasToTarget.len();
            s.snapshot.is_delta = !info.DeltasToTarget.is_empty();
            s.snapshot.download_bytes = if info.DeltasToTarget.is_empty() {
                info.TargetFullRelease.Size
            } else {
                info.DeltasToTarget.iter().map(|d| d.Size).sum()
            };
            s.snapshot.pending_restart = inner.verified_sha.as_deref()
                == Some(info.TargetFullRelease.SHA256.as_str())
                && !info.TargetFullRelease.SHA256.is_empty();
        } else if let Some(p) = &pending {
            s.snapshot.available_version = Some(p.Version.clone());
            s.snapshot.pending_restart = true;
        }
        s.notify_ui();
    }

    pub fn start_check_async(&self) {
        {
            let mut s = self.shared.lock().unwrap();
            if s.snapshot.busy {
                return;
            }
            s.cancel_token = CancellationToken::new();
            s.snapshot.busy = true;
            s.snapshot.cancellable = true;
            s.snapshot.failed_action.clear();
            s.snapshot.phase = UpdatePhase::Checking;
            s.snapshot.progress = 0;
            s.snapshot.status = "正在检查更新…".into();
            s.notify_ui();
        }

        let svc = self.clone();
        std::thread::spawn(move || {
            let res = svc.run_check_sync();
            let mut s = svc.shared.lock().unwrap();
            s.snapshot.busy = false;
            s.snapshot.cancellable = false;
            if s.cancel_token.is_cancelled() {
                s.snapshot.phase = UpdatePhase::Cancelled;
                s.snapshot.status = "已取消检查更新".into();
            } else if let Err(err) = res {
                s.snapshot.phase = UpdatePhase::Error;
                s.snapshot.failed_action = "check".into();
                s.snapshot.status = err;
            }
            s.notify_ui();
        });
    }

    pub fn start_download_or_install_async(&self, on_exit_for_apply: Arc<dyn Fn() + Send + Sync>) {
        let snap = self.snapshot();
        if snap.busy {
            return;
        }
        if !snap.managed {
            if let Some(url) = snap.release_page_url {
                #[cfg(windows)]
                crate::util::open_https_url(&url);
            } else {
                self.start_check_async();
            }
            return;
        }
        if snap.available_version.is_none() && !snap.pending_restart {
            self.start_check_async();
            return;
        }

        {
            let mut s = self.shared.lock().unwrap();
            s.cancel_token = CancellationToken::new();
            s.snapshot.busy = true;
            s.snapshot.cancellable = !snap.pending_restart;
            s.snapshot.failed_action.clear();
            s.snapshot.phase = if snap.pending_restart {
                UpdatePhase::Applying
            } else {
                UpdatePhase::Downloading
            };
            s.snapshot.status = if snap.pending_restart {
                "正在重启 RHPicture 并安装更新…".into()
            } else {
                "正在准备更新…".into()
            };
            s.notify_ui();
        }

        let svc = self.clone();
        std::thread::spawn(move || {
            let res = if snap.pending_restart {
                svc.run_apply_sync(&on_exit_for_apply)
            } else {
                svc.run_download_sync()
            };
            let mut s = svc.shared.lock().unwrap();
            s.snapshot.busy = false;
            s.snapshot.cancellable = false;
            if s.cancel_token.is_cancelled() {
                s.snapshot.phase = UpdatePhase::Cancelled;
                s.snapshot.progress = 0;
                s.snapshot.status = "已取消下载，未安装更新".into();
            } else if let Err(err) = res {
                s.snapshot.phase = UpdatePhase::Error;
                s.snapshot.failed_action = "install".into();
                s.snapshot.status = err;
            }
            s.notify_ui();
        });
    }

    fn run_check_sync(&self) -> Result<(), String> {
        let is_managed = self.inner.lock().unwrap().manager.is_some();

        if is_managed {
            let manager = self.inner.lock().unwrap().manager.clone().unwrap();
            let check = manager.check_for_updates().map_err(|e| e.to_string())?;
            let found = match check {
                UpdateCheck::UpdateAvailable(info) => Some(*info),
                _ => None,
            };

            let mut verified_sha = None;
            if let Some(ref info) = found {
                let pkg_dir = self.packages_dir()?;
                {
                    let mut s = self.shared.lock().unwrap();
                    s.snapshot.phase = UpdatePhase::Verifying;
                    s.snapshot.status = "正在核对本地缓存…".into();
                    s.notify_ui();
                }
                if verify_cached(&pkg_dir, &info.TargetFullRelease)? {
                    verified_sha = Some(info.TargetFullRelease.SHA256.clone());
                }
            }

            {
                let mut inner = self.inner.lock().unwrap();
                inner.available = found;
                inner.verified_sha = verified_sha;
            }
            self.refresh_snapshot();

            let mut s = self.shared.lock().unwrap();
            if s.snapshot.pending_restart {
                s.snapshot.phase = UpdatePhase::Downloaded;
                s.snapshot.status = format!(
                    "新版本 v{} 已在本地就绪，点击「重启并安装」即可更新",
                    s.snapshot.available_version.as_deref().unwrap_or("")
                );
            } else if let Some(ver) = s.snapshot.available_version.clone() {
                s.snapshot.phase = UpdatePhase::Available;
                s.snapshot.status = if s.snapshot.is_delta {
                    format!(
                        "发现新版本 v{ver}（支持增量更新：约 {}，完整包 {}）",
                        format_bytes(s.snapshot.download_bytes),
                        format_bytes(s.snapshot.full_bytes)
                    )
                } else {
                    format!(
                        "发现新版本 v{ver}（完整更新包 {}）",
                        format_bytes(s.snapshot.full_bytes)
                    )
                };
            } else {
                s.snapshot.phase = UpdatePhase::Current;
                s.snapshot.status = format!(
                    "当前已是最新版本 v{}（来源：{}）",
                    APP_VERSION, s.snapshot.provider
                );
            }
            Ok(())
        } else {
            // Standalone mode: still fetch releases.win.json directly so user can verify CNB/GitHub URL!
            let mirrors = self.inner.lock().unwrap().mirrors.clone();
            let feed = mirrors
                .fetch_feed_internal("win")
                .map_err(|e| e.to_string())?;
            let current = semver::Version::parse(APP_VERSION)
                .unwrap_or_else(|_| semver::Version::new(0, 1, 0));

            let best = feed
                .Assets
                .iter()
                .filter(|a| a.Type.eq_ignore_ascii_case("Full"))
                .filter_map(|a| semver::Version::parse(&a.Version).ok().map(|v| (v, a)))
                .max_by(|a, b| a.0.cmp(&b.0));

            let has_delta = feed
                .Assets
                .iter()
                .any(|a| a.Type.eq_ignore_ascii_case("Delta"));

            let mut s = self.shared.lock().unwrap();
            if let Some((ver, asset)) = best {
                if ver > current {
                    s.snapshot.available_version = Some(asset.Version.clone());
                    s.snapshot.notes = asset.NotesMarkdown.clone();
                    s.snapshot.full_bytes = asset.Size;
                    s.snapshot.download_bytes = asset.Size;
                    s.snapshot.is_delta = has_delta;
                    s.snapshot.phase = UpdatePhase::Available;
                    s.snapshot.status = format!(
                        "发现新版本 v{}（独立运行模式，请打开发布页或使用 Velopack 安装包启用增量更新）",
                        asset.Version
                    );
                    return Ok(());
                }
            }
            s.snapshot.available_version = None;
            s.snapshot.phase = UpdatePhase::Current;
            s.snapshot.status = format!(
                "当前已是最新版本 v{}（来源：{}，独立运行模式）",
                APP_VERSION, s.snapshot.provider
            );
            Ok(())
        }
    }

    fn packages_dir(&self) -> Result<PathBuf, String> {
        let inner = self.inner.lock().unwrap();
        if let Some(dir) = inner.packages_dir.get() {
            return Ok(dir.clone());
        }
        let dir = velopack::locator::auto_locate_app_manifest(
            velopack::locator::LocationContext::FromCurrentExe,
        )
        .map(|l| l.get_packages_dir())
        .map_err(|e| e.to_string())?;
        let _ = inner.packages_dir.set(dir.clone());
        Ok(dir)
    }

    fn run_download_sync(&self) -> Result<(), String> {
        let (manager, update) = {
            let inner = self.inner.lock().unwrap();
            let manager = inner
                .manager
                .clone()
                .ok_or_else(|| "当前未启用 Velopack 托管更新".to_string())?;
            let update = inner
                .available
                .clone()
                .ok_or_else(|| "请先检查更新".to_string())?;
            (manager, update)
        };

        let pkg_dir = self.packages_dir()?;
        let target = &update.TargetFullRelease;

        if verify_cached(&pkg_dir, target)? {
            {
                let mut inner = self.inner.lock().unwrap();
                inner.verified_sha = Some(target.SHA256.clone());
            }
            self.refresh_snapshot();
            let mut s = self.shared.lock().unwrap();
            s.snapshot.phase = UpdatePhase::Downloaded;
            s.snapshot.progress = 100;
            s.snapshot.status = format!("新版本 v{} 已就绪，点击「重启并安装」完成更新", target.Version);
            return Ok(());
        } else {
            let path = cached_path(&pkg_dir, target)?;
            let _ = fs::remove_file(&path);
        }

        let is_delta = !update.DeltasToTarget.is_empty();
        {
            let mut s = self.shared.lock().unwrap();
            s.snapshot.phase = UpdatePhase::Downloading;
            s.snapshot.progress = 0;
            s.snapshot.status = if is_delta {
                "正在增量下载并合成更新包（0%）…".into()
            } else {
                "正在下载更新包（0%）…".into()
            };
            s.notify_ui();
        }

        let (tx, rx) = mpsc::channel::<i16>();
        let shared_prog = self.shared.clone();
        let prog_thread = std::thread::spawn(move || {
            for pct in rx {
                let pct = pct.clamp(0, 100);
                let mut s = shared_prog.lock().unwrap();
                s.snapshot.progress = pct;
                if pct >= 100 {
                    s.snapshot.phase = UpdatePhase::Verifying;
                    s.snapshot.status = if is_delta {
                        "正在应用增量补丁并校验更新包…".into()
                    } else {
                        "正在校验更新包…".into()
                    };
                } else {
                    s.snapshot.phase = UpdatePhase::Downloading;
                    s.snapshot.status = if is_delta {
                        format!("正在增量下载更新（{pct}%）…")
                    } else {
                        format!("正在下载更新（{pct}%）…")
                    };
                }
                s.notify_ui();
            }
        });

        let dl_res = manager
            .download_updates(&update, Some(tx))
            .map_err(|e| e.to_string());
        let _ = prog_thread.join();
        dl_res?;

        if self.shared.lock().unwrap().cancel_token.is_cancelled() {
            return Err("更新操作已取消".into());
        }

        if !verify_cached(&pkg_dir, target)? {
            return Err("更新包校验失败，请重试下载".into());
        }

        {
            let mut inner = self.inner.lock().unwrap();
            inner.verified_sha = Some(target.SHA256.clone());
        }
        self.refresh_snapshot();
        {
            let mut s = self.shared.lock().unwrap();
            s.snapshot.phase = UpdatePhase::Downloaded;
            s.snapshot.progress = 100;
            s.snapshot.status = format!(
                "新版本 v{} 下载校验完成，点击「重启并安装」即可更新",
                target.Version
            );
        }
        Ok(())
    }

    fn run_apply_sync(&self, on_exit: &Arc<dyn Fn() + Send + Sync>) -> Result<(), String> {
        let (manager, pending) = {
            let inner = self.inner.lock().unwrap();
            let manager = inner
                .manager
                .clone()
                .ok_or_else(|| "当前未启用 Velopack 托管更新".to_string())?;
            let pending = inner
                .available
                .as_ref()
                .map(|u| u.TargetFullRelease.clone())
                .or_else(|| manager.get_update_pending_restart())
                .ok_or_else(|| "更新包尚未准备好，请重新下载".to_string())?;
            (manager, pending)
        };

        let pkg_dir = self.packages_dir()?;
        if !verify_cached(&pkg_dir, &pending)? {
            self.inner.lock().unwrap().verified_sha = None;
            self.refresh_snapshot();
            return Err("本地缓存校验不匹配，请重新下载".into());
        }

        {
            let mut s = self.shared.lock().unwrap();
            s.snapshot.phase = UpdatePhase::Applying;
            s.snapshot.status = format!("正在重启并安装 v{}…", pending.Version);
            s.notify_ui();
        }

        manager
            .wait_exit_then_apply_updates(&pending, false, true, Vec::<String>::new())
            .map_err(|e| format!("启动更新安装程序失败：{e}"))?;

        on_exit();
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn normalizes_cnb_github_and_custom_urls() {
        let cnb = normalize_mirror_url(
            "https://cnb.cool/railgunhamster/RHPicture",
            UpdateProvider::Cnb,
            false,
        )
        .unwrap();
        assert_eq!(
            cnb.feed_url("releases.win.json"),
            "https://cnb.cool/railgunhamster/RHPicture/-/releases/latest/download/releases.win.json"
        );
        let asset = VelopackAsset {
            Version: "0.1.2".into(),
            FileName: "RHPicture-0.1.2-delta.nupkg".into(),
            ..Default::default()
        };
        assert_eq!(
            cnb.asset_urls(&asset).unwrap()[0],
            "https://cnb.cool/railgunhamster/RHPicture/-/releases/download/v0.1.2/RHPicture-0.1.2-delta.nupkg"
        );

        let gh = normalize_mirror_url(
            "https://github.com/RailgunHamster/RHPicture.git",
            UpdateProvider::Github,
            false,
        )
        .unwrap();
        assert_eq!(
            gh.feed_url("releases.win.json"),
            "https://github.com/RailgunHamster/RHPicture/releases/latest/download/releases.win.json"
        );
        assert_eq!(
            gh.asset_urls(&asset).unwrap()[0],
            "https://github.com/RailgunHamster/RHPicture/releases/download/v0.1.2/RHPicture-0.1.2-delta.nupkg"
        );

        // Reject insecure HTTP or path traversal
        assert!(
            normalize_mirror_url("http://github.com/a/b", UpdateProvider::Github, false).is_err()
        );
        assert!(
            cnb.asset_urls(&VelopackAsset {
                Version: "0.1.2".into(),
                FileName: "../escape.nupkg".into(),
                ..Default::default()
            })
            .is_err()
        );
    }

    #[test]
    fn mirrors_fallback_from_preferred_to_secondary_and_download_delta() {
        let server = tiny_http::Server::http("127.0.0.1:0").unwrap();
        let addr = server.server_addr().to_ip().unwrap();
        let worker = std::thread::spawn(move || {
            let mut seen = Vec::new();
            // 1st request: primary mirror returns 503
            let req1 = server.recv_timeout(Duration::from_secs(5)).unwrap().unwrap();
            seen.push(req1.url().to_string());
            req1.respond(tiny_http::Response::from_string("down").with_status_code(503))
                .unwrap();

            // 2nd request: secondary mirror returns feed with Full + Delta assets
            let req2 = server.recv_timeout(Duration::from_secs(5)).unwrap().unwrap();
            seen.push(req2.url().to_string());
            let feed_json = r#"{"Assets":[{"PackageId":"RHPicture","Version":"0.2.0","Type":"Full","FileName":"RHPicture-0.2.0-full.nupkg","Size":12,"SHA256":"","SHA1":""},{"PackageId":"RHPicture","Version":"0.2.0","Type":"Delta","FileName":"RHPicture-0.2.0-delta.nupkg","Size":5,"SHA256":"","SHA1":""}]}"#;
            req2.respond(tiny_http::Response::from_string(feed_json).with_status_code(200))
                .unwrap();

            // 3rd request: download delta from the working secondary mirror first
            let req3 = server.recv_timeout(Duration::from_secs(5)).unwrap().unwrap();
            seen.push(req3.url().to_string());
            req3.respond(tiny_http::Response::from_data(b"delta".as_slice()).with_status_code(200))
                .unwrap();
            seen
        });

        let cfg = UpdateConfig {
            preferred_source: UpdateProvider::Cnb,
            cnb_url: format!("http://{addr}/cnb"),
            github_url: format!("http://{addr}/github"),
            ..Default::default()
        };
        let shared = Arc::new(Mutex::new(SharedState {
            config: cfg,
            snapshot: UpdateSnapshot::default(),
            selected_mirror: None,
            cancel_token: CancellationToken::new(),
            allow_loopback_http: true,
            notify: None,
        }));
        let mirrors = Mirrors { shared };

        let feed = mirrors.get_release_feed("win", &Manifest::default(), "").unwrap();
        assert_eq!(feed.Assets.len(), 2);
        let delta = &feed.Assets[1];
        let tmp = std::env::temp_dir().join(format!("rhpicture-delta-{}.nupkg", std::process::id()));
        mirrors.download_release_entry(delta, &tmp, None).unwrap();
        assert_eq!(fs::read(&tmp).unwrap(), b"delta");
        let _ = fs::remove_file(&tmp);

        let seen = worker.join().unwrap();
        assert_eq!(
            seen,
            vec![
                "/cnb/releases.win.json",
                "/github/releases.win.json",
                "/github/RHPicture-0.2.0-delta.nupkg"
            ]
        );
    }

    #[test]
    fn verify_cached_checks_sha256_and_rejects_corruption() {
        let dir = std::env::temp_dir().join(format!("rhpicture-verify-{}", std::process::id()));
        let _ = fs::create_dir_all(&dir);
        let fname = "RHPicture-0.2.0-full.nupkg";
        let path = dir.join(fname);
        fs::write(&path, b"valid-package-content").unwrap();
        let (sha1, sha256) = velopack::misc::calculate_sha1_sha256(&path).unwrap();

        let mut asset = VelopackAsset {
            PackageId: "RHPicture".into(),
            Version: "0.2.0".into(),
            Type: "Full".into(),
            FileName: fname.into(),
            Size: 21,
            SHA1: sha1,
            SHA256: sha256,
            ..Default::default()
        };
        assert!(verify_cached(&dir, &asset).unwrap());

        fs::write(&path, b"corrupted-pkg-content").unwrap();
        assert!(!verify_cached(&dir, &asset).unwrap());

        asset.FileName = "../bad.nupkg".into();
        assert!(verify_cached(&dir, &asset).is_err());

        let _ = fs::remove_dir_all(&dir);
    }
}
