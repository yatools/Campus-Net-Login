using System.Net;
using CampusNetAutoLogin;

namespace CampusNetAutoLogin.Tests;

internal static class Program
{
    private static readonly List<(string Name, Func<Task> Test)> Tests =
    [
        ("服务商后缀映射", TestProviderMappingAsync),
        ("设置验证与原子保存", TestSettingsStoreAsync),
        ("旧版零冷却迁移为负一", TestCooldownMigrationAsync),
        ("DPAPI 当前用户加解密", TestDpapiAsync),
        ("登录 URL 编码", TestLoginUriEncodingAsync),
        ("注销 URL 与响应解析", TestLogoutProtocolAsync),
        ("主探测成功时不请求备用地址", TestPrimaryProbeAsync),
        ("主探测失败后使用备用地址", TestFallbackProbeAsync),
        ("网络客户端始终绕过系统代理", TestDirectNetworkClientAsync),
        ("托盘图标按网络状态切换", TestTrayIconMappingAsync),
        ("任务计划程序 COM 可读取", TestTaskSchedulerReadAsync),
        ("任务计划程序创建与删除", TestTaskSchedulerLifecycleAsync),
        ("瞬时断网二次确认后恢复", TestTransientFailureAsync),
        ("自动登录按 5/10/20 秒重试", TestAutomaticRetryAsync),
        ("冷却为零时按正常间隔继续", TestZeroCooldownContinuesAsync),
        ("冷却为负一时停止自动重试", TestNegativeCooldownPauseAsync),
        ("立即重连清除失败暂停但保留用户暂停", TestReconnectPauseBehaviorAsync),
        ("注销成功后暂停自动重连", TestLogoutSuccessAsync),
        ("注销失败时不误报成功", TestLogoutFailureAsync),
        ("首次登录成功更新时间", TestLoginSuccessAsync)
    ];

    [STAThread]
    private static async Task<int> Main()
    {
        int failed = 0;
        foreach ((string name, Func<Task> test) in Tests)
        {
            try
            {
                await test();
                Console.WriteLine($"PASS  {name}");
            }
            catch (Exception exception)
            {
                failed++;
                Console.Error.WriteLine($"FAIL  {name}");
                Console.Error.WriteLine(
                    $"      {exception.GetType().Name} 0x{exception.HResult:X8}: {exception.Message}");
                Console.Error.WriteLine(exception.StackTrace);
            }
        }

        Console.WriteLine();
        Console.WriteLine($"总计 {Tests.Count}，通过 {Tests.Count - failed}，失败 {failed}");
        return failed == 0 ? 0 : 1;
    }

    private static Task TestProviderMappingAsync()
    {
        Equal("11230909", Provider.Campus.BuildSubmittedAccount("11230909"));
        Equal("11230909@cmcc", Provider.ChinaMobile.BuildSubmittedAccount("11230909"));
        Equal("11230909@unicom", Provider.ChinaUnicom.BuildSubmittedAccount("11230909"));
        Equal("11230909@telecom", Provider.ChinaTelecom.BuildSubmittedAccount("11230909"));
        return Task.CompletedTask;
    }

    private static async Task TestSettingsStoreAsync()
    {
        using TempDirectory temp = new();
        string settingsPath = Path.Combine(temp.Path, "settings.json");
        SettingsStore store = new(settingsPath);
        AppSettings settings = new()
        {
            Account = " 11230909 ",
            Provider = Provider.ChinaMobile,
            EncryptedPassword = "cipher",
            CheckIntervalSeconds = 30,
            FailureCooldownSeconds = 300,
            FallbackProbeUrl = "https://example.com/tiny"
        };

        await store.SaveAsync(settings);
        AppSettings loaded = await store.LoadAsync();

        Equal("11230909", loaded.Account);
        Equal(Provider.ChinaMobile, loaded.Provider);
        Equal("cipher", loaded.EncryptedPassword);
        True(!File.Exists(settingsPath + ".tmp"), "原子保存后不应残留临时文件。");
        True(
            loaded.Validate(requirePassword: true).Count == 0,
            "有效设置不应产生验证错误。");

        AppSettings invalid = loaded with { Account = "11230909@cmcc" };
        True(
            invalid.Validate(requirePassword: true).Any(error => error.Contains("后缀")),
            "带 @ 的账号必须被拒绝。");
    }

