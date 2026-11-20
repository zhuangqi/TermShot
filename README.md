# TermShot

TermShot 是常驻系统托盘或菜单栏的截图工具，支持 Windows 10/11 x64，并提供 macOS 14+ 开发预览。Windows 版支持矩形、箭头、画笔和文字标注。保存后可把文件路径粘贴到本机终端。

## Windows 安装

在 [GitHub Releases](https://github.com/zhuangqi/TermShot/releases/latest) 下载 `TermShot-*-Windows-x64-Setup.exe` 并运行。安装完成后，从 Windows 开始菜单打开 TermShot。

## macOS 安装

在同一个 [GitHub Release](https://github.com/zhuangqi/TermShot/releases/latest) 下载 `TermShot-*-macOS-Universal.dmg`。打开磁盘映像后，将 TermShot 拖入「应用程序」。此开发预览支持 macOS 14+、Apple Silicon 和 Intel，未经过 Apple 公证，首次打开可能被 Gatekeeper 拦截。

## 截图

1. 启动 TermShot 后按 `Alt+Shift+S`。将鼠标移到窗口上可预览窗口范围，单击选中窗口，也可以拖动鼠标自由框选。
2. 选定截图区域后默认画矩形，也可从工具栏选择箭头、画笔或文字。文字框从一个汉字宽度开始，到选区右边缘自动换行，按 `Enter` 确认文字。文字放不下时会保留在编辑框中，缩短内容后可继续保存。
3. 每笔完成后可继续使用当前工具。点击「选择标注」再点击已有标注，就能移动标注或拖动白色控制点调整大小。箭头的两个控制点可调整方向和长度。选中后可用工具栏的色板改色，或按 `Delete` 删除。按 `Ctrl+Z` 撤销，按 `Ctrl+Y` 重做。鼠标悬停在工具栏图标上可查看用途，工具栏可从左端拖动。
4. 选区内外均可双击完成截图，也可按 `Enter` 或点击工具栏的 `✓`，完成后会复制截图路径。点击 `×` 可放弃截图。输入文字时，按 `Esc` 取消当前文字输入。
5. 回到终端，用平时的粘贴快捷键粘贴路径。在 Windows Terminal 里可按 `Ctrl+Shift+V`。图片的引用语法由 CLI 决定，例如 Codex 的 `-i "路径"`，或 Pi 的 `"@路径"`。命令行参数中的路径有空格时加引号。

## 设置与清理

右键点击托盘中的 TermShot 图标，选择「设置」修改全局截图快捷键和自动删除时间。首次运行使用 `Alt+Shift+S`。要更换主键，点击当前主键（或用 Tab 移到该处），直接按下字母、数字或 F1–F11，再保存设置。已经保存过的快捷键可在设置中更换。

「开机启动」默认关闭。开启并保存后，TermShot 会在当前用户登录 Windows 时自动启动到系统托盘。

新安装的截图保存在 `%LOCALAPPDATA%\TermShot\captures`。从 CliScreenshot 升级时，先退出旧版，再启动 TermShot，原有设置和截图目录会继续使用。可以通过托盘里的「打开截图目录」查看截图。默认在保存后 **10 分钟**自动删除。修改保留时间时，已有截图按各自的保存时间重新计算。工具退出期间到期的文件会在下次启动时清理。已经粘贴的路径会在文件删除后失效。

## 构建

需要 Rust 1.86 或更高版本、`x86_64-pc-windows-msvc` 工具链、MSVC 构建工具与 Windows SDK。

```powershell
cargo test --locked
cargo build --release --locked
```

可执行文件位于 `target\release\TermShot.exe`。

如需生成安装包，先完成发布构建，再使用 Inno Setup 6.7 或更高版本的 `ISCC.exe`。

```powershell
$version = (cargo metadata --no-deps --format-version 1 | ConvertFrom-Json).packages[0].version
ISCC.exe "/DAppVersion=$version" installer\TermShot.iss
```

安装包位于 `target\installer`。

## macOS 14+ 开发预览

安装 Rust 和 Xcode Command Line Tools 后，在 Mac 上执行：

```sh
sh macos/build-app.sh
open target/release/TermShot.app
```

应用驻留菜单栏。按 `Option+Shift+S` 或点击菜单中的「截图」，首次使用时允许屏幕录制。拖动时会显示最终截图的像素尺寸，松开鼠标保存 PNG 并复制绝对路径。点击浮动提示中的「取消」或按 `Esc` 可退出截图。可跨显示器框选，输出使用选区涉及的最高 backing scale。截图位于 `~/Library/Application Support/TermShot/captures`，默认保留 10 分钟。应用关闭时到期的截图在下次启动时清理。

使用 macOS 26 或更新版本的 SDK 编译后，浮动截图控件在 macOS 26 及更新系统上使用原生 Liquid Glass。macOS 14 和 15 使用系统 HUD 材质。

此构建脚本生成当前 Mac 架构的本地签名应用。Windows 的标注编辑、窗口吸附、自定义快捷键和登录启动目前仅在 Windows 版提供。

发布用通用磁盘映像可用 `sh macos/build-app.sh --dmg` 构建。与 `Cargo.toml` 版本匹配的 `v*` 标签会触发 GitHub Actions，并在两个安装包构建通过后将它们上传到同一个 Release 草稿。
