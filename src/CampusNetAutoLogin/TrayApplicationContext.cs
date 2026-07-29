using System.Diagnostics;
using System.Drawing;

namespace CampusNetAutoLogin;

public sealed class TrayApplicationContext : ApplicationContext
{
    private readonly SettingsManager _settings;
    private readonly ICredentialProtector _credentialProtector;
    private readonly StartupManager _startupManager;
    private readonly EventLogger _logger;
    private readonly NetworkMonitor _monitor;
    private readonly DataCleaner _cleaner;
    private readonly SingleInstanceCoordinator _singleInstance;
    private readonly ISystemProxyManager _systemProxyManager;
    private readonly Control _dispatcher = new();
    private readonly AppIcons _icons;
    private readonly NotifyIcon _notifyIcon;
    private readonly ToolStripMenuItem _statusMenuItem;
    private readonly ToolStripMenuItem _lastLoginMenuItem;
    private readonly ToolStripMenuItem _pauseMenuItem;
    private readonly ToolStripMenuItem _autoStartMenuItem;
    private readonly ToolStripMenuItem _logoutMenuItem;
    private readonly ToolStripMenuItem _clearProxyMenuItem;

    private SettingsForm? _settingsForm;
    private TrayIconKind _currentTrayIconKind = TrayIconKind.Blue;
    private bool _initialized;
    private bool _exiting;

    public TrayApplicationContext(
        SettingsManager settings,
        ICredentialProtector credentialProtector,
        StartupManager startupManager,
        EventLogger logger,
        NetworkMonitor monitor,
        DataCleaner cleaner,
        SingleInstanceCoordinator singleInstance,
        ISystemProxyManager systemProxyManager)
    {
        _settings = settings;
        _credentialProtector = credentialProtector;
        _startupManager = startupManager;
        _logger = logger;
        _monitor = monitor;
        _cleaner = cleaner;
        _singleInstance = singleInstance;
        _systemProxyManager = systemProxyManager;
        _icons = AppIcons.Load();

        _dispatcher.CreateControl();
        _ = _dispatcher.Handle;

        ContextMenuStrip menu = new();
        _statusMenuItem = new ToolStripMenuItem("网络状态：等待检测")
        {
            Enabled = false
        };
        _lastLoginMenuItem = new ToolStripMenuItem("最近登录：暂无")
        {
            Enabled = false
        };

        ToolStripMenuItem openPortalItem =
            new("跳转到校园网登录界面");
        openPortalItem.Click += (_, _) => OpenPortal();

        ToolStripMenuItem detectItem = new("立即检测");
        detectItem.Click += (_, _) => _monitor.RequestDetection();

        ToolStripMenuItem reloginItem = new("立即重新登录");
        reloginItem.Click += (_, _) => _monitor.RequestManualLogin();

        _logoutMenuItem = new ToolStripMenuItem("注销校园网");
        _logoutMenuItem.Click += (_, _) => ConfirmAndLogout();

        _clearProxyMenuItem = new ToolStripMenuItem("清除系统代理并重连");
        _clearProxyMenuItem.Click += async (_, _) =>
            await ClearProxyAndReconnectAsync();

        _pauseMenuItem = new ToolStripMenuItem("暂停自动重连");
        _pauseMenuItem.Click += (_, _) =>
            _monitor.SetUserPaused(!_monitor.IsPaused);

        ToolStripMenuItem settingsItem = new("设置");
        settingsItem.Click += (_, _) => ShowSettings();

        _autoStartMenuItem = new ToolStripMenuItem("开机自启")
        {
            CheckOnClick = false
        };
        _autoStartMenuItem.Click += async (_, _) =>
            await ToggleAutoStartAsync();

        ToolStripMenuItem exitItem = new("退出");
        exitItem.Click += async (_, _) => await ExitApplicationAsync();

        menu.Items.AddRange(
        [
            _statusMenuItem,
            _lastLoginMenuItem,
            new ToolStripSeparator(),
            openPortalItem,
            detectItem,
            reloginItem,
            _logoutMenuItem,
            _clearProxyMenuItem,
            _pauseMenuItem,
            settingsItem,
            _autoStartMenuItem,
            exitItem
        ]);
        menu.Opening += async (_, _) => await RefreshMenuAsync();

        _notifyIcon = new NotifyIcon
        {
            Icon = _icons.Blue,
            Text = "校园网自动登录",
            Visible = true,
            ContextMenuStrip = menu
        };
        _notifyIcon.DoubleClick += (_, _) => ShowSettings();

        _monitor.StatusChanged += (_, snapshot) =>
            Dispatch(() => ApplyStatus(snapshot));
        _monitor.NotificationRequested += (_, notification) =>
            Dispatch(() => ShowNotification(notification));
        _settings.Changed += (_, _) =>
            Dispatch(UpdateLastLoginText);

        Application.Idle += InitializeOnce;
    }

