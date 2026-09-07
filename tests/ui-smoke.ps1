param(
    [string]$Executable = (Join-Path $PSScriptRoot '..\publish\南湖校园网自动登陆.exe'),
    [string]$Screenshot
)

$ErrorActionPreference = 'Stop'
Set-StrictMode -Version Latest

$executablePath = (Resolve-Path -LiteralPath $Executable).Path
$testData = Join-Path ([IO.Path]::GetTempPath()) ('CampusNetAutoLogin-UiTest-' + [Guid]::NewGuid().ToString('N'))
New-Item -ItemType Directory -Path $testData | Out-Null

Add-Type -TypeDefinition @'
using System;
using System.Runtime.InteropServices;
using System.Text;

public static class CampusUiSmokeNative
{
    [StructLayout(LayoutKind.Sequential)]
    public struct NativeRect
    {
        public int Left;
        public int Top;
        public int Right;
        public int Bottom;
    }

    [StructLayout(LayoutKind.Sequential)]
    private struct MonitorInfo
    {
        public uint Size;
        public NativeRect Monitor;
        public NativeRect Work;
        public uint Flags;
    }

    private delegate bool EnumProc(IntPtr window, IntPtr state);

    [DllImport("user32.dll")]
    private static extern bool EnumWindows(EnumProc callback, IntPtr state);

    [DllImport("user32.dll")]
    private static extern uint GetWindowThreadProcessId(IntPtr window, out uint processId);

    [DllImport("user32.dll", CharSet = CharSet.Unicode)]
    private static extern int GetClassName(IntPtr window, StringBuilder text, int capacity);

    [DllImport("user32.dll", CharSet = CharSet.Unicode)]
    private static extern int GetWindowText(IntPtr window, StringBuilder text, int capacity);

    [DllImport("user32.dll")]
    public static extern IntPtr GetDlgItem(IntPtr dialog, int id);

    [DllImport("user32.dll")]
    public static extern IntPtr SendMessage(IntPtr window, uint message, UIntPtr wParam, IntPtr lParam);

    [DllImport("user32.dll", CharSet = CharSet.Unicode, EntryPoint = "SendMessageW")]
    private static extern IntPtr SendMessageText(IntPtr window, uint message, UIntPtr wParam, StringBuilder lParam);

    [DllImport("user32.dll", CharSet = CharSet.Unicode, EntryPoint = "SendMessageW")]
    private static extern IntPtr SendMessageSetText(IntPtr window, uint message, UIntPtr wParam, string lParam);

    [DllImport("user32.dll")]
    public static extern bool PostMessage(IntPtr window, uint message, UIntPtr wParam, IntPtr lParam);

    [DllImport("user32.dll")]
    private static extern bool GetWindowRect(IntPtr window, out NativeRect rectangle);

    [DllImport("user32.dll")]
    public static extern bool PrintWindow(IntPtr window, IntPtr deviceContext, uint flags);

    [DllImport("user32.dll")]
    public static extern uint GetDpiForWindow(IntPtr window);

    [DllImport("user32.dll")]
    public static extern uint GetGuiResources(IntPtr process, uint flags);

    [DllImport("user32.dll")]
    private static extern IntPtr MonitorFromWindow(IntPtr window, uint flags);

    [DllImport("user32.dll", CharSet = CharSet.Unicode)]
    private static extern bool GetMonitorInfo(IntPtr monitor, ref MonitorInfo information);

    public static IntPtr Find(uint processId, string className)
    {
        IntPtr found = IntPtr.Zero;
        EnumWindows(delegate(IntPtr window, IntPtr state)
        {
            uint candidateProcess;
            GetWindowThreadProcessId(window, out candidateProcess);
            if (candidateProcess == processId)
            {
                var text = new StringBuilder(256);
                GetClassName(window, text, text.Capacity);
                if (text.ToString() == className)
                {
                    found = window;
                    return false;
                }
            }
            return true;
        }, IntPtr.Zero);
        return found;
    }

    public static string WindowText(IntPtr window)
    {
        var text = new StringBuilder(512);
        GetWindowText(window, text, text.Capacity);
        return text.ToString();
    }

    public static string ItemText(IntPtr dialog, int id)
    {
        var text = new StringBuilder(2048);
        SendMessageText(GetDlgItem(dialog, id), 0x000D, (UIntPtr)text.Capacity, text);
        return text.ToString();
    }