    private static async Task TestCooldownMigrationAsync()
    {
        using TempDirectory temp = new();
        string settingsPath = Path.Combine(temp.Path, "settings.json");
        await File.WriteAllTextAsync(
            settingsPath,
            """
            {
              "schemaVersion": 1,
              "failureCooldownSeconds": 0
            }
            """);

        SettingsStore store = new(settingsPath);
        AppSettings migrated = await store.LoadAsync();
        Equal(AppSettings.CurrentSchemaVersion, migrated.SchemaVersion);
        Equal(-1, migrated.FailureCooldownSeconds);

        AppSettings zeroCooldown = new() { FailureCooldownSeconds = 0 };
        Equal(0, zeroCooldown.Normalize().FailureCooldownSeconds);
        True(
            !zeroCooldown.Validate(requirePassword: false)
                .Any(error => error.Contains("失败冷却")),
            "零冷却本身应有效。");

        AppSettings invalid = new()
        {
            Account = "11230909",
            FailureCooldownSeconds = -2
        };
        True(
            invalid.Validate(requirePassword: false)
                .Any(error => error.Contains("-1 到 3600")),
            "小于 -1 的失败冷却必须被拒绝。");
    }

    private static Task TestDpapiAsync()
    {
        CredentialProtector protector = new();
        const string password = "复杂 密码!@#";
        string encrypted = protector.Encrypt(password);

        True(encrypted != password, "配置中不能出现明文密码。");
        True(protector.TryDecrypt(encrypted, out string decrypted), "DPAPI 密文应可解密。");
        Equal(password, decrypted);
        True(!protector.TryDecrypt("not-base64", out _), "无效密文必须安全失败。");
        return Task.CompletedTask;
    }

    private static Task TestLoginUriEncodingAsync()
    {
        Uri uri = CampusAuthenticator.BuildLoginUri(
            "user name@cmcc",
            "p@ss word&=");
        string text = uri.OriginalString;

        Contains("c=Portal", text);
        Contains("a=login", text);
        Contains("login_method=1", text);
        Contains("user_account=user%20name%40cmcc", text);
        Contains("user_password=p%40ss%20word%26%3D", text);
        True(!text.Contains("p@ss word&=", StringComparison.Ordinal), "URL 不应包含未编码密码。");
        return Task.CompletedTask;
    }

    private static async Task TestLogoutProtocolAsync()
    {
        Uri logoutUri = CampusAuthenticator.BuildLogoutUri();
        Equal(
            "http://10.2.5.251:801/eportal/?c=Portal&a=logout",
            logoutUri.OriginalString);
        True(
            CampusAuthenticator.TryParseLogoutSuccess(
                "({\"result\":\"1\",\"msg\":\"注销成功\"})"),
            "实测注销响应必须识别为成功。");
        True(
            CampusAuthenticator.TryParseLogoutSuccess(
                "callback({\"result\":\"ok\"});"),
            "JSONP 的 ok 结果必须识别为成功。");
        True(
            !CampusAuthenticator.TryParseLogoutSuccess(
                "({\"result\":\"0\",\"msg\":\"注销失败\"})"),
            "result=0 不能误报成功。");
        True(
            !CampusAuthenticator.TryParseLogoutSuccess("not-json"),
            "无效响应不能误报成功。");

        QueueHttpHandler handler = new(
            new HttpResponseMessage(HttpStatusCode.OK)
            {
                Content = new StringContent(
                    "({\"result\":\"1\",\"msg\":\"注销成功\"})")
            });
        using HttpClient client = new(handler) { Timeout = Timeout.InfiniteTimeSpan };
        CampusAuthenticator authenticator = new(client);
        LogoutAttemptResult result =
            await authenticator.LogoutAsync(CancellationToken.None);

        True(result.Succeeded, "实测格式的 HTTP 200 响应应判定注销成功。");
        Equal(1, handler.RequestCount);
        Equal(logoutUri, handler.Requests.Single());
    }

