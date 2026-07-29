using System.Drawing;
using System.Reflection;

namespace CampusNetAutoLogin;

internal enum TrayIconKind
{
    Blue,
    Red
}

internal sealed class AppIcons : IDisposable
{
    private const string BlueResourceName =
        "CampusNetAutoLogin.Assets.campus-blue.ico";
    private const string RedResourceName =
        "CampusNetAutoLogin.Assets.campus-red.ico";

    private AppIcons(Icon blue, Icon red)
    {
        Blue = blue;
        Red = red;
    }

    public Icon Blue { get; }

    public Icon Red { get; }

    public static AppIcons Load() =>
        new(LoadIcon(BlueResourceName), LoadIcon(RedResourceName));

    public static TrayIconKind ForState(NetworkState state) => state switch
    {
        NetworkState.Checking or NetworkState.Online => TrayIconKind.Blue,
        _ => TrayIconKind.Red
    };

    private static Icon LoadIcon(string resourceName)
    {
        Assembly assembly = typeof(AppIcons).Assembly;
        using Stream stream = assembly.GetManifestResourceStream(resourceName)
            ?? throw new InvalidOperationException($"找不到图标资源：{resourceName}");
        using Icon icon = new(stream);
        return (Icon)icon.Clone();
    }

    public void Dispose()
    {
        Blue.Dispose();
        Red.Dispose();
    }
}
