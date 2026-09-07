param(
    [string]$Executable = (Join-Path $PSScriptRoot '..\publish\南湖校园网自动登陆.exe'),
    [ValidateRange(1, 15)]
    [int]$ActiveMinutes = 10,
    [ValidateRange(1, 30)]
    [int]$ReleaseMinutes = 20,
    [ValidateRange(10, 300)]
    [int]$ProbeIntervalSeconds = 30
)

$ErrorActionPreference = 'Stop'
Set-StrictMode -Version Latest

Add-Type -AssemblyName System.Security.Cryptography.ProtectedData
Add-Type -TypeDefinition @'
using System;
using System.Runtime.InteropServices;
using System.Text;

public static class CampusHandleNative
{
    private delegate bool EnumProc(IntPtr window, IntPtr state);

    [DllImport("user32.dll")]
    private static extern bool EnumWindows(EnumProc callback, IntPtr state);

    [DllImport("user32.dll")]
    private static extern uint GetWindowThreadProcessId(IntPtr window, out uint processId);

    [DllImport("user32.dll", CharSet = CharSet.Unicode)]
    private static extern int GetClassName(IntPtr window, StringBuilder text, int capacity);

    [DllImport("user32.dll")]
    public static extern bool PostMessage(IntPtr window, uint message, UIntPtr wParam, IntPtr lParam);

