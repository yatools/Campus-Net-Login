using System.Net;
using System.Net.Sockets;
using System.Text;
using System.Text.Json;

namespace CampusNetAutoLogin;

public interface IConnectivityProbe
{
    Task<bool> IsInternetAvailableAsync(
        AppSettings settings,
        CancellationToken cancellationToken);

    Task<bool> IsAuthenticationServerReachableAsync(
        CancellationToken cancellationToken);
}

public sealed class ConnectivityProbe : IConnectivityProbe
{
    public static readonly Uri PrimaryProbeUri =
        new("http://www.msftconnecttest.com/connecttest.txt");

    public const string PrimaryExpectedBody = "Microsoft Connect Test";

    private readonly HttpClient _httpClient;

    public ConnectivityProbe(HttpClient httpClient)
    {
        _httpClient = httpClient;
    }

    public async Task<bool> IsInternetAvailableAsync(
        AppSettings settings,
        CancellationToken cancellationToken)
    {
        if (await ProbePrimaryAsync(cancellationToken).ConfigureAwait(false))
        {
            return true;
        }

        return await ProbeFallbackAsync(
            settings.FallbackProbeUrl,
            cancellationToken).ConfigureAwait(false);
    }

    private async Task<bool> ProbePrimaryAsync(CancellationToken cancellationToken)
    {
        using CancellationTokenSource timeout =
            CancellationTokenSource.CreateLinkedTokenSource(cancellationToken);
        timeout.CancelAfter(TimeSpan.FromSeconds(3));

        try
        {
            using HttpRequestMessage request = new(HttpMethod.Get, PrimaryProbeUri);
            using HttpResponseMessage response = await _httpClient.SendAsync(
                request,
                HttpCompletionOption.ResponseHeadersRead,
                timeout.Token).ConfigureAwait(false);

            if (response.StatusCode != HttpStatusCode.OK)
            {
                return false;
            }

            await using Stream stream =
                await response.Content.ReadAsStreamAsync(timeout.Token).ConfigureAwait(false);
            byte[] buffer = new byte[129];
            int total = 0;
            while (total < buffer.Length)
            {
                int read = await stream.ReadAsync(
                    buffer.AsMemory(total, buffer.Length - total),
                    timeout.Token).ConfigureAwait(false);
                if (read == 0)
                {
                    break;
                }

                total += read;
            }

            if (total > 128)
            {
                return false;
            }

            string body = Encoding.UTF8.GetString(buffer, 0, total).TrimStart('\uFEFF');
            return body.Equals(PrimaryExpectedBody, StringComparison.Ordinal);
        }
        catch (HttpRequestException)
        {
            return false;
        }
        catch (OperationCanceledException) when (!cancellationToken.IsCancellationRequested)
        {
            return false;
        }
        catch (IOException)
        {
            return false;
        }
    }

    private async Task<bool> ProbeFallbackAsync(
        string fallbackUrl,
        CancellationToken cancellationToken)
    {
        if (!AppSettings.IsValidFallbackUrl(fallbackUrl))
        {
            return false;
        }

        using CancellationTokenSource timeout =
            CancellationTokenSource.CreateLinkedTokenSource(cancellationToken);
        timeout.CancelAfter(TimeSpan.FromSeconds(3));

        try
        {
            using HttpRequestMessage request = new(HttpMethod.Get, fallbackUrl);
            using HttpResponseMessage response = await _httpClient.SendAsync(
                request,
                HttpCompletionOption.ResponseHeadersRead,
                timeout.Token).ConfigureAwait(false);
            return response.IsSuccessStatusCode;
        }
        catch (HttpRequestException)
        {
            return false;
        }
        catch (OperationCanceledException) when (!cancellationToken.IsCancellationRequested)
        {
            return false;
        }
    }

    public async Task<bool> IsAuthenticationServerReachableAsync(
        CancellationToken cancellationToken)
    {
        using CancellationTokenSource timeout =
            CancellationTokenSource.CreateLinkedTokenSource(cancellationToken);
        timeout.CancelAfter(TimeSpan.FromSeconds(2));

        try
        {
            using TcpClient client = new();
            await client.ConnectAsync("10.2.5.251", 801, timeout.Token).ConfigureAwait(false);
            return client.Connected;
        }
        catch (SocketException)
        {
            return false;
        }
        catch (OperationCanceledException) when (!cancellationToken.IsCancellationRequested)
        {
            return false;
        }
    }
}

public interface ICampusAuthenticator
{
    Task<LoginAttemptResult> LoginAsync(
        string submittedAccount,
        string password,
        CancellationToken cancellationToken);

    Task<LogoutAttemptResult> LogoutAsync(CancellationToken cancellationToken);
}

public sealed class CampusAuthenticator : ICampusAuthenticator
{
    public static readonly Uri PortalEndpoint =
        new("http://10.2.5.251:801/eportal/");

    private readonly HttpClient _httpClient;

    public CampusAuthenticator(HttpClient httpClient)
    {
        _httpClient = httpClient;
    }

    public async Task<LoginAttemptResult> LoginAsync(
        string submittedAccount,
        string password,
        CancellationToken cancellationToken)
    {
        ArgumentException.ThrowIfNullOrWhiteSpace(submittedAccount);
        ArgumentNullException.ThrowIfNull(password);

        Uri uri = BuildLoginUri(submittedAccount, password);
        return await SendAsync(uri, cancellationToken).ConfigureAwait(false);
    }

