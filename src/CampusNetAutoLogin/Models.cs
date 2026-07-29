using System.Text.Json.Serialization;

namespace CampusNetAutoLogin;

[JsonConverter(typeof(JsonStringEnumConverter<Provider>))]
public enum Provider
{
    Campus,
    ChinaMobile,
    ChinaUnicom,
    ChinaTelecom
}

public static class ProviderExtensions
{
    public static string GetDisplayName(this Provider provider) => provider switch
    {
        Provider.Campus => "校园网",
        Provider.ChinaMobile => "中国移动",
        Provider.ChinaUnicom => "中国联通",
        Provider.ChinaTelecom => "中国电信",
        _ => throw new ArgumentOutOfRangeException(nameof(provider))
    };

    public static string GetSuffix(this Provider provider) => provider switch
    {
        Provider.Campus => string.Empty,
        Provider.ChinaMobile => "@cmcc",
        Provider.ChinaUnicom => "@unicom",
        Provider.ChinaTelecom => "@telecom",
        _ => throw new ArgumentOutOfRangeException(nameof(provider))
    };

    public static string BuildSubmittedAccount(this Provider provider, string account)
    {
        ArgumentException.ThrowIfNullOrWhiteSpace(account);
        return account.Trim() + provider.GetSuffix();
    }
}

public sealed record AppSettings
{
    public const int CurrentSchemaVersion = 2;
    public const string DefaultFallbackProbeUrl = "https://www.baidu.com/favicon.ico";

    public int SchemaVersion { get; init; } = CurrentSchemaVersion;
    public string Account { get; init; } = string.Empty;
    public Provider Provider { get; init; } = Provider.Campus;
    public string EncryptedPassword { get; init; } = string.Empty;
    public int CheckIntervalSeconds { get; init; } = 30;
    public int FailureCooldownSeconds { get; init; } = 300;
    public string FallbackProbeUrl { get; init; } = DefaultFallbackProbeUrl;
    public bool AutoStart { get; init; }
    public bool SuccessNotification { get; init; } = true;
    public DateTimeOffset? LastSuccessfulLoginUtc { get; init; }

    [JsonIgnore]
    public bool HasCredentials =>
        !string.IsNullOrWhiteSpace(Account) &&
        !string.IsNullOrWhiteSpace(EncryptedPassword);

    public AppSettings Normalize()
    {
        int normalizedCooldown =
            SchemaVersion < 2 && FailureCooldownSeconds == 0
                ? -1
                : Math.Clamp(FailureCooldownSeconds, -1, 3600);

        return this with
        {
            SchemaVersion = CurrentSchemaVersion,
            Account = (Account ?? string.Empty).Trim(),
            EncryptedPassword = EncryptedPassword ?? string.Empty,
            CheckIntervalSeconds = Math.Clamp(CheckIntervalSeconds, 10, 3600),
            FailureCooldownSeconds = normalizedCooldown,
            FallbackProbeUrl = IsValidFallbackUrl(FallbackProbeUrl)
                ? FallbackProbeUrl!.Trim()
                : DefaultFallbackProbeUrl
        };
    }

    public IReadOnlyList<string> Validate(bool requirePassword)
    {
        List<string> errors = [];

        if (string.IsNullOrWhiteSpace(Account))
        {
            errors.Add("请输入学号或账号。");
        }
        else if (Account.Contains('@'))
        {
            errors.Add("账号中不要填写运营商后缀，请通过服务商下拉框选择。");
        }

        if (requirePassword && string.IsNullOrWhiteSpace(EncryptedPassword))
        {
            errors.Add("首次保存时必须输入密码。");
        }

        if (CheckIntervalSeconds is < 10 or > 3600)
        {
            errors.Add("检测间隔必须在 10 到 3600 秒之间。");
        }

        if (FailureCooldownSeconds is < -1 or > 3600)
        {
            errors.Add("失败冷却时间必须在 -1 到 3600 秒之间。");
        }

        if (!IsValidFallbackUrl(FallbackProbeUrl))
        {
            errors.Add("国内备用探测地址必须是有效的 HTTPS 地址。");
        }

        return errors;
    }

    public static bool IsValidFallbackUrl(string? value) =>
        Uri.TryCreate(value?.Trim(), UriKind.Absolute, out Uri? uri) &&
        uri.Scheme.Equals(Uri.UriSchemeHttps, StringComparison.OrdinalIgnoreCase) &&
        !string.IsNullOrWhiteSpace(uri.Host) &&
        string.IsNullOrEmpty(uri.UserInfo);
}

public enum NetworkState
{
    NotConfigured,
    Checking,
    Online,
    Offline,
    Authenticating,
    LoggingOut,
    AuthServerUnavailable,
    LoginFailed,
    LogoutFailed,
    Paused
}

public sealed record NetworkStatusSnapshot(
    NetworkState State,
    string Message,
    DateTimeOffset UpdatedAtUtc)
{
    public static NetworkStatusSnapshot Initial { get; } =
        new(NetworkState.Checking, "等待首次检测", DateTimeOffset.UtcNow);

    public string DisplayName => State switch
    {
        NetworkState.NotConfigured => "未配置",
        NetworkState.Checking => "检测中",
        NetworkState.Online => "已连接",
        NetworkState.Offline => "网络不可用",
        NetworkState.Authenticating => "正在登录",
        NetworkState.LoggingOut => "正在注销",
        NetworkState.AuthServerUnavailable => "认证服务器不可达",
        NetworkState.LoginFailed => "登录失败",
        NetworkState.LogoutFailed => "注销失败",
        NetworkState.Paused => "已暂停",
        _ => State.ToString()
    };
}

public sealed record LoginAttemptResult(bool RequestCompleted, int? StatusCode, string? Error)
{
    public static LoginAttemptResult Failed(string error) => new(false, null, error);
}

public sealed record LogoutAttemptResult(
    bool Succeeded,
    int? StatusCode,
    string? Error)
{
    public static LogoutAttemptResult Success(int statusCode) =>
        new(true, statusCode, null);

    public static LogoutAttemptResult Failed(string error, int? statusCode = null) =>
        new(false, statusCode, error);
}

public enum NotificationKind
{
    Information,
    Success,
    Warning,
    Error
}

public sealed record AppNotification(NotificationKind Kind, string Title, string Message);