    private void InitializeOnce(object? sender, EventArgs e)
    {
        if (_initialized)
        {
            return;
        }

        _initialized = true;
        Application.Idle -= InitializeOnce;

        _singleInstance.Listen(() => Dispatch(ShowSettings));
        UpdateLastLoginText();
        _monitor.Start();

        if (!_settings.Current.HasCredentials)
        {
            ShowSettings();
        }
    }

    private SettingsForm GetSettingsForm()
    {
        if (_settingsForm is null || _settingsForm.IsDisposed)
        {
            _settingsForm = new SettingsForm(
                _settings,
                _credentialProtector,
                _startupManager,
                _logger,
                () => _cleaner.CleanAsync(),
                () => _monitor.NotifySettingsChanged(),
                ExitAfterCleanup,
                _icons.Blue);
            _settingsForm.ApplyStatus(_monitor.Current);
        }

        return _settingsForm;
    }

    private void ShowSettings()
    {
        if (_exiting)
        {
            return;
        }

        SettingsForm form = GetSettingsForm();
        _ = form.ReloadAsync();
        form.ApplyStatus(_monitor.Current);

        if (!form.Visible)
        {
            form.Show();
        }

        if (form.WindowState == FormWindowState.Minimized)
        {
            form.WindowState = FormWindowState.Normal;
        }

        form.Activate();
        form.BringToFront();
    }

    private void ApplyStatus(NetworkStatusSnapshot snapshot)
    {
        _statusMenuItem.Text = $"网络状态：{snapshot.DisplayName}";
        _pauseMenuItem.Text = _monitor.IsPaused
            ? "恢复自动重连"
            : "暂停自动重连";
        _notifyIcon.Text = $"校园网自动登录 - {snapshot.DisplayName}";
        _logoutMenuItem.Enabled = snapshot.State != NetworkState.LoggingOut;
        ApplyTrayIcon(AppIcons.ForState(snapshot.State));

        if (_settingsForm is { IsDisposed: false })
        {
            _settingsForm.ApplyStatus(snapshot);
        }
    }

    private void ConfirmAndLogout()
    {
        const string message =
            "确定要注销当前校园网会话吗？\n\n" +
            "注销成功后程序会暂停自动重连，避免刚注销又被自动登录。" +
            "需要恢复联网时，可从托盘菜单选择“恢复自动重连”或“立即重新登录”。";

        if (MessageBox.Show(
                message,
                "确认注销校园网",
                MessageBoxButtons.YesNo,
                MessageBoxIcon.Question,
                MessageBoxDefaultButton.Button2) != DialogResult.Yes)
        {
            return;
        }

        _logoutMenuItem.Enabled = false;
        _monitor.RequestLogout();
    }

    private void ApplyTrayIcon(TrayIconKind kind)
    {
        if (_currentTrayIconKind == kind)
        {
            return;
        }

        _notifyIcon.Icon = kind == TrayIconKind.Blue
            ? _icons.Blue
            : _icons.Red;
        _currentTrayIconKind = kind;
    }

    private async Task ClearProxyAndReconnectAsync()
    {
        const string message =
            "此操作将清除当前 Windows 用户的手动代理、PAC 自动配置脚本和自动检测代理，然后立即重新检测并尝试连接校园网。\n\n" +
            "不会退出 Clash、V2Ray 等程序，不会关闭 VPN/TUN 网卡，也不会修改 WinHTTP 系统级代理。\n\n" +
            "是否继续？";

        if (MessageBox.Show(
                message,
                "确认清除系统代理",
                MessageBoxButtons.YesNo,
                MessageBoxIcon.Warning,
                MessageBoxDefaultButton.Button2) != DialogResult.Yes)
        {
            return;
        }

        _clearProxyMenuItem.Enabled = false;
        try
        {
            ProxyClearResult result =
                await _systemProxyManager.ClearCurrentUserProxyAsync()
                    .ConfigureAwait(true);
            if (!result.Succeeded)
            {
                MessageBox.Show(
                    result.Message,
                    "清除系统代理失败",
                    MessageBoxButtons.OK,
                    MessageBoxIcon.Error);
                return;
            }

            try
            {
                await _logger.LogAsync(
                    "system-proxy-cleared",
                    "用户清除了当前用户的 WinINet 代理设置并请求重新连接。")
                    .ConfigureAwait(true);
            }
            catch
            {
                // 代理已成功清除，日志异常不应阻止重新连接。
            }

            _monitor.RequestReconnectNow();
            ShowNotification(
                new AppNotification(
                    NotificationKind.Information,
                    "系统代理已清除",
                    "正在重新检测并连接校园网。"));
        }
        catch (Exception exception)
        {
            MessageBox.Show(
                "清除系统代理失败：" + exception.Message,
                "校园网自动登录",
                MessageBoxButtons.OK,
                MessageBoxIcon.Error);
        }
        finally
        {
            _clearProxyMenuItem.Enabled = true;
        }
    }