    public async Task<LogoutAttemptResult> LogoutAsync(
        CancellationToken cancellationToken)
    {
        using CancellationTokenSource timeout =
            CancellationTokenSource.CreateLinkedTokenSource(cancellationToken);
        timeout.CancelAfter(TimeSpan.FromSeconds(5));

        try
        {
            using HttpRequestMessage request =
                new(HttpMethod.Get, BuildLogoutUri());
            using HttpResponseMessage response = await _httpClient.SendAsync(
                request,
                HttpCompletionOption.ResponseHeadersRead,
                timeout.Token).ConfigureAwait(false);

            if (!response.IsSuccessStatusCode)
            {
                return LogoutAttemptResult.Failed(
                    $"注销请求返回 HTTP {(int)response.StatusCode}。",
                    (int)response.StatusCode);
            }

            string body = await ReadLimitedBodyAsync(
                response.Content,
                timeout.Token).ConfigureAwait(false);
            return TryParseLogoutSuccess(body)
                ? LogoutAttemptResult.Success((int)response.StatusCode)
                : LogoutAttemptResult.Failed(
                    "认证服务器未确认注销成功。",
                    (int)response.StatusCode);
        }
        catch (HttpRequestException)
        {
            return LogoutAttemptResult.Failed("无法连接认证服务器。");
        }
        catch (OperationCanceledException) when (!cancellationToken.IsCancellationRequested)
        {
            return LogoutAttemptResult.Failed("注销请求超时。");
        }
    }

    public static Uri BuildLoginUri(string submittedAccount, string password) =>
        BuildUri(
            ("c", "Portal"),
            ("a", "login"),
            ("login_method", "1"),
            ("user_account", submittedAccount),
            ("user_password", password));

    public static Uri BuildLogoutUri() =>
        BuildUri(
            ("c", "Portal"),
            ("a", "logout"));

    internal static bool TryParseLogoutSuccess(string responseBody)
    {
        if (string.IsNullOrWhiteSpace(responseBody))
        {
            return false;
        }

        string body = responseBody.Trim();
        int objectStart = body.IndexOf('{');
        int objectEnd = body.LastIndexOf('}');
        if (objectStart < 0 || objectEnd <= objectStart)
        {
            return false;
        }

        try
        {
            using JsonDocument document = JsonDocument.Parse(
                body[objectStart..(objectEnd + 1)]);
            if (!document.RootElement.TryGetProperty(
                    "result",
                    out JsonElement result))
            {
                return false;
            }

            return result.ValueKind switch
            {
                JsonValueKind.Number => result.TryGetInt32(out int value) && value == 1,
                JsonValueKind.String =>
                    result.GetString() is string text &&
                    (text.Equals("1", StringComparison.OrdinalIgnoreCase) ||
                     text.Equals("ok", StringComparison.OrdinalIgnoreCase)),
                _ => false
            };
        }
        catch (JsonException)
        {
            return false;
        }
    }

    private static Uri BuildUri(params (string Key, string Value)[] parameters)
    {
        string query = string.Join(
            "&",
            parameters.Select(pair =>
                $"{Uri.EscapeDataString(pair.Key)}={Uri.EscapeDataString(pair.Value)}"));
        UriBuilder builder = new(PortalEndpoint)
        {
            Query = query
        };
        return builder.Uri;
    }

    private async Task<LoginAttemptResult> SendAsync(
        Uri uri,
        CancellationToken cancellationToken)
    {
        using CancellationTokenSource timeout =
            CancellationTokenSource.CreateLinkedTokenSource(cancellationToken);
        timeout.CancelAfter(TimeSpan.FromSeconds(5));

        try
        {
            using HttpRequestMessage request = new(HttpMethod.Get, uri);
            using HttpResponseMessage response = await _httpClient.SendAsync(
                request,
                HttpCompletionOption.ResponseHeadersRead,
                timeout.Token).ConfigureAwait(false);
            return new LoginAttemptResult(
                RequestCompleted: true,
                StatusCode: (int)response.StatusCode,
                Error: null);
        }
        catch (HttpRequestException)
        {
            return LoginAttemptResult.Failed("无法连接认证服务器。");
        }
        catch (OperationCanceledException) when (!cancellationToken.IsCancellationRequested)
        {
            return LoginAttemptResult.Failed("登录请求超时。");
        }
    }

    private static async Task<string> ReadLimitedBodyAsync(
        HttpContent content,
        CancellationToken cancellationToken)
    {
        const int maximumBytes = 4096;
        await using Stream stream =
            await content.ReadAsStreamAsync(cancellationToken).ConfigureAwait(false);
        byte[] buffer = new byte[maximumBytes + 1];
        int total = 0;

        while (total < buffer.Length)
        {
            int read = await stream.ReadAsync(
                buffer.AsMemory(total, buffer.Length - total),
                cancellationToken).ConfigureAwait(false);
            if (read == 0)
            {
                break;
            }

            total += read;
        }

        if (total > maximumBytes)
        {
            return string.Empty;
        }

        return Encoding.UTF8.GetString(buffer, 0, total);
    }
}

public interface IAsyncDelay
{
    Task DelayAsync(TimeSpan delay, CancellationToken cancellationToken);
}

public sealed class SystemAsyncDelay : IAsyncDelay
{
    public Task DelayAsync(TimeSpan delay, CancellationToken cancellationToken) =>
        Task.Delay(delay, cancellationToken);
}
