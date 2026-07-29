using System.Threading.Channels;

namespace CampusNetAutoLogin;

public sealed class NetworkMonitor : IAsyncDisposable
{
    private static readonly TimeSpan[] RetryDelays =
    [
        TimeSpan.Zero,
        TimeSpan.FromSeconds(5),
        TimeSpan.FromSeconds(10),
        TimeSpan.FromSeconds(20)
    ];

    private readonly SettingsManager _settings;
    private readonly IConnectivityProbe _probe;
    private readonly ICampusAuthenticator _authenticator;
    private readonly ICredentialProtector _credentials;
    private readonly EventLogger _logger;
    private readonly IAsyncDelay _delay;
    private readonly TimeProvider _timeProvider;
    private readonly Channel<MonitorCommand> _commands;
    private readonly CancellationTokenSource _stop = new();
    private readonly object _operationGate = new();

    private Task? _worker;
    private CancellationTokenSource? _activeOperation;
    private NetworkStatusSnapshot _current = NetworkStatusSnapshot.Initial;
    private volatile bool _userPaused;
    private volatile bool _failurePaused;
    private bool _offlineEpisode;
    private bool _disposed;

    public NetworkMonitor(
        SettingsManager settings,
        IConnectivityProbe probe,
        ICampusAuthenticator authenticator,
        ICredentialProtector credentials,
        EventLogger logger,
        IAsyncDelay? delay = null,
        TimeProvider? timeProvider = null)
    {
        _settings = settings;
        _probe = probe;
        _authenticator = authenticator;
        _credentials = credentials;
        _logger = logger;
        _delay = delay ?? new SystemAsyncDelay();
        _timeProvider = timeProvider ?? TimeProvider.System;
        _commands = Channel.CreateUnbounded<MonitorCommand>(
            new UnboundedChannelOptions
            {
                SingleReader = true,
                SingleWriter = false,
                AllowSynchronousContinuations = false
            });
    }

    public event EventHandler<NetworkStatusSnapshot>? StatusChanged;
    public event EventHandler<AppNotification>? NotificationRequested;

    public NetworkStatusSnapshot Current => Volatile.Read(ref _current);

    public bool IsUserPaused => _userPaused;

    public bool IsPaused => _userPaused || _failurePaused;

    public void Start()
    {
        ObjectDisposedException.ThrowIf(_disposed, this);
        _worker ??= Task.Run(RunAsync);
    }

    public void RequestDetection() => Queue(MonitorCommand.Detect);

    public void RequestManualLogin() => Queue(MonitorCommand.ManualLogin);

    public void RequestLogout()
    {
        CancelActiveOperation();
        Queue(MonitorCommand.Logout);
    }

    public void RequestReconnectNow()
    {
        _failurePaused = false;
        CancelActiveOperation();
        Queue(MonitorCommand.Reconnect);
    }

    public void NotifySettingsChanged() => Queue(MonitorCommand.SettingsChanged);

    public void SetUserPaused(bool paused)
    {
        _userPaused = paused;
        if (!paused)
        {
            _failurePaused = false;
        }

        Queue(MonitorCommand.PauseChanged);
    }

    private void Queue(MonitorCommand command)
    {
        if (!_disposed)
        {
            _commands.Writer.TryWrite(command);
        }
    }

