using System.Globalization;
using System.Text;

namespace CampusNetAutoLogin;

public sealed class EventLogger : IAsyncDisposable
{
    private const long MaximumLogFileBytes = 1024 * 1024;
    private const int MaximumLogFiles = 7;

    private readonly string _logsDirectory;
    private readonly SemaphoreSlim _gate = new(1, 1);
    private bool _disposed;

    public EventLogger(string? logsDirectory = null)
    {
        _logsDirectory = logsDirectory ?? AppPaths.LogsDirectory;
    }

    public async Task LogAsync(
        string eventName,
        string message,
        CancellationToken cancellationToken = default)
    {
        if (_disposed)
        {
            return;
        }

        string sanitizedEvent = Sanitize(eventName);
        string sanitizedMessage = Sanitize(message);

        await _gate.WaitAsync(cancellationToken).ConfigureAwait(false);
        try
        {
            if (_disposed)
            {
                return;
            }

            Directory.CreateDirectory(_logsDirectory);
            string filePath = Path.Combine(
                _logsDirectory,
                DateTime.Now.ToString("yyyy-MM-dd", CultureInfo.InvariantCulture) + ".log");

            if (File.Exists(filePath) && new FileInfo(filePath).Length >= MaximumLogFileBytes)
            {
                return;
            }

            string line =
                $"{DateTimeOffset.Now:O}\t{sanitizedEvent}\t{sanitizedMessage}{Environment.NewLine}";
            byte[] bytes = Encoding.UTF8.GetBytes(line);

            await using FileStream stream = new(
                filePath,
                FileMode.Append,
                FileAccess.Write,
                FileShare.Read,
                bufferSize: 4096,
                FileOptions.Asynchronous);
            await stream.WriteAsync(bytes, cancellationToken).ConfigureAwait(false);
            await stream.FlushAsync(cancellationToken).ConfigureAwait(false);

            DeleteExpiredLogs();
        }
        catch (IOException)
        {
            // 日志绝不能影响联网主流程。
        }
        catch (UnauthorizedAccessException)
        {
            // 日志绝不能影响联网主流程。
        }
        finally
        {
            _gate.Release();
        }
    }

    private void DeleteExpiredLogs()
    {
        DirectoryInfo directory = new(_logsDirectory);
        FileInfo[] files = directory
            .EnumerateFiles("*.log", SearchOption.TopDirectoryOnly)
            .OrderByDescending(file => file.Name, StringComparer.Ordinal)
            .ToArray();

        foreach (FileInfo file in files.Skip(MaximumLogFiles))
        {
            try
            {
                file.Delete();
            }
            catch (IOException)
            {
                // 下次写日志时再尝试。
            }
            catch (UnauthorizedAccessException)
            {
                // 下次写日志时再尝试。
            }
        }
    }

    private static string Sanitize(string value) =>
        value
            .Replace('\r', ' ')
            .Replace('\n', ' ')
            .Replace('\t', ' ')
            .Trim();

    public async ValueTask DisposeAsync()
    {
        if (_disposed)
        {
            return;
        }

        await _gate.WaitAsync().ConfigureAwait(false);
        try
        {
            _disposed = true;
        }
        finally
        {
            _gate.Release();
        }
    }
}
