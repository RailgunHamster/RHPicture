# RHPicture

超轻量、**零显存占用（0 MB VRAM）**、纯 Rust 编写的 Windows 截图与贴图工具（参考 PixPin / Snipaste 交互与 RHRemote 增量更新机制）。

## 设计目标与资源占用

- **零 GPU / 显存开销**：全程使用纯 Win32 原生 API + CPU 软绘制（GDI `DIBSection` + `tiny-skia` 小区域矢量图标），不创建任何 Direct3D / OpenGL / Vulkan / WebView2 / Qt 渲染上下文。
- **极低内存常驻**：
  - 托盘后台常驻仅占用约 **5–10 MB 内存**；
  - 截图结束（复制 / 保存 / 贴图 / 取消）后**立即释放**全屏截图缓存并回收工作集；
  - 贴图隐藏时自动将大图像无损压缩为 PNG 存于内存，显著降低常驻内存，恢复时毫秒级还原。

## 核心功能

### 1. 全局快捷键（可在设置界面自由自定义）
- **开始截图**：默认 `F1`
- **剪贴板贴图**：默认 `F3`（将剪贴板里的图片直接贴到屏幕上）
- **隐藏 / 显示全部贴图**：默认 `Shift+F3`

### 2. 截图交互（参考 PixPin / Snipaste）
- **智能窗口识别**：鼠标悬停自动识别窗口边界，单击即可选中，或按住左键自由框选。
- **像素级放大镜与取色**：悬停时显示像素网格与 `#RRGGBB` 色值，按 `C` 直接复制色值。
- **像素级微调**：选区完成后，方向键 `↑↓←→` 移动 1px（`Alt` 移动 10px），`Ctrl+方向键` 扩展，`Shift+方向键` 收缩，`Ctrl+A` 选中当前显示器，右键取消选区重选。
- **三种处理方式**：
  1. **复制到剪贴板**：点击「复制」、按 `Enter` / `Ctrl+C` 或**双击选区**（同时写入 `CF_DIB` 与 `PNG` 剪贴板格式）。
  2. **保存到本地**：点击「保存」或按 `Ctrl+S`（支持 PNG / JPEG / BMP，可在设置中选择弹出另存为或直接静默保存到默认目录）。
  3. **贴到屏幕上**：点击「贴图」或按 `F3` / `T`。

### 3. 屏幕贴图交互
- **拖动**：左键按住任意位置拖动。
- **缩放**：鼠标滚轮以光标为中心平滑缩放（`10%–800%`），`+`/`-` 键缩放，`0` 键重置 100%。
- **透明度**：`Ctrl + 滚轮`（或 `Alt + 滚轮`）调节透明度，或右键菜单快速选择。
- **隐藏与恢复**：双击贴图（可在设置切换动作）、按 `H`、右键菜单「隐藏此贴图」，或全局快捷键 `Shift+F3` 批量隐藏/恢复；托盘菜单可恢复任意单个隐藏贴图。
- **更多快捷操作**：`Ctrl+C` 复制贴图、`Ctrl+S` 保存贴图、`R` 顺时针旋转 90°、`Esc` 关闭贴图、右键支持鼠标穿透（可从托盘菜单一键取消）。

### 4. 设置界面与 CNB / GitHub 增量更新（参考 RHRemote）
- 点击托盘图标或右键「设置与快捷键…」打开原生设置界面：
  - **快捷键与常规**：按键录制绑定、开机自启、智能窗口识别、放大镜开关。
  - **贴图与保存**：默认保存目录与格式、双击贴图行为、实时内存/显存与贴图统计。
  - **更新设置**：
    - 支持自定义填写 **CNB 仓库地址**（如 `https://cnb.cool/用户名/RHPicture`）与 **GitHub 仓库地址**（如 `https://github.com/用户名/RHPicture`）；
    - 支持设置首选更新源并在失败时自动回退到另一源；
    - 支持配置 HTTP/HTTPS 更新代理、取消正在进行的检查/下载、SHA256 完整性校验；
    - 通过 **Velopack** 原生支持 **增量差分更新（`*-delta.nupkg`）** 与完整包更新。

## 构建与打包

### 开发构建与单元测试
```powershell
cargo test
cargo build --release
```

### 打包 Velopack 安装包 / 便携包（支持增量更新）
```powershell
# 打包当前 Cargo.toml 版本（保留 dist\releases 中的旧版 *-full.nupkg 即可自动生成 *-delta.nupkg 增量补丁）
pwsh -File tools\package-windows-velopack.ps1
```

### 发布到 CNB 或 GitHub
```powershell
# 发布到 CNB（需设置 $env:CNB_TOKEN 或本地 DPAPI token 文件）
pwsh -File tools\publish-release.ps1 -Provider cnb -Repository railgunhamster/RHPicture -Version 0.1.0

# 发布到 GitHub（需已登录 gh CLI）
pwsh -File tools\publish-release.ps1 -Provider github -Repository RailgunHamster/RHPicture -Version 0.1.0
```