    private async Task RunAsync()
    {
        TimeSpan nextDelay = TimeSpan.Zero;
        CancellationToken cancellationToken = _stop.Token;

        try
        {
            while (!cancellationToken.IsCancellationRequested)
            {
                MonitorCommand command = await WaitForCommandAsync(
                    nextDelay,
                    cancellationToken).ConfigureAwait(false);

                using CancellationTokenSource operationCancellation =
                    CancellationTokenSource.CreateLinkedTokenSource(cancellationToken);
                SetActiveOperation(operationCancellation);
                try
                {
                    CancellationToken operationToken = operationCancellation.Token;
                    switch (command)
                    {
                        case MonitorCommand.Stop:
                            return;

                        case MonitorCommand.ManualLogin:
                            await ExecuteManualLoginAsync(operationToken).ConfigureAwait(false);
                            nextDelay = NormalInterval();
                            break;

                        case MonitorCommand.Logout:
                            await ExecuteLogoutAsync(operationToken).ConfigureAwait(false);
                            nextDelay = NormalInterval();
                            break;

                        case MonitorCommand.Reconnect:
                            _failurePaused = false;
                            nextDelay = await ExecuteDetectionCycleAsync(operationToken)
                                .ConfigureAwait(false);
                            break;

                        case MonitorCommand.PauseChanged:
                            if (_userPaused || _failurePaused)
                            {
                                Publish(NetworkState.Paused, "自动重连已暂停，仍会继续检测网络。");
                                nextDelay = NormalInterval();
                            }
                            else
                            {
                                nextDelay = TimeSpan.Zero;
                            }

                            break;

                        case MonitorCommand.SettingsChanged:
                            nextDelay = TimeSpan.Zero;
                            break;

                        default:
                            nextDelay = await ExecuteDetectionCycleAsync(operationToken)
                                .ConfigureAwait(false);
                            break;
                    }
                }
                catch (OperationCanceledException) when (
                    !cancellationToken.IsCancellationRequested &&
                    operationCancellation.IsCancellationRequested)
                {
                    // 新的立即重连请求抢占了当前检测、退避或冷却。
                    nextDelay = TimeSpan.Zero;
                }
                finally
                {
                    ClearActiveOperation(operationCancellation);
                }
            }
        }
        catch (OperationCanceledException) when (cancellationToken.IsCancellationRequested)
        {
            // 正常退出。
        }
        catch (Exception exception)
        {
            await _logger.LogAsync(
                "monitor-crash",
                exception.GetType().Name + ": " + exception.Message).ConfigureAwait(false);
            Publish(NetworkState.LoginFailed, "网络监控发生异常。");
            RaiseNotification(
                NotificationKind.Error,
                "校园网自动登录",
                "网络监控发生异常，请打开设置后重试。");
        }
    }

    private void SetActiveOperation(CancellationTokenSource operation)
    {
        lock (_operationGate)
        {
            _activeOperation = operation;
        }
    }

    private void ClearActiveOperation(CancellationTokenSource operation)
    {
        lock (_operationGate)
        {
            if (ReferenceEquals(_activeOperation, operation))
            {
                _activeOperation = null;
            }
        }
    }

    private void CancelActiveOperation()
    {
        lock (_operationGate)
        {
            try
            {
                _activeOperation?.Cancel();
            }
            catch (ObjectDisposedException)
            {
                // 操作刚好结束。
            }
        }
    }

    private async Task<MonitorCommand> WaitForCommandAsync(
        TimeSpan delay,
        CancellationToken cancellationToken)
    {
        if (_commands.Reader.TryRead(out MonitorCommand queued))
        {
            return queued;
        }

        if (delay <= TimeSpan.Zero)
        {
            return MonitorCommand.Detect;
        }

        using CancellationTokenSource timerCancellation =
            CancellationTokenSource.CreateLinkedTokenSource(cancellationToken);
        Task timerTask = _delay.DelayAsync(delay, timerCancellation.Token);
        Task<bool> commandTask = _commands.Reader
            .WaitToReadAsync(cancellationToken)
            .AsTask();

        Task completed = await Task.WhenAny(timerTask, commandTask).ConfigureAwait(false);
        if (completed == commandTask && await commandTask.ConfigureAwait(false))
        {
            timerCancellation.Cancel();
            try
            {
                await timerTask.ConfigureAwait(false);
            }
            catch (OperationCanceledException)
            {
                // 命令唤醒计时器。
            }

            return _commands.Reader.TryRead(out MonitorCommand command)
                ? command
                : MonitorCommand.Detect;
        }

        await timerTask.ConfigureAwait(false);
        return MonitorCommand.Detect;
    }

