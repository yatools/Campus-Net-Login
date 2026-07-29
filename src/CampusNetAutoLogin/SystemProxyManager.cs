using System.ComponentModel;
using System.Runtime.InteropServices;

namespace CampusNetAutoLogin;

public sealed record ProxyClearResult(
    bool Succeeded,
    bool SettingsChanged,
    int? NativeErrorCode,
    string Message);

public interface ISystemProxyManager
{
    Task<ProxyClearResult> ClearCurrentUserProxyAsync(
        CancellationToken cancellationToken = default);
}

public sealed class SystemProxyManager : ISystemProxyManager
{
    private const int InternetOptionRefresh = 37;
    private const int InternetOptionSettingsChanged = 39;
    private const int InternetOptionPerConnectionOption = 75;

    private const int InternetPerConnFlags = 1;
    private const int InternetPerConnProxyServer = 2;
    private const int InternetPerConnProxyBypass = 3;
    private const int InternetPerConnAutoConfigUrl = 4;
    private const int InternetPerConnAutoDiscoveryFlags = 5;

    private const int ProxyTypeDirect = 0x00000001;

    public Task<ProxyClearResult> ClearCurrentUserProxyAsync(
        CancellationToken cancellationToken = default)
    {
        cancellationToken.ThrowIfCancellationRequested();
        return Task.FromResult(ClearCurrentUserProxy());
    }

    private static ProxyClearResult ClearCurrentUserProxy()
    {
        IntPtr emptyProxyServer = IntPtr.Zero;
        IntPtr emptyProxyBypass = IntPtr.Zero;
        IntPtr emptyAutoConfigUrl = IntPtr.Zero;
        IntPtr optionsPointer = IntPtr.Zero;

        try
        {
            emptyProxyServer = Marshal.StringToHGlobalUni(string.Empty);
            emptyProxyBypass = Marshal.StringToHGlobalUni(string.Empty);
            emptyAutoConfigUrl = Marshal.StringToHGlobalUni(string.Empty);

            InternetPerConnectionOption[] options =
            [
                InternetPerConnectionOption.FromDword(
                    InternetPerConnFlags,
                    ProxyTypeDirect),
                InternetPerConnectionOption.FromString(
                    InternetPerConnProxyServer,
                    emptyProxyServer),
                InternetPerConnectionOption.FromString(
                    InternetPerConnProxyBypass,
                    emptyProxyBypass),
                InternetPerConnectionOption.FromString(
                    InternetPerConnAutoConfigUrl,
                    emptyAutoConfigUrl),
                InternetPerConnectionOption.FromDword(
                    InternetPerConnAutoDiscoveryFlags,
                    0)
            ];

            int optionSize = Marshal.SizeOf<InternetPerConnectionOption>();
            optionsPointer = Marshal.AllocHGlobal(optionSize * options.Length);
            for (int index = 0; index < options.Length; index++)
            {
                Marshal.StructureToPtr(
                    options[index],
                    IntPtr.Add(optionsPointer, optionSize * index),
                    fDeleteOld: false);
            }

            InternetPerConnectionOptionList optionList = new()
            {
                Size = Marshal.SizeOf<InternetPerConnectionOptionList>(),
                Connection = IntPtr.Zero,
                OptionCount = options.Length,
                OptionError = 0,
                Options = optionsPointer
            };

            if (!InternetSetOption(
                    IntPtr.Zero,
                    InternetOptionPerConnectionOption,
                    ref optionList,
                    optionList.Size))
            {
                return Failure("无法清除当前用户的系统代理设置。");
            }

            if (!InternetSetOption(
                    IntPtr.Zero,
                    InternetOptionSettingsChanged,
                    IntPtr.Zero,
                    0))
            {
                return Failure(
                    "代理已写入，但无法广播系统代理设置变化。",
                    settingsChanged: true);
            }

            if (!InternetSetOption(
                    IntPtr.Zero,
                    InternetOptionRefresh,
                    IntPtr.Zero,
                    0))
            {
                return Failure(
                    "代理已写入，但无法刷新系统代理设置。",
                    settingsChanged: true);
            }

            return new ProxyClearResult(
                Succeeded: true,
                SettingsChanged: true,
                NativeErrorCode: null,
                Message: "当前用户的手动代理、PAC 和自动检测代理已清除。");
        }
        finally
        {
            if (optionsPointer != IntPtr.Zero)
            {
                Marshal.FreeHGlobal(optionsPointer);
            }

            if (emptyProxyServer != IntPtr.Zero)
            {
                Marshal.FreeHGlobal(emptyProxyServer);
            }

            if (emptyProxyBypass != IntPtr.Zero)
            {
                Marshal.FreeHGlobal(emptyProxyBypass);
            }

            if (emptyAutoConfigUrl != IntPtr.Zero)
            {
                Marshal.FreeHGlobal(emptyAutoConfigUrl);
            }
        }
    }

    private static ProxyClearResult Failure(
        string prefix,
        bool settingsChanged = false)
    {
        int error = Marshal.GetLastWin32Error();
        return new ProxyClearResult(
            Succeeded: false,
            SettingsChanged: settingsChanged,
            NativeErrorCode: error,
            Message: $"{prefix}{Environment.NewLine}{new Win32Exception(error).Message}（错误 {error}）");
    }

    [DllImport(
        "wininet.dll",
        EntryPoint = "InternetSetOptionW",
        SetLastError = true)]
    [return: MarshalAs(UnmanagedType.Bool)]
    private static extern bool InternetSetOption(
        IntPtr internet,
        int option,
        ref InternetPerConnectionOptionList buffer,
        int bufferLength);

    [DllImport(
        "wininet.dll",
        EntryPoint = "InternetSetOptionW",
        SetLastError = true)]
    [return: MarshalAs(UnmanagedType.Bool)]
    private static extern bool InternetSetOption(
        IntPtr internet,
        int option,
        IntPtr buffer,
        int bufferLength);

    [StructLayout(LayoutKind.Sequential)]
    private struct InternetPerConnectionOptionList
    {
        public int Size;
        public IntPtr Connection;
        public int OptionCount;
        public int OptionError;
        public IntPtr Options;
    }

    [StructLayout(LayoutKind.Sequential)]
    private struct InternetPerConnectionOption
    {
        public int Option;
        public InternetPerConnectionOptionValue Value;

        public static InternetPerConnectionOption FromDword(
            int option,
            int value) =>
            new()
            {
                Option = option,
                Value = new InternetPerConnectionOptionValue
                {
                    DwordValue = value
                }
            };

        public static InternetPerConnectionOption FromString(
            int option,
            IntPtr value) =>
            new()
            {
                Option = option,
                Value = new InternetPerConnectionOptionValue
                {
                    StringValue = value
                }
            };
    }

    [StructLayout(LayoutKind.Explicit)]
    private struct InternetPerConnectionOptionValue
    {
        [FieldOffset(0)]
        public int DwordValue;

        [FieldOffset(0)]
        public IntPtr StringValue;
    }
}