    private static async Task TestPrimaryProbeAsync()
    {
        QueueHttpHandler handler = new(
            new HttpResponseMessage(HttpStatusCode.OK)
            {
                Content = new StringContent(ConnectivityProbe.PrimaryExpectedBody)
            });
        using HttpClient client = new(handler) { Timeout = Timeout.InfiniteTimeSpan };
        ConnectivityProbe probe = new(client);

        bool online = await probe.IsInternetAvailableAsync(new AppSettings(), CancellationToken.None);
        True(online, "精确匹配主探测正文时应判定在线。");
        Equal(1, handler.RequestCount);
    }

    private static async Task TestFallbackProbeAsync()
    {
        QueueHttpHandler handler = new(
            new HttpResponseMessage(HttpStatusCode.Redirect),
            new HttpResponseMessage(HttpStatusCode.NoContent));
        using HttpClient client = new(handler) { Timeout = Timeout.InfiniteTimeSpan };
        ConnectivityProbe probe = new(client);

        bool online = await probe.IsInternetAvailableAsync(
            new AppSettings { FallbackProbeUrl = "https://example.com/tiny" },
            CancellationToken.None);
        True(online, "备用 HTTPS 返回成功状态时应判定在线。");
        Equal(2, handler.RequestCount);
        Equal("https", handler.Requests[1].Scheme);
    }

    private static Task TestDirectNetworkClientAsync()
    {
        using HttpClientHandler handler = NetworkClientFactory.CreateHandler();
        True(!handler.UseProxy, "应用网络客户端必须明确绕过系统代理。");
        True(!handler.AllowAutoRedirect, "应用网络客户端必须禁用自动重定向。");
        True(handler.Proxy is null, "绕过代理时不应配置代理实例。");
        return Task.CompletedTask;
    }

    private static Task TestTrayIconMappingAsync()
    {
        using AppIcons icons = AppIcons.Load();
        True(icons.Blue.Width > 0 && icons.Blue.Height > 0, "蓝色托盘图标资源必须可加载。");
        True(icons.Red.Width > 0 && icons.Red.Height > 0, "红色托盘图标资源必须可加载。");

        Equal(TrayIconKind.Blue, AppIcons.ForState(NetworkState.Checking));
        Equal(TrayIconKind.Blue, AppIcons.ForState(NetworkState.Online));

        NetworkState[] redStates =
        [
            NetworkState.NotConfigured,
            NetworkState.Offline,
            NetworkState.Authenticating,
            NetworkState.LoggingOut,
            NetworkState.AuthServerUnavailable,
            NetworkState.LoginFailed,
            NetworkState.LogoutFailed,
            NetworkState.Paused
        ];

        foreach (NetworkState state in redStates)
        {
            Equal(TrayIconKind.Red, AppIcons.ForState(state));
        }

        return Task.CompletedTask;
    }

    private static async Task TestTaskSchedulerReadAsync()
    {
        StartupManager startupManager = new(Environment.ProcessPath);
        _ = await startupManager.IsEnabledAsync();
    }

    private static async Task TestTaskSchedulerLifecycleAsync()
    {
        string taskName = $"CampusNetAutoLogin-Test-{Guid.NewGuid():N}";
        StartupManager startupManager = new(Environment.ProcessPath, taskName);
        try
        {
            await startupManager.EnableAsync();
            True(await startupManager.IsEnabledAsync(), "创建后应能读取到临时开机任务。");
        }
        finally
        {
            await startupManager.DisableAsync();
        }

        True(!await startupManager.IsEnabledAsync(), "删除后不应再读取到临时开机任务。");
    }

