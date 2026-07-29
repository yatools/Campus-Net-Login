namespace CampusNetAutoLogin;

internal static class Program
{
    [STAThread]
    private static void Main()
    {
        ApplicationConfiguration.Initialize();
        Application.SetUnhandledExceptionMode(UnhandledExceptionMode.CatchException);

        using SingleInstanceCoordinator singleInstance = new();
        if (!singleInstance.IsFirstInstance)
        {
            singleInstance.SignalFirstInstance();
            return;
        }

        SettingsStore settingsStore = new();
        SettingsManager settingsManager = new(settingsStore);
        EventLogger logger = new();
        NetworkMonitor? monitor = null;

        try
        {
            settingsManager.InitializeAsync().GetAwaiter().GetResult();
            CredentialProtector credentialProtector = new();
            StartupManager startupManager = new();

            if (settingsManager.Current.AutoStart)
            {
                try
                {
                    startupManager.EnableAsync().GetAwaiter().GetResult();
                }
                catch (Exception exception)
                {
                    logger.LogAsync(
                            "autostart-reconcile-failed",
                            exception.GetType().Name + ": " + exception.Message)
                        .GetAwaiter()
                        .GetResult();
                }
            }

            using HttpClient httpClient =
                NetworkClientFactory.CreateDirectClient();

            ConnectivityProbe probe = new(httpClient);
            CampusAuthenticator authenticator = new(httpClient);
            SystemProxyManager systemProxyManager = new();
            monitor = new NetworkMonitor(
                settingsManager,
                probe,
                authenticator,
                credentialProtector,
                logger);
            DataCleaner cleaner = new(monitor, startupManager, logger);

            Application.ThreadException += (_, args) =>
                SafeLog(logger, "ui-exception", args.Exception);
            AppDomain.CurrentDomain.UnhandledException += (_, args) =>
            {
                if (args.ExceptionObject is Exception exception)
                {
                    SafeLog(logger, "unhandled-exception", exception);
                }
            };

            using TrayApplicationContext context = new(
                settingsManager,
                credentialProtector,
                startupManager,
                logger,
                monitor,
                cleaner,
                singleInstance,
                systemProxyManager);
            Application.Run(context);
        }
        catch (Exception exception)
        {
            SafeLog(logger, "startup-failed", exception);
            MessageBox.Show(
                "程序启动失败：" + exception.Message,
                "校园网自动登录",
                MessageBoxButtons.OK,
                MessageBoxIcon.Error);
        }
        finally
        {
            if (monitor is not null)
            {
                try
                {
                    monitor.DisposeAsync().AsTask().GetAwaiter().GetResult();
                }
                catch
                {
                    // 退出阶段不再弹出异常。
                }
            }

            try
            {
                logger.DisposeAsync().AsTask().GetAwaiter().GetResult();
            }
            catch
            {
                // 退出阶段不再弹出异常。
            }
        }
    }

    private static void SafeLog(EventLogger logger, string eventName, Exception exception)
    {
        try
        {
            logger.LogAsync(
                    eventName,
                    exception.GetType().Name + ": " + exception.Message)
                .GetAwaiter()
                .GetResult();
        }
        catch
        {
            // 记录异常本身不能触发新的异常。
        }
    }
}