    public static IntPtr Find(uint processId, string className)
    {
        IntPtr found = IntPtr.Zero;
        EnumWindows(delegate(IntPtr window, IntPtr state)
        {
            uint candidate;
            GetWindowThreadProcessId(window, out candidate);
            if (candidate == processId)
            {
                var text = new StringBuilder(128);
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
}
'@

function New-ResourceSample(
    [Diagnostics.Process]$Process,
    [double]$Second,
    [string]$Phase,
    [int]$ProbeCount
) {
    $Process.Refresh()
    if ($Process.HasExited) {
        throw '程序在句柄测试过程中退出。'
    }
    [pscustomobject]@{
        Second = $Second
        Phase = $Phase
        ProbeCount = $ProbeCount
        Handles = $Process.HandleCount
        PrivateMiB = $Process.PrivateMemorySize64 / 1MB
        WorkingSetMiB = $Process.WorkingSet64 / 1MB
        Threads = $Process.Threads.Count
        CpuMilliseconds = $Process.TotalProcessorTime.TotalMilliseconds
    }
}

function Find-NearestSample(
    [Collections.Generic.List[object]]$Samples,
    [double]$Second
) {
    $Samples |
        Sort-Object { [Math]::Abs($_.Second - $Second) } |
        Select-Object -First 1
}

$executablePath = (Resolve-Path -LiteralPath $Executable).Path
$temporaryRoot = [IO.Path]::GetTempPath()
$testData = Join-Path $temporaryRoot ('CampusNetAutoLogin-handles-' + [Guid]::NewGuid().ToString('N'))
$applicationData = Join-Path $testData 'CampusNetAutoLogin'
New-Item -ItemType Directory -Path $applicationData -Force | Out-Null

$plaintext = [Text.Encoding]::UTF8.GetBytes('benchmark-only-not-a-real-password')
$entropy = [Text.Encoding]::UTF8.GetBytes('CampusNetAutoLogin/v1/CurrentUser')
$encrypted = [Security.Cryptography.ProtectedData]::Protect(
    $plaintext,
    $entropy,
    [Security.Cryptography.DataProtectionScope]::CurrentUser)
$settings = [ordered]@{
    schemaVersion = 2
    account = 'HANDLE_BENCHMARK_ONLY'
    provider = 'Campus'
    encryptedPassword = [Convert]::ToBase64String($encrypted)
    checkIntervalSeconds = 3600
    failureCooldownSeconds = 300
    fallbackProbeUrl = 'https://www.baidu.com/favicon.ico'
    autoStart = $false
    successNotification = $false
    lastSuccessfulLoginUtc = $null
}
[IO.File]::WriteAllText(
    (Join-Path $applicationData 'settings.json'),
    ($settings | ConvertTo-Json -Compress),
    [Text.UTF8Encoding]::new($false))
[Array]::Clear($plaintext, 0, $plaintext.Length)
[Array]::Clear($encrypted, 0, $encrypted.Length)

$savedLocalAppData = $env:LOCALAPPDATA
$process = $null
try {
    $env:LOCALAPPDATA = $testData
    $process = Start-Process `
        -FilePath $executablePath `
        -ArgumentList '--autostart' `
        -WindowStyle Hidden `
        -PassThru
    $env:LOCALAPPDATA = $savedLocalAppData

    $mainWindow = [IntPtr]::Zero
    for ($index = 0; $index -lt 200 -and $mainWindow -eq [IntPtr]::Zero; $index++) {
        Start-Sleep -Milliseconds 20
        if ($process.HasExited) {
            throw '测试实例被另一个实例阻止，请先结束同一路径的遗留测试进程。'
        }
        $mainWindow = [CampusHandleNative]::Find(
            [uint32]$process.Id,
            'CampusNetAutoLogin.NativeWindow')
    }
    if ($mainWindow -eq [IntPtr]::Zero) {
        throw '未找到程序消息窗口。'
    }

    # Pause reconnect immediately. Manual detection remains available and cannot log in.
    [void][CampusHandleNative]::PostMessage(
        $mainWindow,
        0x0111,
        [UIntPtr]4004,
        [IntPtr]::Zero)
    Start-Sleep -Milliseconds 500

    $activeSeconds = $ActiveMinutes * 60.0
    $releaseSeconds = $ReleaseMinutes * 60.0
    $totalSeconds = $activeSeconds + $releaseSeconds
    $watch = [Diagnostics.Stopwatch]::StartNew()
    $nextProbe = 0.0
    $nextMinute = 60.0
    $probeCount = 0
    $samples = [Collections.Generic.List[object]]::new()
    $samples.Add((New-ResourceSample $process 0.0 'active' 0))

    while ($watch.Elapsed.TotalSeconds -lt $totalSeconds) {
        $elapsed = $watch.Elapsed.TotalSeconds
        while ($elapsed -lt $activeSeconds -and $elapsed -ge $nextProbe) {
            [void][CampusHandleNative]::PostMessage(
                $mainWindow,
                0x0111,
                [UIntPtr]4002,
                [IntPtr]::Zero)
            $probeCount++
            $nextProbe += $ProbeIntervalSeconds
        }

        Start-Sleep -Seconds 1
        $elapsed = $watch.Elapsed.TotalSeconds
        $phase = if ($elapsed -lt $activeSeconds) { 'active' } else { 'release' }
        $sample = New-ResourceSample $process $elapsed $phase $probeCount
        $samples.Add($sample)

        if ($elapsed -ge $nextMinute) {
            Write-Output ("PROGRESS {0:N0}m phase={1} probes={2} handles={3} private={4:N2}MiB ws={5:N2}MiB threads={6}" -f
                ($elapsed / 60.0),
                $phase,
                $probeCount,
                $sample.Handles,
                $sample.PrivateMiB,
                $sample.WorkingSetMiB,
                $sample.Threads)
            $nextMinute += 60.0
        }
    }

    $start = Find-NearestSample $samples 0
    $activeEnd = Find-NearestSample $samples $activeSeconds
    $release5 = Find-NearestSample $samples ($activeSeconds + [Math]::Min(300, $releaseSeconds))
    $release10 = Find-NearestSample $samples ($activeSeconds + [Math]::Min(600, $releaseSeconds))
    $releaseEnd = Find-NearestSample $samples $totalSeconds
    $maximumHandles = ($samples | Measure-Object Handles -Maximum).Maximum
    $minimumReleaseHandles = ($samples |
        Where-Object Phase -eq 'release' |
        Measure-Object Handles -Minimum).Minimum
    $maximumPrivateMiB = ($samples | Measure-Object PrivateMiB -Maximum).Maximum
    $maximumWorkingSetMiB = ($samples | Measure-Object WorkingSetMiB -Maximum).Maximum

    [pscustomobject]@{
        TotalMinutes = $ActiveMinutes + $ReleaseMinutes
        ActiveMinutes = $ActiveMinutes
        ReleaseMinutes = $ReleaseMinutes
        ManualProbeCount = $probeCount
        StartHandles = $start.Handles
        ActiveEndHandles = $activeEnd.Handles
        MaximumHandles = $maximumHandles
        Release5MinutesHandles = $release5.Handles
        Release10MinutesHandles = $release10.Handles
        ReleaseEndHandles = $releaseEnd.Handles
        MinimumReleaseHandles = $minimumReleaseHandles
        ActiveHandleChange = $activeEnd.Handles - $start.Handles
        ReleaseHandleChange = $releaseEnd.Handles - $activeEnd.Handles
        MaximumPrivateMiB = [Math]::Round($maximumPrivateMiB, 2)
        MaximumWorkingSetMiB = [Math]::Round($maximumWorkingSetMiB, 2)
        FinalPrivateMiB = [Math]::Round($releaseEnd.PrivateMiB, 2)
        FinalWorkingSetMiB = [Math]::Round($releaseEnd.WorkingSetMiB, 2)
        FinalThreads = $releaseEnd.Threads
        CpuTimeDeltaMilliseconds = [Math]::Round(
            $releaseEnd.CpuMilliseconds - $start.CpuMilliseconds,
            2)
    } | Format-List

    'MINUTE_SNAPSHOTS'
    $minuteSnapshots = [Collections.Generic.List[object]]::new()
    for ($minute = 0; $minute -le ($ActiveMinutes + $ReleaseMinutes); $minute++) {
        $snapshot = Find-NearestSample $samples ($minute * 60.0)
        $minuteSnapshots.Add([pscustomobject]@{
            Minute = $minute
            Phase = if ($minute -lt $ActiveMinutes) { 'active' } else { 'release' }
            Probes = $snapshot.ProbeCount
            Handles = $snapshot.Handles
            PrivateMiB = [Math]::Round($snapshot.PrivateMiB, 2)
            WorkingSetMiB = [Math]::Round($snapshot.WorkingSetMiB, 2)
            Threads = $snapshot.Threads
        })
    }
    $minuteSnapshots | Format-Table -AutoSize
}
finally {
    $env:LOCALAPPDATA = $savedLocalAppData
    if ($process -and -not $process.HasExited) {
        $mainWindow = [CampusHandleNative]::Find(
            [uint32]$process.Id,
            'CampusNetAutoLogin.NativeWindow')
        if ($mainWindow -ne [IntPtr]::Zero) {
            [void][CampusHandleNative]::PostMessage(
                $mainWindow,
                0x0010,
                [UIntPtr]::Zero,
                [IntPtr]::Zero)
            [void]$process.WaitForExit(5000)
        }
        if (-not $process.HasExited) {
            $process.Kill()
            $process.WaitForExit()
        }
    }

    $resolvedTemporaryRoot = [IO.Path]::GetFullPath($temporaryRoot)
    $resolvedTestData = [IO.Path]::GetFullPath($testData)
    if ($resolvedTestData.StartsWith(
            $resolvedTemporaryRoot,
            [StringComparison]::OrdinalIgnoreCase) -and
        (Split-Path -Leaf $resolvedTestData).StartsWith(
            'CampusNetAutoLogin-handles-',
            [StringComparison]::Ordinal)) {
        Remove-Item -LiteralPath $resolvedTestData -Recurse -Force
    }
}