    private static async Task TestTransientFailureAsync()
    {
        using MonitorFixture fixture = await MonitorFixture.CreateAsync(
            internetResults: [false, true],
            cooldownSeconds: 300);

        await fixture.Monitor.ExecuteDetectionCycleAsync(CancellationToken.None);

        Equal(NetworkState.Online, fixture.Monitor.Current.State);
        Equal(0, fixture.Authenticator.LoginCount);
        Equal(TimeSpan.FromSeconds(2), fixture.Delay.Delays.Single());
    }

    private static async Task TestAutomaticRetryAsync()
    {
        using MonitorFixture fixture = await MonitorFixture.CreateAsync(
            internetResults: [false, false, false, false, false, false],
            cooldownSeconds: 300);

        TimeSpan nextDelay =
            await fixture.Monitor.ExecuteDetectionCycleAsync(CancellationToken.None);

        Equal(4, fixture.Authenticator.LoginCount);
        Equal(NetworkState.LoginFailed, fixture.Monitor.Current.State);
        Equal(TimeSpan.FromSeconds(300), nextDelay);
        SequenceEqual(
            [
                2,
                3,
                5,
                3,
                10,
                3,
                20,
                3
            ],
            fixture.Delay.Delays.Select(delay => (int)delay.TotalSeconds));
    }

    private static async Task TestZeroCooldownContinuesAsync()
    {
        using MonitorFixture fixture = await MonitorFixture.CreateAsync(
            internetResults: [false, false, false, false, false, false],
            cooldownSeconds: 0);

        TimeSpan nextDelay =
            await fixture.Monitor.ExecuteDetectionCycleAsync(CancellationToken.None);

        Equal(4, fixture.Authenticator.LoginCount);
        Equal(NetworkState.LoginFailed, fixture.Monitor.Current.State);
        Equal(TimeSpan.FromSeconds(30), nextDelay);
        True(!fixture.Monitor.IsPaused, "零冷却不应暂停自动重连。");
    }

    private static async Task TestNegativeCooldownPauseAsync()
    {
        using MonitorFixture fixture = await MonitorFixture.CreateAsync(
            internetResults: [false, false, false, false, false, false],
            cooldownSeconds: -1);

        await fixture.Monitor.ExecuteDetectionCycleAsync(CancellationToken.None);

        Equal(4, fixture.Authenticator.LoginCount);
        Equal(NetworkState.Paused, fixture.Monitor.Current.State);
        True(fixture.Monitor.IsPaused, "负一冷却必须停止后续自动重试。");

        fixture.Monitor.SetUserPaused(false);
        True(!fixture.Monitor.IsPaused, "用户恢复后必须清除失败暂停。");
    }

    private static async Task TestReconnectPauseBehaviorAsync()
    {
        using MonitorFixture fixture = await MonitorFixture.CreateAsync(
            internetResults: [false, false, false, false, false, false],
            cooldownSeconds: -1);

        await fixture.Monitor.ExecuteDetectionCycleAsync(CancellationToken.None);
        True(fixture.Monitor.IsPaused, "测试前置条件：登录失败应触发失败暂停。");

        fixture.Monitor.RequestReconnectNow();
        True(!fixture.Monitor.IsPaused, "立即重连应清除登录失败造成的暂停。");

        fixture.Monitor.SetUserPaused(true);
        fixture.Monitor.RequestReconnectNow();
        True(fixture.Monitor.IsPaused, "立即重连不应解除用户主动暂停。");
        True(fixture.Monitor.IsUserPaused, "用户主动暂停状态必须保留。");
    }

    private static async Task TestLogoutSuccessAsync()
    {
        using MonitorFixture fixture = await MonitorFixture.CreateAsync(
            internetResults: [],
            cooldownSeconds: 300);

        await fixture.Monitor.ExecuteLogoutAsync(CancellationToken.None);

        Equal(1, fixture.Authenticator.LogoutCount);
        Equal(NetworkState.Paused, fixture.Monitor.Current.State);
        True(fixture.Monitor.IsUserPaused, "注销成功后必须暂停自动重连。");
    }