    public static void SetItemText(IntPtr dialog, int id, string text)
    {
        SendMessageSetText(GetDlgItem(dialog, id), 0x000C, UIntPtr.Zero, text);
    }

    public static NativeRect Rectangle(IntPtr window)
    {
        NativeRect rectangle;
        if (!GetWindowRect(window, out rectangle))
        {
            throw new InvalidOperationException("无法读取窗口位置。");
        }
        return rectangle;
    }

    public static NativeRect WorkArea(IntPtr window)
    {
        IntPtr monitor = MonitorFromWindow(window, 2);
        var information = new MonitorInfo();
        information.Size = (uint)Marshal.SizeOf(typeof(MonitorInfo));
        if (monitor == IntPtr.Zero || !GetMonitorInfo(monitor, ref information))
        {
            throw new InvalidOperationException("无法读取显示器工作区。");
        }
        return information.Work;
    }
}
'@

$savedLocalAppData = $env:LOCALAPPDATA
$first = $null
$second = $null
try {
    $env:LOCALAPPDATA = $testData
    $workingDirectory = Split-Path -Parent $executablePath
    $firstWindowStyle = 'Hidden'
    $first = Start-Process -FilePath $executablePath -ArgumentList '--autostart' -WorkingDirectory $workingDirectory -WindowStyle $firstWindowStyle -PassThru
    Start-Sleep -Seconds 2
    $first.Refresh()
    if ($first.HasExited) {
        throw "首个实例提前退出：$($first.ExitCode)"
    }

    $second = Start-Process -FilePath $executablePath -WorkingDirectory $workingDirectory -WindowStyle Hidden -PassThru
    if (-not $second.WaitForExit(3000)) {
        throw '第二个实例未在 3 秒内退出。'
    }

    $dialog = [IntPtr]::Zero
    for ($index = 0; $index -lt 30 -and $dialog -eq [IntPtr]::Zero; $index++) {
        Start-Sleep -Milliseconds 100
        $dialog = [CampusUiSmokeNative]::Find([uint32]$first.Id, '#32770')
    }
    $mainWindow = [CampusUiSmokeNative]::Find([uint32]$first.Id, 'CampusNetAutoLogin.NativeWindow')
    if ($mainWindow -eq [IntPtr]::Zero) {
        throw '找不到托盘消息窗口。'
    }
    if ($dialog -eq [IntPtr]::Zero) {
        throw '第二次启动未唤起设置窗口。'
    }

    if ([CampusUiSmokeNative]::ItemText($dialog, 1004) -ne '1800' -or
        [CampusUiSmokeNative]::ItemText($dialog, 1005) -ne '20') {
        throw '默认检测间隔或失败冷却不正确。'
    }
    foreach ($controlId in @(1016, 1017, 1018)) {
        if ([CampusUiSmokeNative]::GetDlgItem($dialog, $controlId) -eq [IntPtr]::Zero) {
            throw "新增设置控件不存在：$controlId"
        }
    }
    if ([CampusUiSmokeNative]::ItemText($dialog, 1008) -notmatch '所有系统通知') {
        throw '通知开关未更新。'
    }

    $providerCombo = [CampusUiSmokeNative]::GetDlgItem($dialog, 1003)
    $providerCount = [CampusUiSmokeNative]::SendMessage(
        $providerCombo,
        0x0146,
        [UIntPtr]::Zero,
        [IntPtr]::Zero).ToInt64()

    [CampusUiSmokeNative]::SetItemText($dialog, 1001, 'TS25030092A31LD')
    [void][CampusUiSmokeNative]::SendMessage(
        $providerCombo,
        0x014E,
        [UIntPtr]3,
        [IntPtr]::Zero)
    [void][CampusUiSmokeNative]::PostMessage(
        $dialog,
        0x0111,
        [UIntPtr]((1 -shl 16) -bor 1003),
        $providerCombo)
    Start-Sleep -Milliseconds 100
    $submittedAccount = [CampusUiSmokeNative]::ItemText($dialog, 1012)
    $securityNotice = [CampusUiSmokeNative]::ItemText($dialog, 1015)
    $clearDataText = [CampusUiSmokeNative]::ItemText($dialog, 1011)
    $intervalSpin = [CampusUiSmokeNative]::GetDlgItem($dialog, 1013)
    $cooldownSpin = [CampusUiSmokeNative]::GetDlgItem($dialog, 1014)
    if ($submittedAccount -ne 'TS25030092A31LD@telecom') {
        $actualAccount = [CampusUiSmokeNative]::ItemText($dialog, 1001)
        $actualProvider = [CampusUiSmokeNative]::SendMessage(
            $providerCombo,
            0x0147,
            [UIntPtr]::Zero,
            [IntPtr]::Zero).ToInt64()
        throw "实际提交账号没有随账号和服务商更新：$submittedAccount；账号=$actualAccount；服务商索引=$actualProvider"
    }
    if ($securityNotice -notmatch 'Windows' -or $securityNotice -notmatch 'HTTP') {
        throw "安全说明缺失：$securityNotice"
    }
    if ($clearDataText -ne '⚠ 清除所有数据并退出') {
        throw "清除按钮标题不正确：$clearDataText"
    }
    if ($intervalSpin -eq [IntPtr]::Zero -or $cooldownSpin -eq [IntPtr]::Zero) {
        throw '数字调节控件未创建。'
    }

    $dialogRect = [CampusUiSmokeNative]::Rectangle($dialog)
    $workArea = [CampusUiSmokeNative]::WorkArea($dialog)
    $currentDpi = [CampusUiSmokeNative]::GetDpiForWindow($dialog)
    if ($dialogRect.Left -lt $workArea.Left -or $dialogRect.Top -lt $workArea.Top -or
        $dialogRect.Right -gt $workArea.Right -or $dialogRect.Bottom -gt $workArea.Bottom) {
        throw '设置窗口超出了当前显示器工作区。'
    }
    $logicalWidth = $dialogRect.Right - $dialogRect.Left
    $logicalHeight = $dialogRect.Bottom - $dialogRect.Top
    $displayProfiles = @(
        @{ Name = '1080p@100%'; Width = 1920; Height = 1030; Dpi = 96 },
        @{ Name = '1080p@125%'; Width = 1920; Height = 1010; Dpi = 120 },
        @{ Name = '1080p@150%'; Width = 1920; Height = 990; Dpi = 144 },
        @{ Name = '2K@150%'; Width = 2560; Height = 1350; Dpi = 144 },
        @{ Name = '4K@200%'; Width = 3840; Height = 2040; Dpi = 192 }
    )
    foreach ($profile in $displayProfiles) {
        $scaledWidth = [Math]::Ceiling($logicalWidth * $profile.Dpi / 96.0)
        $scaledHeight = [Math]::Ceiling($logicalHeight * $profile.Dpi / 96.0)
        if ($scaledWidth -gt $profile.Width -or $scaledHeight -gt $profile.Height) {
            throw "设置窗口无法适配 $($profile.Name)：${scaledWidth}x${scaledHeight}。"
        }
    }
    $clearRect = [CampusUiSmokeNative]::Rectangle([CampusUiSmokeNative]::GetDlgItem($dialog, 1011))
    $closeRect = [CampusUiSmokeNative]::Rectangle([CampusUiSmokeNative]::GetDlgItem($dialog, 2))
    $saveRect = [CampusUiSmokeNative]::Rectangle([CampusUiSmokeNative]::GetDlgItem($dialog, 1))
    if (-not ($clearRect.Left -lt $closeRect.Left -and $closeRect.Left -lt $saveRect.Left)) {
        throw '底部按钮没有按“清除、关闭、保存”从左到右排列。'
    }

    if ($Screenshot) {
        Add-Type -AssemblyName System.Drawing
        $screenshotPath = [IO.Path]::GetFullPath($Screenshot)
        $screenshotDirectory = [IO.Path]::GetDirectoryName($screenshotPath)
        if ($screenshotDirectory) {
            New-Item -ItemType Directory -Path $screenshotDirectory -Force | Out-Null
        }
        $dpi = [CampusUiSmokeNative]::GetDpiForWindow($dialog)
        if ($dpi -eq 0) { $dpi = 96 }
        $dpiScale = $dpi / 96.0
        $width = [int][Math]::Round(($dialogRect.Right - $dialogRect.Left) * $dpiScale)
        $height = [int][Math]::Round(($dialogRect.Bottom - $dialogRect.Top) * $dpiScale)
        $bitmap = New-Object Drawing.Bitmap $width, $height
        $graphics = [Drawing.Graphics]::FromImage($bitmap)
        try {
            $deviceContext = $graphics.GetHdc()
            try {
                if (-not [CampusUiSmokeNative]::PrintWindow($dialog, $deviceContext, 2)) {
                    throw 'Windows 无法渲染设置窗口预览。'
                }
            }
            finally {
                $graphics.ReleaseHdc($deviceContext)
            }
            $bitmap.Save($screenshotPath, [Drawing.Imaging.ImageFormat]::Png)
        }
        finally {
            $graphics.Dispose()
            $bitmap.Dispose()
        }
    }

    $first.Refresh()
    [pscustomobject]@{
        SecondInstanceExit = $second.ExitCode
        MainWindowFound = $true
        DialogTitle = [CampusUiSmokeNative]::WindowText($dialog)
        Account = [CampusUiSmokeNative]::ItemText($dialog, 1001)
        Interval = [CampusUiSmokeNative]::ItemText($dialog, 1004)
        Cooldown = [CampusUiSmokeNative]::ItemText($dialog, 1005)
        Fallback = [CampusUiSmokeNative]::ItemText($dialog, 1006)
        ProviderCount = $providerCount
        SubmittedAccount = $submittedAccount
        ClearDataButton = $clearDataText
        DialogSize = "$(($dialogRect.Right - $dialogRect.Left))x$(($dialogRect.Bottom - $dialogRect.Top))"
        CurrentDpi = $currentDpi
        WorkArea = "$(($workArea.Right - $workArea.Left))x$(($workArea.Bottom - $workArea.Top))"
        ValidatedProfiles = ($displayProfiles.Name -join ', ')
        DialogOpenWorkingSetMiB = [Math]::Round($first.WorkingSet64 / 1MB, 2)
        DialogOpenPrivateMiB = [Math]::Round($first.PrivateMemorySize64 / 1MB, 2)
        DialogOpenThreads = $first.Threads.Count
        DialogOpenHandles = $first.HandleCount
        DialogOpenGdiObjects = [CampusUiSmokeNative]::GetGuiResources($first.Handle, 0)
        DialogOpenUserObjects = [CampusUiSmokeNative]::GetGuiResources($first.Handle, 1)
        Screenshot = $Screenshot
    }

    [void][CampusUiSmokeNative]::PostMessage($dialog, 0x0111, [UIntPtr]2, [IntPtr]::Zero)
    Start-Sleep -Seconds 5
    $first.Refresh()
    [pscustomobject]@{
        AfterDialogWorkingSetMiB = [Math]::Round($first.WorkingSet64 / 1MB, 2)
        AfterDialogPrivateMiB = [Math]::Round($first.PrivateMemorySize64 / 1MB, 2)
        AfterDialogThreads = $first.Threads.Count
        AfterDialogHandles = $first.HandleCount
        AfterDialogGdiObjects = [CampusUiSmokeNative]::GetGuiResources($first.Handle, 0)
        AfterDialogUserObjects = [CampusUiSmokeNative]::GetGuiResources($first.Handle, 1)
    }
    [void][CampusUiSmokeNative]::PostMessage($mainWindow, 0x8003, [UIntPtr]::Zero, [IntPtr]::Zero)
    $reopenedDialog = [IntPtr]::Zero
    for ($index = 0; $index -lt 30 -and $reopenedDialog -eq [IntPtr]::Zero; $index++) {
        Start-Sleep -Milliseconds 100
        $reopenedDialog = [CampusUiSmokeNative]::Find([uint32]$first.Id, '#32770')
    }
    if ($reopenedDialog -eq [IntPtr]::Zero) {
        throw '设置窗口关闭后无法再次打开。'
    }
    [void][CampusUiSmokeNative]::PostMessage($mainWindow, 0x0010, [UIntPtr]::Zero, [IntPtr]::Zero)
    if (-not $first.WaitForExit(5000)) {
        throw '首个实例未在 5 秒内正常退出。'
    }
}
finally {
    $env:LOCALAPPDATA = $savedLocalAppData
    if ($second -and -not $second.HasExited) {
        Stop-Process -Id $second.Id -Force
    }
    if ($first -and -not $first.HasExited) {
        Stop-Process -Id $first.Id -Force
    }
    if (Test-Path -LiteralPath $testData) {
        $resolvedTestData = [IO.Path]::GetFullPath($testData)
        $resolvedTempRoot = [IO.Path]::GetFullPath([IO.Path]::GetTempPath()).TrimEnd('\') + '\'
        if ($resolvedTestData.StartsWith($resolvedTempRoot, [StringComparison]::OrdinalIgnoreCase) -and
            [IO.Path]::GetFileName($resolvedTestData).StartsWith('CampusNetAutoLogin-UiTest-', [StringComparison]::Ordinal)) {
            Remove-Item -LiteralPath $resolvedTestData -Recurse -Force
        }
    }
}
