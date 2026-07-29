namespace CampusNetAutoLogin;

public static class AppPaths
{
    public const string AppName = "CampusNetAutoLogin";

    public static string DataDirectory =>
        Path.Combine(
            Environment.GetFolderPath(Environment.SpecialFolder.LocalApplicationData),
            AppName);

    public static string SettingsFile => Path.Combine(DataDirectory, "settings.json");

    public static string LogsDirectory => Path.Combine(DataDirectory, "logs");

    public static string BundleCacheRoot =>
        Path.Combine(Path.GetTempPath(), ".net", AppName);

    public static string ExecutablePath =>
        Environment.ProcessPath
        ?? throw new InvalidOperationException("无法确定当前程序路径。");
}