    private static async Task TestLogoutFailureAsync()
    {
        using MonitorFixture fixture = await MonitorFixture.CreateAsync(
            internetResults: [],
            cooldownSeconds: 300);
        fixture.Authenticator.LogoutResult =
            LogoutAttemptResult.Failed("认证服务器拒绝注销。", 500);

        await fixture.Monitor.ExecuteLogoutAsync(CancellationToken.None);

        Equal(1, fixture.Authenticator.LogoutCount);
        Equal(NetworkState.LogoutFailed, fixture.Monitor.Current.State);
        True(!fixture.Monitor.IsPaused, "注销失败时不能误暂停或误报成功。");
    }

    private static async Task TestLoginSuccessAsync()
    {
        using MonitorFixture fixture = await MonitorFixture.CreateAsync(
            internetResults: [false, false, true],
            cooldownSeconds: 300);

        await fixture.Monitor.ExecuteDetectionCycleAsync(CancellationToken.None);

        Equal(1, fixture.Authenticator.LoginCount);
        Equal(NetworkState.Online, fixture.Monitor.Current.State);
        True(
            fixture.Settings.Current.LastSuccessfulLoginUtc.HasValue,
            "登录成功后必须记录最近成功时间。");
    }

    private static void True(bool condition, string message)
    {
        if (!condition)
        {
            throw new InvalidOperationException(message);
        }
    }

    private static void Equal<T>(T expected, T actual)
        where T : notnull
    {
        if (!EqualityComparer<T>.Default.Equals(expected, actual))
        {
            throw new InvalidOperationException($"期望：{expected}；实际：{actual}");
        }
    }

    private static void Contains(string expected, string actual)
    {
        if (!actual.Contains(expected, StringComparison.Ordinal))
        {
            throw new InvalidOperationException($"未找到：{expected}{Environment.NewLine}{actual}");
        }
    }

    private static void SequenceEqual(
        IEnumerable<int> expected,
        IEnumerable<int> actual)
    {
        int[] expectedArray = expected.ToArray();
        int[] actualArray = actual.ToArray();
        if (!expectedArray.SequenceEqual(actualArray))
        {
            throw new InvalidOperationException(
                $"期望：{string.Join(",", expectedArray)}；实际：{string.Join(",", actualArray)}");
        }
    }

    private sealed class QueueHttpHandler : HttpMessageHandler
    {
        private readonly Queue<HttpResponseMessage> _responses;

        public QueueHttpHandler(params HttpResponseMessage[] responses)
        {
            _responses = new Queue<HttpResponseMessage>(responses);
        }

        public int RequestCount { get; private set; }
        public List<Uri> Requests { get; } = [];

        protected override Task<HttpResponseMessage> SendAsync(
            HttpRequestMessage request,
            CancellationToken cancellationToken)
        {
            RequestCount++;
            Requests.Add(request.RequestUri!);
            if (_responses.Count == 0)
            {
                throw new InvalidOperationException("测试响应队列为空。");
            }

            return Task.FromResult(_responses.Dequeue());
        }
    }

    private sealed class SequenceProbe : IConnectivityProbe
    {
        private readonly Queue<bool> _internetResults;

        public SequenceProbe(IEnumerable<bool> internetResults)
        {
            _internetResults = new Queue<bool>(internetResults);
        }

        public Task<bool> IsInternetAvailableAsync(
            AppSettings settings,
            CancellationToken cancellationToken)
        {
            if (_internetResults.Count == 0)
            {
                throw new InvalidOperationException("公网探测测试序列已耗尽。");
            }

            return Task.FromResult(_internetResults.Dequeue());
        }

        public Task<bool> IsAuthenticationServerReachableAsync(
            CancellationToken cancellationToken) =>
            Task.FromResult(true);
    }