    private void UpdateLastLoginText()
    {
        DateTimeOffset? lastLogin = _settings.Current.LastSuccessfulLoginUtc;
        _lastLoginMenuItem.Text = lastLogin.HasValue
            ? $"最近登录：{lastLogin.Value.ToLocalTime():yyyy-MM-dd HH:mm:ss}"
            : "最近登录：暂无";
        _settingsForm?.RefreshLastLogin();
    }

    private async Task RefreshMenuAsync()
    {
        _pauseMenuItem.Text = _monitor.IsPaused
            ? "恢复自动重连"
            : "暂停自动重连";

        try
        {
            _autoStartMenuItem.Checked =
                await _startupManager.IsEnabledAsync().ConfigureAwait(true);
        }
        catch
        {
            _autoStartMenuItem.Checked = _settings.Current.AutoStart;
        }
    }

    private async Task ToggleAutoStartAsync()
    {
        bool target = !_autoStartMenuItem.Checked;
        _autoStartMenuItem.Enabled = false;
        try
        {
            if (target)
            {
                await _startupManager.EnableAsync().ConfigureAwait(true);
            }
            else
            {
                await _startupManager.DisableAsync().ConfigureAwait(true);
            }

            await _settings.UpdateAsync(current => current with
            {
                AutoStart = target
            }).ConfigureAwait(true);
            _autoStartMenuItem.Checked = target;
            await _logger.LogAsync(
                "autostart-changed",
                target ? "已启用开机自启。" : "已关闭开机自启。").ConfigureAwait(true);
        }
        catch (Exception exception)
        {
            MessageBox.Show(
                "修改开机自启失败：" + exception.Message,
                "校园网自动登录",
                MessageBoxButtons.OK,
                MessageBoxIcon.Error);
        }
        finally
        {
            _autoStartMenuItem.Enabled = true;
        }
    }

    private void OpenPortal()
    {
        try
        {
            Process.Start(new ProcessStartInfo("http://10.2.5.251/")
            {
                UseShellExecute = true
            });
        }
        catch (Exception exception)
        {
            MessageBox.Show(
                "无法打开登录页面：" + exception.Message,
                "校园网自动登录",
                MessageBoxButtons.OK,
                MessageBoxIcon.Error);
        }
    }

    private void ShowNotification(AppNotification notification)
    {
        ToolTipIcon icon = notification.Kind switch
        {
            NotificationKind.Warning => ToolTipIcon.Warning,
            NotificationKind.Error => ToolTipIcon.Error,
            _ => ToolTipIcon.Info
        };

        _notifyIcon.ShowBalloonTip(
            timeout: 5000,
            tipTitle: notification.Title,
            tipText: notification.Message,
            tipIcon: icon);
    }

    private async Task ExitApplicationAsync()
    {
        if (_exiting)
        {
            return;
        }

        _exiting = true;
        _notifyIcon.Visible = false;
        try
        {
            await _monitor.StopAsync().ConfigureAwait(true);
        }
        finally
        {
            if (_settingsForm is { IsDisposed: false })
            {
                _settingsForm.AllowClose();
                _settingsForm.Close();
            }

            ExitThread();
        }
    }

    private void ExitAfterCleanup()
    {
        _exiting = true;
        _notifyIcon.Visible = false;
        if (_settingsForm is { IsDisposed: false })
        {
            _settingsForm.AllowClose();
            _settingsForm.Close();
        }

        ExitThread();
    }

    private void Dispatch(Action action)
    {
        if (_dispatcher.IsDisposed)
        {
            return;
        }

        if (_dispatcher.InvokeRequired)
        {
            try
            {
                _dispatcher.BeginInvoke(action);
            }
            catch (InvalidOperationException)
            {
                // 正在退出消息循环。
            }
        }
        else
        {
            action();
        }
    }

    protected override void Dispose(bool disposing)
    {
        if (disposing)
        {
            Application.Idle -= InitializeOnce;
            _notifyIcon.Visible = false;
            _notifyIcon.Dispose();
            _settingsForm?.Dispose();
            _icons.Dispose();
            _dispatcher.Dispose();
        }

        base.Dispose(disposing);
    }
}
