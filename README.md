# CampusNetAutoLogin

面向 Windows 10/11 x64 的校园网托盘自动登录程序，使用 .NET 10 和 WinForms。

## 界面

### 托盘图标菜单
<img width="327" height="311" alt="image" src="https://github.com/user-attachments/assets/cba77f26-7a7f-4b66-a194-c268f7c97080" />

### 设置界面
<img width="673" height="627" alt="image" src="https://github.com/user-attachments/assets/7a408e5b-c75c-4a09-b1cd-17ceab26dac0" />



## 功能

- 启动后隐藏到系统托盘，首次运行且没有凭据时自动打开设置。
- 自动生成移动、联通、电信运营商账号后缀。
- 使用 Windows DPAPI `CurrentUser` 加密本地密码。
- 异步检测公网、确认瞬时故障并按 5/10/20 秒退避登录。
- 失败冷却支持 `-1–3600` 秒：`-1` 表示失败后暂停自动重连，`0` 表示不额外冷却。
- 程序网络请求始终直接连接，不经过 Windows 系统代理。
- 托盘可注销当前校园网会话；成功后自动暂停重连，避免立即重新登录。
- 托盘可清除当前用户的手动代理、PAC 和自动检测代理并立即重连。
- 通过 Windows 任务计划程序 COM API 管理当前用户开机自启。
- 限制日志大小与保留时间，支持二次确认后清除应用数据。

> 校园网认证地址使用 HTTP。DPAPI 只能保护本机静态存储，无法改变认证服务器的网络传输方式。

## 运行要求

发布版本面向 Windows 10/11 x64，已包含运行所需的 .NET 10 组件，目标电脑不需要另外安装 .NET Desktop Runtime。

程序采用自包含、单文件、压缩和兼容性优先的 `partial` 剪裁。首次启动或版本更新后，Windows 可能需要短暂解压原生运行库，启动速度会比框架依赖版稍慢。

## 构建

需要 Windows x64 和 .NET 10 SDK：

```powershell
dotnet build .\CampusNetAutoLogin.sln -c Release -p:Platform=x64
```

运行自动化测试：

```powershell
dotnet run --project .\tests\CampusNetAutoLogin.Tests\CampusNetAutoLogin.Tests.csproj -c Release -p:Platform=x64
```

发布不依赖目标电脑安装 .NET 的剪裁单 EXE：

```powershell
dotnet publish .\src\CampusNetAutoLogin\CampusNetAutoLogin.csproj -c Release -r win-x64 --self-contained true -o .\publish
```

本项目交付文件位于 `publish\南湖校园网自动登陆.exe`。发布目录只需分发这一个文件。

## 代理清理边界

“清除系统代理并重连”只修改当前 Windows 用户的 WinINet 代理配置：

- 清除手动代理服务器与绕过列表；
- 清除 PAC 自动配置脚本；
- 关闭自动检测代理；
- 不退出 Clash、V2Ray 等第三方程序；
- 不关闭 VPN/TUN 网卡；
- 不修改 WinHTTP 系统级代理。

## 本地数据

- 配置：`%LocalAppData%\CampusNetAutoLogin\settings.json`
- 日志：`%LocalAppData%\CampusNetAutoLogin\logs`
- 自包含单文件缓存：`%TEMP%\.net\CampusNetAutoLogin`

自包含单文件会在该目录解压少量原生运行库。程序运行时正在使用的缓存无法保证立即删除；“清除所有数据并退出”会尽力清理可删除的缓存，并准确提示仍需在退出后手动删除的路径。

正常联网状态不会写日志。应用不会调用 `curl`、`ping`、PowerShell、CMD 或 `schtasks.exe`。

校园网注销使用认证页实测的最小请求：

```text
GET http://10.2.5.251:801/eportal/?c=Portal&a=logout
```

请求不携带账号、密码、终端 IP 或 MAC；只有响应明确返回 `result=1` 或 `result=ok` 时才报告注销成功。
