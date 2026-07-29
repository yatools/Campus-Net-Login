namespace CampusNetAutoLogin;

public sealed record CleanupResult(
    bool Succeeded,
    IReadOnlyList<string> FailedPaths,
    IReadOnlyList<string> RemainingBundleCachePaths,
    string? Error);

public sealed class DataCleaner
{
    private readonly NetworkMonitor _monitor;
    private readonly StartupManager _startupManager;
    private readonly EventLogger _logger;

    public DataCleaner(
        NetworkMonitor monitor,
        StartupManager startupManager,
        EventLogger logger)
    {
        _monitor = monitor;
        _startupManager = startupManager;
        _logger = logger;
    }

    public async Task<CleanupResult> CleanAsync()
    {
        try
        {
            await _startupManager.DisableAsync().ConfigureAwait(false);
        }
        catch (Exception exception)
        {
            return new CleanupResult(
                false,
                [],
                [],
                "删除开机任务失败：" + exception.Message);
        }

        await _monitor.StopAsync().ConfigureAwait(false);
        await _logger.DisposeAsync().ConfigureAwait(false);

        List<string> failedPaths = [];
        DeleteProgramData(failedPaths);
        List<string> remainingCache = DeleteBundleCaches();

        return new CleanupResult(
            failedPaths.Count == 0,
            failedPaths,
            remainingCache,
            failedPaths.Count == 0 ? null : "部分应用数据无法删除。");
    }

    private static void DeleteProgramData(List<string> failedPaths)
    {
        string dataDirectory = Path.GetFullPath(AppPaths.DataDirectory);
        string localAppData = Path.GetFullPath(
            Environment.GetFolderPath(Environment.SpecialFolder.LocalApplicationData));

        if (!dataDirectory.StartsWith(
                localAppData.TrimEnd(Path.DirectorySeparatorChar) + Path.DirectorySeparatorChar,
                StringComparison.OrdinalIgnoreCase) ||
            !Path.GetFileName(dataDirectory).Equals(
                AppPaths.AppName,
                StringComparison.OrdinalIgnoreCase))
        {
            failedPaths.Add(dataDirectory);
            return;
        }

        if (!Directory.Exists(dataDirectory))
        {
            return;
        }

        try
        {
            Directory.Delete(dataDirectory, recursive: true);
        }
        catch (Exception exception) when (
            exception is IOException or UnauthorizedAccessException)
        {
            failedPaths.Add(dataDirectory);
        }
    }

    private static List<string> DeleteBundleCaches()
    {
        List<string> remaining = [];
        string root = AppPaths.BundleCacheRoot;
        if (!Directory.Exists(root))
        {
            return remaining;
        }

        foreach (string directory in Directory.EnumerateDirectories(root))
        {
            try
            {
                Directory.Delete(directory, recursive: true);
            }
            catch (Exception exception) when (
                exception is IOException or UnauthorizedAccessException)
            {
                remaining.Add(directory);
            }
        }

        try
        {
            if (!Directory.EnumerateFileSystemEntries(root).Any())
            {
                Directory.Delete(root);
            }
        }
        catch (Exception exception) when (
            exception is IOException or UnauthorizedAccessException)
        {
            if (!remaining.Contains(root, StringComparer.OrdinalIgnoreCase))
            {
                remaining.Add(root);
            }
        }

        return remaining;
    }
}
