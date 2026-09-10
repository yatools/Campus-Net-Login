# 南湖校园网自动登录

面向 Windows 10/11 x64 的轻量原生托盘程序。程序使用 Rust 和 Windows 自带接口实现，发布物只有一个 EXE。

## 功能

- 自动检测公网，并在瞬时故障后等待 2 秒再次确认。
- 公网不可用时自动向校园网认证服务器登录，每轮尝试登录一次；默认失败冷却 20 秒，连续第 3 次失败后翻倍，此后每次失败继续翻倍，上限为设置中的检测间隔（默认 1800 秒），恢复联网后重置。
- 可选“仅开机自启时自动登录一次”：仅 `--autostart` 启动时执行一次自动检测/登录，已有网络则结束，失败不重试；手动启动不自动联网。手动登录仍可使用。
- 成功、失败通知分别控制，默认只开启失败通知。登录成功及注销完成归入成功通知；错误和警告归入失败通知。升级后首次使用这两个新选项时采用上述默认值；设置输入错误和删除确认仍在操作窗口显示。
- 新配置默认检测间隔 1800 秒、失败冷却 20 秒；已有配置保留用户设置。
- 设置窗口采用紧凑左右边距，网络状态位于最近成功登录下方，右上角空位提供小型 GitHub 按钮；输入文字按实际字体高度上下居中，账号和密码使用较短输入框，清除按钮使用静态红色样式。
- 支持校园网、移动、联通和电信账号后缀。
- 密码由 Windows DPAPI 按当前用户加密保存。
- 双公网探测、失败冷却、暂停/恢复、立即检测和立即登录。
- 可直接打开校园网登录页，也可从托盘注销当前校园网会话。
- 注销成功后自动暂停重连，避免程序立即再次登录；需要联网时可从托盘恢复。
- 原生托盘菜单与设置窗口，无 WebView 或 GUI 框架运行时。
- 设置窗口采用 Per-Monitor V2 高 DPI、对话框单位布局和当前屏幕工作区居中，可适配常见 1080p、2K、4K 显示器及 100%–200% 缩放；跨屏移动时由 Windows 按目标屏幕 DPI 重排。
- 设置窗口使用按需界面线程，关闭后立即释放高 DPI 输入控件占用的线程与句柄。
- 使用当前用户的 `HKCU\Software\Microsoft\Windows\CurrentVersion\Run` 标准启动项；Windows 登录后程序立即进入后台并开始检查网络，不额外等待。
- 单实例运行；再次启动会打开现有程序的设置窗口。
- 确认后可清除账号、加密密码、设置和自启项并退出。

网络请求由 WinHTTP 直接连接，禁用系统代理和自动跳转，并设置明确超时。校园网认证地址本身使用 HTTP；DPAPI 保护的是本机静态存储，不能改变认证服务器的传输协议。

## 兼容旧版本

- 继续读取 `%LocalAppData%\CampusNetAutoLogin\settings.json` 的 schema v1/v2/v3/v4/v5；已移除按时段、星期自动暂停的功能，旧配置中的相关字段不再生效，保存时会移除。
- 沿用 `CampusNetAutoLogin/v1/CurrentUser` DPAPI entropy，旧版保存的密码无需重输。
- 自启项名称为 `CampusNetAutoLogin`，命令保留 `--autostart` 参数；EXE 移动后，下次启动会自动更新启动项中的程序路径。
- 不创建或读取 Windows 任务计划，也不获取当前用户 SID；注册表通过原生 Windows API 直接操作。
- 配置通过同目录临时文件、落盘刷新和原子替换保存。


## TODO
加一个保存后提示右下角右键托盘可手动登录的提示/动画

加一个清除系统代理登录的功能
......



## 构建

开发电脑需要 Rust 1.98 GNU 工具链和 64 位 MinGW-w64，`cargo`、`gcc` 与 `windres` 应在 `PATH` 中。

```powershell
.\build-release.ps1
```

脚本会先运行测试，再执行尺寸优化、LTO、静态 GNU 运行库链接和符号剥离，并生成：

```text
publish\南湖校园网自动登陆.exe
```

也可以分别执行：

```powershell
cargo test --locked --all-targets
cargo build --release --locked
```

原生窗口和单实例冒烟测试会短暂打开一次设置窗口：

```powershell
.\tests\ui-smoke.ps1
```

由于仓库路径可能包含中文，发布脚本会把中间目标目录放到系统临时目录，规避部分 MinGW 链接器对非 ASCII 路径的兼容问题。

## 本地数据

- 配置：`%LocalAppData%\CampusNetAutoLogin\settings.json`
- 自启：`HKCU\Software\Microsoft\Windows\CurrentVersion\Run` 中的 `CampusNetAutoLogin`

程序空闲时不写日志、不解压运行库，也不调用 `curl`、`ping`、`reg.exe`、PowerShell、CMD 或 `schtasks.exe`。