    private sealed class FakeAuthenticator : ICampusAuthenticator
    {
        public int LoginCount { get; private set; }
        public int LogoutCount { get; private set; }
        public LogoutAttemptResult LogoutResult { get; set; } =
            LogoutAttemptResult.Success(200);

        public Task<LoginAttemptResult> LoginAsync(
            string submittedAccount,
            string password,
            CancellationToken cancellationToken)
        {
            LoginCount++;
            return Task.FromResult(new LoginAttemptResult(true, 200, null));
        }

        public Task<LogoutAttemptResult> LogoutAsync(
            CancellationToken cancellationToken)
        {
            LogoutCount++;
            return Task.FromResult(LogoutResult);
        }
    }

    private sealed class FakeCredentialProtector : ICredentialProtector
    {
        public string Encrypt(string plaintext) => "cipher";

        public bool TryDecrypt(string encryptedBase64, out string plaintext)
        {
            plaintext = "password";
            return true;
        }
    }

    private sealed class RecordingDelay : IAsyncDelay
    {
        public List<TimeSpan> Delays { get; } = [];

        public Task DelayAsync(TimeSpan delay, CancellationToken cancellationToken)
        {
            Delays.Add(delay);
            return Task.CompletedTask;
        }
    }

    private sealed class MonitorFixture : IDisposable
    {
        private readonly TempDirectory _temp;

        private MonitorFixture(
            TempDirectory temp,
            SettingsManager settings,
            FakeAuthenticator authenticator,
            RecordingDelay delay,
            EventLogger logger,
            NetworkMonitor monitor)
        {
            _temp = temp;
            Settings = settings;
            Authenticator = authenticator;
            Delay = delay;
            Logger = logger;
            Monitor = monitor;
        }

        public SettingsManager Settings { get; }
        public FakeAuthenticator Authenticator { get; }
        public RecordingDelay Delay { get; }
        public EventLogger Logger { get; }
        public NetworkMonitor Monitor { get; }

        public static async Task<MonitorFixture> CreateAsync(
            IEnumerable<bool> internetResults,
            int cooldownSeconds)
        {
            TempDirectory temp = new();
            SettingsStore store = new(Path.Combine(temp.Path, "settings.json"));
            SettingsManager settings = new(store);
            await settings.InitializeAsync();
            await settings.UpdateAsync(current => current with
            {
                Account = "11230909",
                Provider = Provider.ChinaMobile,
                EncryptedPassword = "cipher",
                CheckIntervalSeconds = 30,
                FailureCooldownSeconds = cooldownSeconds,
                FallbackProbeUrl = "https://example.com/tiny",
                SuccessNotification = true
            });

            FakeAuthenticator authenticator = new();
            RecordingDelay delay = new();
            EventLogger logger = new(Path.Combine(temp.Path, "logs"));
            NetworkMonitor monitor = new(
                settings,
                new SequenceProbe(internetResults),
                authenticator,
                new FakeCredentialProtector(),
                logger,
                delay);

            return new MonitorFixture(
                temp,
                settings,
                authenticator,
                delay,
                logger,
                monitor);
        }

        public void Dispose()
        {
            Monitor.DisposeAsync().AsTask().GetAwaiter().GetResult();
            Logger.DisposeAsync().AsTask().GetAwaiter().GetResult();
            _temp.Dispose();
        }
    }

    private sealed class TempDirectory : IDisposable
    {
        public TempDirectory()
        {
            Path = System.IO.Path.Combine(
                System.IO.Path.GetTempPath(),
                "CampusNetAutoLogin.Tests",
                Guid.NewGuid().ToString("N"));
            Directory.CreateDirectory(Path);
        }

        public string Path { get; }

        public void Dispose()
        {
            try
            {
                Directory.Delete(Path, recursive: true);
            }
            catch (IOException)
            {
                // 测试进程退出后由系统临时目录清理。
            }
        }
    }
}
