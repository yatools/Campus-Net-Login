param(
    [string]$Executable = (Join-Path $PSScriptRoot '..\publish\南湖校园网自动登陆.exe'),
    [ValidateRange(1, 30)]
    [int]$DurationMinutes = 10,
    [ValidateRange(50, 1000)]
    [int]$SampleMilliseconds = 100,
    [ValidateRange(10, 3600)]
    [int]$CheckIntervalSeconds = 30
)

$ErrorActionPreference = 'Stop'
Set-StrictMode -Version Latest

Add-Type -AssemblyName System.Security.Cryptography.ProtectedData
Add-Type -TypeDefinition @'
using System;
using System.Runtime.InteropServices;
using System.Text;

public static class CampusCpuNative
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

    [DllImport("kernel32.dll")]
    public static extern bool QueryProcessCycleTime(IntPtr process, out ulong cycles);

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

function Read-Cycles([Diagnostics.Process]$Process) {
    [uint64]$cycles = 0
    if (-not [CampusCpuNative]::QueryProcessCycleTime($Process.Handle, [ref]$cycles)) {
        throw '无法读取进程 CPU 周期。'
    }
    return $cycles
}

# Calibrate cycle counts against Windows process CPU time so sub-tick work remains visible.
$self = [Diagnostics.Process]::GetCurrentProcess()
$warm = 0.0
for ($index = 0; $index -lt 20000; $index++) {
    $warm += [Math]::Sqrt($index + 1)
}
$self.Refresh()
$calibrationCpuStart = $self.TotalProcessorTime.TotalMilliseconds
$calibrationCycleStart = Read-Cycles $self
$calibrationWatch = [Diagnostics.Stopwatch]::StartNew()
while ($calibrationWatch.Elapsed.TotalSeconds -lt 1.5) {
    for ($index = 1; $index -le 2000; $index++) {
        $warm += [Math]::Sqrt($index)
    }
}
$self.Refresh()
$calibrationCpuMilliseconds = $self.TotalProcessorTime.TotalMilliseconds - $calibrationCpuStart
$calibrationCycles = (Read-Cycles $self) - $calibrationCycleStart
$cyclesPerCpuSecond = if ($calibrationCpuMilliseconds -gt 0) {
    $calibrationCycles / ($calibrationCpuMilliseconds / 1000.0)
}
else {
    0
}