    internal async Task<TimeSpan> ExecuteDetectionCycleAsync(
        CancellationToken cancellationToken)
    {
        AppSettings settings = _settings.Current;
        if (!settings.HasCredentials)
        {
            Publish(NetworkState.NotConfigured, "请先填写账号和密码。");
            return NormalInterval();
        }

        Publish(NetworkState.Checking, "正在检测公网连接。");
        if (await _probe.IsInternetAvailableAsync(settings, cancellationToken).ConfigureAwait(false))
        {
            _offlineEpisode = false;
            _failurePaused = false;
            Publish(NetworkState.Online, "公网连接正常。");
            return NormalInterval();
        }

        await _delay.DelayAsync(TimeSpan.FromSeconds(2), cancellationToken).ConfigureAwait(false);
        if (await _probe.IsInternetAvailableAsync(settings, cancellationToken).ConfigureAwait(false))
        {
            _offlineEpisode = false;
            _failurePaused = false;
            Publish(NetworkState.Online, "公网连接正常。");
            return NormalInterval();
        }

        if (!_offlineEpisode)
        {
            _offlineEpisode = true;
            await _logger.LogAsync(
                "network-offline",
                "连续两次公网探测失败。",
                cancellationToken).ConfigureAwait(false);
        }

        if (_userPaused || _failurePaused)
        {
            Publish(NetworkState.Paused, "网络不可用，自动重连当前已暂停。");
            return NormalInterval();
        }

        Publish(NetworkState.Offline, "公网不可用，正在检查认证服务器。");
        if (!await _probe.IsAuthenticationServerReachableAsync(cancellationToken).ConfigureAwait(false))
        {
            Publish(NetworkState.AuthServerUnavailable, "无法访问校园网认证服务器。");
            return NormalInterval();
        }

        return await ExecuteAutomaticLoginAsync(settings, cancellationToken).ConfigureAwait(false);
    }

    private async Task<TimeSpan> ExecuteAutomaticLoginAsync(
        AppSettings settings,
        CancellationToken cancellationToken)
    {
        if (!_credentials.TryDecrypt(settings.EncryptedPassword, out string password))
        {
            Publish(NetworkState.NotConfigured, "已保存的密码无法解密，请重新输入。");
            RaiseNotification(
                NotificationKind.Error,
                "校园网自动登录",
                "已保存的密码无法解密，请打开设置重新输入。");
            return NormalInterval();
        }

        try
        {
            string submittedAccount = settings.Provider.BuildSubmittedAccount(settings.Account);
            for (int index = 0; index < RetryDelays.Length; index++)
            {
                if (RetryDelays[index] > TimeSpan.Zero)
                {
                    await _delay.DelayAsync(RetryDelays[index], cancellationToken)
                        .ConfigureAwait(false);
                }

                Publish(
                    NetworkState.Authenticating,
                    $"正在登录（{index + 1}/{RetryDelays.Length}）。");
                await _logger.LogAsync(
                    "login-attempt",
                    $"开始第 {index + 1} 次登录。",
                    cancellationToken).ConfigureAwait(false);

                await _authenticator.LoginAsync(
                    submittedAccount,
                    password,
                    cancellationToken).ConfigureAwait(false);

                await _delay.DelayAsync(TimeSpan.FromSeconds(3), cancellationToken)
                    .ConfigureAwait(false);

                if (await _probe.IsInternetAvailableAsync(
                    _settings.Current,
                    cancellationToken).ConfigureAwait(false))
                {
                    await RecordSuccessAsync(cancellationToken).ConfigureAwait(false);
                    return NormalInterval();
                }
            }
        }
        finally
        {
            password = string.Empty;
        }

        Publish(NetworkState.LoginFailed, "多次登录后公网仍未恢复。");
        await _logger.LogAsync(
            "login-failed",
            "四次登录后公网仍未恢复。",
            cancellationToken).ConfigureAwait(false);
        RaiseNotification(
            NotificationKind.Error,
            "校园网登录失败",
            "多次尝试后公网仍未恢复，请检查账号、密码或运营商。");

        int cooldown = _settings.Current.FailureCooldownSeconds;
        if (cooldown < 0)
        {
            _failurePaused = true;
            Publish(NetworkState.Paused, "登录失败，已按设置停止自动重试。");
            return NormalInterval();
        }

        return cooldown == 0
            ? NormalInterval()
            : TimeSpan.FromSeconds(cooldown);
    }

