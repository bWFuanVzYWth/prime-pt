[CmdletBinding()]
param(
    [ValidateSet('normal', 'checkpoints', 'stages')]
    [string]$Mode = 'checkpoints',
    [string]$NativeLibrary,
    [switch]$Offline,
    [switch]$PrepareOnly
)

$ErrorActionPreference = 'Stop'
Set-StrictMode -Version Latest
$project = [IO.Path]::GetFullPath((Join-Path $PSScriptRoot '..'))
$engine = [IO.Path]::GetFullPath((Join-Path $project '../..'))
$native = if ($NativeLibrary) {
    [IO.Path]::GetFullPath($NativeLibrary, $project)
} else {
    Join-Path $project 'target/release/prime_pt.dll'
}
if (!(Test-Path -LiteralPath $native -PathType Leaf)) {
    throw "找不到 $native；先运行 .\scripts\prepare-nsight.ps1 -Offline 构建诊断库。"
}
$started = Get-Date
$output = Join-Path $engine "temp/prime-gpu-$($started.ToString('yyyyMMdd-HHmmss-fff'))-$Mode"
New-Item -ItemType Directory -Path $output | Out-Null
$frozenNative = Join-Path $output 'prime_pt.dll'
$arguments = @('--no-daemon', '--console=plain', 'runClient', '-PnativeProfile=release', '-PwindowDiagnostic=true', "-PnativeLibrary=$frozenNative")
if ($Offline) { $arguments += '--offline' }
$sync = if ($Mode -eq 'stages') { '1' } else { '0' }
$checkpoints = if ($Mode -eq 'checkpoints') { '1' } else { '0' }
$source = foreach ($repo in @($engine, $project)) {
    $label = if ($repo -eq $engine) { 'engine' } else { 'prime' }
    $head = & git -C $repo rev-parse HEAD
    if ($LASTEXITCODE -ne 0) { throw "无法读取源码版本：$repo" }
    & git -C $repo diff HEAD --binary | Set-Content -LiteralPath (Join-Path $output "$label.diff") -Encoding utf8
    if ($LASTEXITCODE -ne 0) { throw "无法保存源码修改：$repo" }
    [ordered]@{ name = $label; path = $repo; head = $head }
}
Copy-Item -LiteralPath $native -Destination $frozenNative
$runtime = foreach ($name in @('sl.interposer.dll', 'sl.common.dll', 'sl.dlss_d.dll', 'nvngx_dlssd.dll')) {
    $file = Join-Path (Split-Path -Parent $native) $name
    if (!(Test-Path -LiteralPath $file -PathType Leaf)) { throw "缺少 RR 运行库：$file" }
    Copy-Item -LiteralPath $file -Destination (Join-Path $output $name)
    [ordered]@{ name = $name; sha256 = (Get-FileHash -LiteralPath (Join-Path $output $name) -Algorithm SHA256).Hash }
}
Copy-Item -LiteralPath $PSCommandPath -Destination (Join-Path $output 'launcher.ps1')
$settings = Join-Path $project 'run/config/prime_pt.properties'
if (Test-Path -LiteralPath $settings) {
    Copy-Item -LiteralPath $settings -Destination (Join-Path $output 'settings-before.properties')
}
[ordered]@{
    started = $started.ToString('o')
    mode = $Mode
    launchRequested = !$PrepareOnly
    native = $frozenNative
    nativeSource = $native
    nativeSha256 = (Get-FileHash -LiteralPath $frozenNative -Algorithm SHA256).Hash
    workingDirectory = $project
    arguments = $arguments
    environment = @{ PRIME_PT_DIAGNOSTIC_SYNC = $sync; PRIME_PT_DIAGNOSTIC_CHECKPOINTS = $checkpoints }
    source = $source
    runtime = $runtime
} | ConvertTo-Json -Depth 5 | Set-Content -LiteralPath (Join-Path $output 'launch.json') -Encoding utf8
Write-Host "诊断目录：$output"
if ($PrepareOnly) {
    Write-Host '已检查并保存启动输入，尚未启动游戏。去掉 -PrepareOnly 后运行。'
    return
}

$previous = [Environment]::GetEnvironmentVariable('PRIME_PT_DIAGNOSTIC_SYNC', 'Process')
$previousCheckpoints = [Environment]::GetEnvironmentVariable('PRIME_PT_DIAGNOSTIC_CHECKPOINTS', 'Process')
$code = 1
Push-Location -LiteralPath $project
try {
    $env:PRIME_PT_DIAGNOSTIC_SYNC = $sync
    $env:PRIME_PT_DIAGNOSTIC_CHECKPOINTS = $checkpoints
    Write-Host 'GPU 诊断写入 native.log；退出或崩溃后自动收集游戏日志。'
    & (Join-Path $project 'gradlew.bat') @arguments 2>&1 |
        Tee-Object -FilePath (Join-Path $output 'native.log') |
        Where-Object { $_ -notmatch '\[prime_pt\] gpu_(checkpoint|frame)' -or $_ -match 'failed:' }
    $code = $LASTEXITCODE
} finally {
    Pop-Location
    [Environment]::SetEnvironmentVariable('PRIME_PT_DIAGNOSTIC_SYNC', $previous, 'Process')
    [Environment]::SetEnvironmentVariable('PRIME_PT_DIAGNOSTIC_CHECKPOINTS', $previousCheckpoints, 'Process')
    foreach ($relative in @('run/logs/latest.log', 'run/logs/debug.log', 'run/config/prime_pt.properties')) {
        $file = Join-Path $project $relative
        if (Test-Path -LiteralPath $file) { Copy-Item -LiteralPath $file -Destination $output }
    }
    $crashes = Join-Path $project 'run/crash-reports'
    if (Test-Path -LiteralPath $crashes) {
        Get-ChildItem -LiteralPath $crashes -File |
            Where-Object { $_.LastWriteTime -ge $started } |
            ForEach-Object { Copy-Item -LiteralPath $_.FullName -Destination $output }
    }
    Write-Host "日志已保留：$output"
}
if ($code -ne 0) { throw "客户端退出码 $code；请保留上面的整个诊断目录。" }
