using System.Text.Json;
using System.Text.Json.Serialization;

namespace CampusNetAutoLogin;

public sealed class SettingsStore
{
    private readonly string _settingsFile;
    private readonly SemaphoreSlim _gate = new(1, 1);

    public SettingsStore(string? settingsFile = null)
    {
        _settingsFile = settingsFile ?? AppPaths.SettingsFile;
    }

    public string SettingsFile => _settingsFile;

    public async Task<AppSettings> LoadAsync(CancellationToken cancellationToken = default)
    {
        if (!File.Exists(_settingsFile))
        {
            return new AppSettings();
        }

        await _gate.WaitAsync(cancellationToken).ConfigureAwait(false);
        try
        {
            await using FileStream stream = new(
                _settingsFile,
                FileMode.Open,
                FileAccess.Read,
                FileShare.Read,
                bufferSize: 4096,
                FileOptions.Asynchronous | FileOptions.SequentialScan);

            AppSettings? settings = await JsonSerializer.DeserializeAsync<AppSettings>(
                stream,
                SettingsJsonContext.Default.AppSettings,
                cancellationToken).ConfigureAwait(false);

            return (settings ?? new AppSettings()).Normalize();
        }
        catch (JsonException)
        {
            return new AppSettings();
        }
        catch (IOException)
        {
            return new AppSettings();
        }
        finally
        {
            _gate.Release();
        }
    }

    public async Task SaveAsync(AppSettings settings, CancellationToken cancellationToken = default)
    {
        ArgumentNullException.ThrowIfNull(settings);

        string? directory = Path.GetDirectoryName(_settingsFile);
        if (string.IsNullOrWhiteSpace(directory))
        {
            throw new InvalidOperationException("配置文件路径无效。");
        }

        Directory.CreateDirectory(directory);
        string temporaryFile = _settingsFile + ".tmp";

        await _gate.WaitAsync(cancellationToken).ConfigureAwait(false);
        try
        {
            await using (FileStream stream = new(
                temporaryFile,
                FileMode.Create,
                FileAccess.Write,
                FileShare.None,
                bufferSize: 4096,
                FileOptions.Asynchronous | FileOptions.WriteThrough))
            {
                await JsonSerializer.SerializeAsync(
                    stream,
                    settings.Normalize(),
                    SettingsJsonContext.Default.AppSettings,
                    cancellationToken).ConfigureAwait(false);
                await stream.FlushAsync(cancellationToken).ConfigureAwait(false);
            }

            File.Move(temporaryFile, _settingsFile, overwrite: true);
        }
        finally
        {
            if (File.Exists(temporaryFile))
            {
                try
                {
                    File.Delete(temporaryFile);
                }
                catch (IOException)
                {
                    // 下次保存时会覆盖该临时文件。
                }
            }

            _gate.Release();
        }
    }
}

[JsonSourceGenerationOptions(
    PropertyNamingPolicy = JsonKnownNamingPolicy.CamelCase,
    WriteIndented = true,
    GenerationMode = JsonSourceGenerationMode.Metadata)]
[JsonSerializable(typeof(AppSettings))]
internal partial class SettingsJsonContext : JsonSerializerContext
{
}

public sealed class SettingsManager
{
    private readonly SettingsStore _store;
    private readonly SemaphoreSlim _gate = new(1, 1);
    private AppSettings _current = new();

    public SettingsManager(SettingsStore store)
    {
        _store = store;
    }

    public event EventHandler<AppSettings>? Changed;

    public AppSettings Current => Volatile.Read(ref _current);

    public async Task InitializeAsync(CancellationToken cancellationToken = default)
    {
        AppSettings settings = await _store.LoadAsync(cancellationToken).ConfigureAwait(false);
        Volatile.Write(ref _current, settings);
    }

    public async Task<AppSettings> UpdateAsync(
        Func<AppSettings, AppSettings> update,
        CancellationToken cancellationToken = default)
    {
        ArgumentNullException.ThrowIfNull(update);

        await _gate.WaitAsync(cancellationToken).ConfigureAwait(false);
        try
        {
            AppSettings updated = update(Current).Normalize();
            IReadOnlyList<string> errors = updated.Validate(requirePassword: false);
            if (errors.Count > 0)
            {
                throw new InvalidOperationException(string.Join(Environment.NewLine, errors));
            }

            await _store.SaveAsync(updated, cancellationToken).ConfigureAwait(false);
            Volatile.Write(ref _current, updated);
            Changed?.Invoke(this, updated);
            return updated;
        }
        finally
        {
            _gate.Release();
        }
    }
}