$executablePath = (Resolve-Path -LiteralPath $Executable).Path
$temporaryRoot = [IO.Path]::GetTempPath()
$testData = Join-Path $temporaryRoot ('CampusNetAutoLogin-cpu-' + [Guid]::NewGuid().ToString('N'))
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
    account = 'CPU_BENCHMARK_ONLY'
    provider = 'Campus'
    encryptedPassword = [Convert]::ToBase64String($encrypted)
    checkIntervalSeconds = $CheckIntervalSeconds
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
        $mainWindow = [CampusCpuNative]::Find(
            [uint32]$process.Id,
            'CampusNetAutoLogin.NativeWindow')
    }
    if ($mainWindow -eq [IntPtr]::Zero) {
        throw '未找到程序消息窗口。'
    }

    # Pause reconnect before the first failed double-probe could ever reach login.
    # Periodic public-network detection remains active while paused.
    [void][CampusCpuNative]::PostMessage(
        $mainWindow,
        0x0111,
        [UIntPtr]4004,
        [IntPtr]::Zero)
    Start-Sleep -Milliseconds 500

    $process.Refresh()
    if ($process.HasExited) {
        throw '程序在 CPU 采样开始前退出。'
    }

    $logicalProcessors = [Environment]::ProcessorCount
    $durationSeconds = $DurationMinutes * 60.0
    $watch = [Diagnostics.Stopwatch]::StartNew()
    $startCpuMilliseconds = $process.TotalProcessorTime.TotalMilliseconds
    $startCycles = Read-Cycles $process
    $lastCpuMilliseconds = $startCpuMilliseconds
    $lastCycles = $startCycles
    $lastElapsedMilliseconds = 0.0
    $maximum100MillisecondsOneCore = 0.0
    $maximum100MillisecondsMachine = 0.0
    $maximum100MillisecondsAt = 0.0
    $maximumOneSecondOneCore = 0.0
    $maximumOneSecondMachine = 0.0
    $maximumOneSecondAt = 0.0
    $binStartMilliseconds = 0.0
    $binCycles = [uint64]0
    $activeSamples = 0
    $samples = [Collections.Generic.List[object]]::new()
    $nextProgress = 60.0

    while ($watch.Elapsed.TotalSeconds -lt $durationSeconds) {
        Start-Sleep -Milliseconds $SampleMilliseconds
        $process.Refresh()
        if ($process.HasExited) {
            throw '程序在连续 CPU 采样期间退出。'
        }

        $elapsedMilliseconds = $watch.Elapsed.TotalMilliseconds
        $cpuMilliseconds = $process.TotalProcessorTime.TotalMilliseconds
        $cycles = Read-Cycles $process
        $wallDeltaMilliseconds = $elapsedMilliseconds - $lastElapsedMilliseconds
        $cycleDelta = $cycles - $lastCycles
        $estimatedCpuMilliseconds = if ($cyclesPerCpuSecond -gt 0) {
            ($cycleDelta / $cyclesPerCpuSecond) * 1000.0
        }
        else {
            0.0
        }
        $oneCorePercent = if ($wallDeltaMilliseconds -gt 0) {
            100.0 * $estimatedCpuMilliseconds / $wallDeltaMilliseconds
        }
        else {
            0.0
        }
        $machinePercent = $oneCorePercent / $logicalProcessors

        if ($cycleDelta -gt 0) {
            $activeSamples++
        }
        if ($oneCorePercent -gt $maximum100MillisecondsOneCore) {
            $maximum100MillisecondsOneCore = $oneCorePercent
            $maximum100MillisecondsMachine = $machinePercent
            $maximum100MillisecondsAt = $elapsedMilliseconds / 1000.0
        }

        $samples.Add([pscustomobject]@{
            Second = $elapsedMilliseconds / 1000.0
            Cycles = $cycleDelta
            EstimatedCpuMilliseconds = $estimatedCpuMilliseconds
            OneCorePercent = $oneCorePercent
            MachinePercent = $machinePercent
        })

        $binCycles += $cycleDelta
        if (($elapsedMilliseconds - $binStartMilliseconds) -ge 1000.0) {
            $binWallMilliseconds = $elapsedMilliseconds - $binStartMilliseconds
            $binEstimatedCpuMilliseconds = if ($cyclesPerCpuSecond -gt 0) {
                ($binCycles / $cyclesPerCpuSecond) * 1000.0
            }
            else {
                0.0
            }
            $binOneCore = 100.0 * $binEstimatedCpuMilliseconds / $binWallMilliseconds
            if ($binOneCore -gt $maximumOneSecondOneCore) {
                $maximumOneSecondOneCore = $binOneCore
                $maximumOneSecondMachine = $binOneCore / $logicalProcessors
                $maximumOneSecondAt = $elapsedMilliseconds / 1000.0
            }
            $binStartMilliseconds = $elapsedMilliseconds
            $binCycles = 0
        }

        if ($watch.Elapsed.TotalSeconds -ge $nextProgress) {
            $currentCycles = $cycles - $startCycles
            $currentEstimate = if ($cyclesPerCpuSecond -gt 0) {
                ($currentCycles / $cyclesPerCpuSecond) * 1000.0
            }
            else {
                0.0
            }
            Write-Output ("PROGRESS {0:N0}s cpu={1:N2}ms cycleEstimate={2:N2}ms peak100ms={3:N3}% one-core" -f
                $watch.Elapsed.TotalSeconds,
                ($cpuMilliseconds - $startCpuMilliseconds),
                $currentEstimate,
                $maximum100MillisecondsOneCore)
            $nextProgress += 60.0
        }

        $lastElapsedMilliseconds = $elapsedMilliseconds
        $lastCpuMilliseconds = $cpuMilliseconds
        $lastCycles = $cycles
    }

    $process.Refresh()
    $endCpuMilliseconds = $process.TotalProcessorTime.TotalMilliseconds
    $endCycles = Read-Cycles $process
    $actualDuration = $watch.Elapsed.TotalSeconds
    $totalCpuMilliseconds = $endCpuMilliseconds - $startCpuMilliseconds
    $totalCycleEstimateMilliseconds = if ($cyclesPerCpuSecond -gt 0) {
        (($endCycles - $startCycles) / $cyclesPerCpuSecond) * 1000.0
    }
    else {
        0.0
    }
    $averageOneCore = 100.0 * $totalCycleEstimateMilliseconds / ($actualDuration * 1000.0)
    $averageMachine = $averageOneCore / $logicalProcessors

    $topSamples = $samples |
        Sort-Object OneCorePercent -Descending |
        Select-Object -First 12 `
            Second,
            EstimatedCpuMilliseconds,
            OneCorePercent,
            MachinePercent

    $periodPeaks = [Collections.Generic.List[object]]::new()
    for (
        $period = $CheckIntervalSeconds;
        $period -lt $durationSeconds;
        $period += $CheckIntervalSeconds
    ) {
        $peak = $samples |
            Where-Object {
                $_.Second -ge ($period - 2) -and
                $_.Second -le ($period + 5)
            } |
            Sort-Object OneCorePercent -Descending |
            Select-Object -First 1
        if ($null -ne $peak) {
            $periodPeaks.Add([pscustomobject]@{
                ExpectedSecond = $period
                PeakAtSecond = [math]::Round($peak.Second, 3)
                PeakOneCorePercent = [math]::Round($peak.OneCorePercent, 4)
                PeakMachinePercent = [math]::Round($peak.MachinePercent, 4)
                EstimatedCpuMilliseconds = [math]::Round(
                    $peak.EstimatedCpuMilliseconds,
                    4)
            })
        }
    }

    [pscustomobject]@{
        DurationSeconds = [math]::Round($actualDuration, 2)
        CheckIntervalSeconds = $CheckIntervalSeconds
        ExpectedPeriodicChecks = [math]::Floor($durationSeconds / $CheckIntervalSeconds)
        LogicalProcessors = $logicalProcessors
        WindowsCpuTimeDeltaMilliseconds = [math]::Round($totalCpuMilliseconds, 2)
        CycleEstimatedCpuTimeDeltaMilliseconds = [math]::Round(
            $totalCycleEstimateMilliseconds,
            2)
        AverageOneCorePercent = [math]::Round($averageOneCore, 5)
        AverageMachinePercent = [math]::Round($averageMachine, 5)
        Peak100MillisecondsAtSecond = [math]::Round($maximum100MillisecondsAt, 3)
        Peak100MillisecondsOneCorePercent = [math]::Round(
            $maximum100MillisecondsOneCore,
            4)
        Peak100MillisecondsMachinePercent = [math]::Round(
            $maximum100MillisecondsMachine,
            4)
        PeakOneSecondAtSecond = [math]::Round($maximumOneSecondAt, 3)
        PeakOneSecondOneCorePercent = [math]::Round($maximumOneSecondOneCore, 4)
        PeakOneSecondMachinePercent = [math]::Round($maximumOneSecondMachine, 4)
        Active100MillisecondsSamples = $activeSamples
        FinalWorkingSetMiB = [math]::Round($process.WorkingSet64 / 1MB, 2)
        FinalPrivateMiB = [math]::Round($process.PrivateMemorySize64 / 1MB, 2)
        FinalThreads = $process.Threads.Count
        FinalHandles = $process.HandleCount
        CalibrationCyclesPerCpuSecond = [math]::Round($cyclesPerCpuSecond, 0)
    } | Format-List

    'TOP_100MS_SAMPLES'
    $topSamples | Format-Table -AutoSize
    'PERIOD_WINDOW_PEAKS'
    $periodPeaks | Format-Table -AutoSize
}
finally {
    $env:LOCALAPPDATA = $savedLocalAppData
    if ($process -and -not $process.HasExited) {
        $mainWindow = [CampusCpuNative]::Find(
            [uint32]$process.Id,
            'CampusNetAutoLogin.NativeWindow')
        if ($mainWindow -ne [IntPtr]::Zero) {
            [void][CampusCpuNative]::PostMessage(
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
            'CampusNetAutoLogin-cpu-',
            [StringComparison]::Ordinal)) {
        Remove-Item -LiteralPath $resolvedTestData -Recurse -Force
    }
}
