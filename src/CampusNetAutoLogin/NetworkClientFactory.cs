namespace CampusNetAutoLogin;

public static class NetworkClientFactory
{
    public static HttpClient CreateDirectClient()
    {
        HttpClientHandler handler = CreateHandler();
        return new HttpClient(handler, disposeHandler: true)
        {
            Timeout = Timeout.InfiniteTimeSpan
        };
    }

    internal static HttpClientHandler CreateHandler() =>
        new()
        {
            AllowAutoRedirect = false,
            UseProxy = false,
            Proxy = null
        };
}