    private async Task ExecuteManualLoginAsync(CancellationToken cancellationToken)
    {
        AppSettings settings = _settings.Current;
        if (!settings.HasCredentials)
        {
            Publish(NetworkState.NotConfigured, "请先填写账号和密码。");
            RaiseNotification(
                NotificationKind.Warning,
                "校园网自动登录",
                "请先在设置中填写账号和密码。");
            return;
        }

        if (!_credentials.TryDecrypt(settings.EncryptedPassword, out string password))
        {
            Publish(NetworkState.NotConfigured, "已保存的密码无法解密，请重新输入。");
            RaiseNotification(
                NotificationKind.Error,
                "校园网自动登录",
                "已保存的密码无法解密，请重新输入。");
            return;
        }

        try
        {
            Publish(NetworkState.Authenticating, "正在执行手动重新登录。");
            await _logger.LogAsync(
                "manual-login",
                "用户发起手动重新登录。",
                cancellationToken).ConfigureAwait(false);

            await _authenticator.LoginAsync(
                settings.Provider.BuildSubmittedAccount(settings.Account),
                password,
                cancellationToken).ConfigureAwait(false);
            await _delay.DelayAsync(TimeSpan.FromSeconds(3), cancellationToken)
                .ConfigureAwait(false);

            if (await _probe.IsInternetAvailableAsync(
                _settings.Current,
                cancellationToken).ConfigureAwait(false))
            {
                _failurePaused = false;
                await RecordSuccessAsync(cancellationToken).ConfigureAwait(false);
                return;
            }
        }
        finally
        {
            password = string.Empty;
        }

        Publish(NetworkState.LoginFailed, "手动登录后公网仍未恢复。");
        await _logger.LogAsync(
            "manual-login-failed",
            "手动登录后公网仍未恢复。",
            cancellationToken).ConfigureAwait(false);
        RaiseNotification(
            NotificationKind.Error,
            "校园网登录失败",
            "登录请求已发送，但公网仍未恢复。");
    }

    internal async Task ExecuteLogoutAsync(CancellationToken cancellationToken)
    {
        Publish(NetworkState.LoggingOut, "正在向校园网认证服务器发送注销请求。");
        await _logger.LogAsync(
            "logout-requested",
            "用户发起校园网注销。",
            cancellationToken).ConfigureAwait(false);

        LogoutAttemptResult result =
            await _authenticator.LogoutAsync(cancellationToken).ConfigureAwait(false);
        if (!result.Succeeded)
        {
            string error = result.Error ?? "认证服务器未确认注销成功。";
            Publish(NetworkState.LogoutFailed, error);
            await _logger.LogAsync(
                "logout-failed",
                error,
                cancellationToken).ConfigureAwait(false);
            RaiseNotification(
                NotificationKind.Error,
                "校园网注销失败",
                error);
            return;
        }

        _userPaused = true;
        _failurePaused = false;
        _offlineEpisode = false;
        Publish(NetworkState.Paused, "校园网已注销，自动重连已暂停。");
        await _logger.LogAsync(
            "logout-success",
            "校园网注销成功，自动重连已暂停。",
            cancellationToken).ConfigureAwait(false);
        RaiseNotification(
            NotificationKind.Information,
            "校园网已注销",
            "自动重连已暂停；需要联网时请从托盘恢复。");
    }

    private async Task RecordSuccessAsync(CancellationToken cancellationToken)
    {
        DateTimeOffset now = _timeProvider.GetUtcNow();
        _offlineEpisode = false;
        await _settings.UpdateAsync(
            current => current with { LastSuccessfulLoginUtc = now },
            cancellationToken).ConfigureAwait(false);
        Publish(NetworkState.Online, "登录成功，公网连接已恢复。");
        await _logger.LogAsync(
            "login-success",
            "公网连接已恢复。",
            cancellationToken).ConfigureAwait(false);

        if (_settings.Current.SuccessNotification)
        {
            RaiseNotification(
                NotificationKind.Success,
                "校园网登录成功",
                "公网连接已恢复。");
        }
    }

    private TimeSpan NormalInterval() =>
        TimeSpan.FromSeconds(_settings.Current.CheckIntervalSeconds);

    private void Publish(NetworkState state, string message)
    {
        NetworkStatusSnapshot snapshot =
            new(state, message, _timeProvider.GetUtcNow());
        Volatile.Write(ref _current, snapshot);
        StatusChanged?.Invoke(this, snapshot);
    }

    private void RaiseNotification(
        NotificationKind kind,
        string title,
        string message) =>
        NotificationRequested?.Invoke(this, new AppNotification(kind, title, message));

    public async Task StopAsync()
    {
        if (_disposed)
        {
            return;
        }

        _stop.Cancel();
        _commands.Writer.TryWrite(MonitorCommand.Stop);
        if (_worker is not null)
        {
            try
            {
                await _worker.ConfigureAwait(false);
            }
            catch (OperationCanceledException)
            {
                // 正常退出。
            }
        }
    }

    public async ValueTask DisposeAsync()
    {
        if (_disposed)
        {
            return;
        }

        await StopAsync().ConfigureAwait(false);
        _disposed = true;
        _stop.Dispose();
    }

    private enum MonitorCommand
    {
        Detect,
        ManualLogin,
        Logout,
        Reconnect,
        SettingsChanged,
        PauseChanged,
        Stop
    }
}
