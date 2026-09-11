$ErrorActionPreference = 'Stop'
Set-StrictMode -Version Latest

$workspaceRoot = $PSScriptRoot
$temporaryRoot = [IO.Path]::GetTempPath()
if ($temporaryRoot -match '[^\x21-\x7e]') {
    try {
        $fileSystem = New-Object -ComObject Scripting.FileSystemObject
        $temporaryRoot = $fileSystem.GetFolder($temporaryRoot).ShortPath
    }
    catch {
        $temporaryRoot = $env:PUBLIC
    }
}
if ($temporaryRoot -match '[^\x21-\x7e]') {
    $temporaryRoot = $env:PUBLIC
}
$temporaryTarget = Join-Path $temporaryRoot 'CampusNetAutoLogin-target'
$publishDirectory = Join-Path $workspaceRoot 'publish'
$sourceExecutable = Join-Path $temporaryTarget 'x86_64-pc-windows-gnu\release\campus-net-auto-login.exe'
$publishedExecutable = Join-Path $publishDirectory '南湖校园网自动登陆.exe'

$cargoCommand = Get-Command cargo -ErrorAction Stop
$previousTargetDirectory = $env:CARGO_TARGET_DIR

Push-Location $workspaceRoot
try {
    $env:CARGO_TARGET_DIR = $temporaryTarget
    & $cargoCommand.Source test --locked --all-targets
    if ($LASTEXITCODE -ne 0) {
        throw '自动化测试失败。'
    }

    & $cargoCommand.Source build --release --locked
    if ($LASTEXITCODE -ne 0) {
        throw 'Release 构建失败。'
    }

    $size = (Get-Item -LiteralPath $sourceExecutable).Length
    if ($size -gt 5MB) {
        throw "发布文件超过 5 MiB：$([Math]::Round($size / 1MB, 2)) MiB。"
    }

    New-Item -ItemType Directory -Path $publishDirectory -Force | Out-Null
    Copy-Item -LiteralPath $sourceExecutable -Destination $publishedExecutable -Force

    Write-Host "已生成：$publishedExecutable"
    Write-Host "文件大小：$([Math]::Round($size / 1KB, 1)) KiB"
}
finally {
    $env:CARGO_TARGET_DIR = $previousTargetDirectory
    Pop-Location
}
