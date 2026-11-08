param(
    [Parameter(Mandatory = $true)][string]$WorldPath,
    [Parameter(Mandatory = $true)][string]$OutputDirectory,
    [ValidateRange(1, 100000)][int]$Frames = 600,
    [switch]$PrepareOnly
)
$ErrorActionPreference = 'Stop'
$project = [IO.Path]::GetFullPath((Join-Path $PSScriptRoot '..'))
$world = (Resolve-Path -LiteralPath $WorldPath).Path
if (-not (Test-Path -LiteralPath (Join-Path $world 'level.dat') -PathType Leaf)) { throw 'WorldPath 必须是包含 level.dat 的地图目录。' }
$output = [IO.Path]::GetFullPath($OutputDirectory)
if (Test-Path -LiteralPath $output) { throw '输出目录必须是尚不存在的新目录，避免覆盖已有现场。' }
$run = Join-Path $output 'run'
$saves = Join-Path $run 'saves'
New-Item -ItemType Directory -Path $saves -Force | Out-Null
$copy = [IO.Path]::GetFullPath((Join-Path $saves (Split-Path $world -Leaf)))
if (-not $copy.StartsWith($saves + [IO.Path]::DirectorySeparatorChar, [StringComparison]::OrdinalIgnoreCase)) { throw '地图副本越过诊断目录。' }
Copy-Item -LiteralPath $world -Destination $copy -Recurse
New-Item -ItemType Directory -Path (Join-Path $run 'config') | Out-Null
foreach ($relative in @('options.txt', 'config/prime_pt.properties')) {
    $source = Join-Path $project ('run/' + $relative)
    if (Test-Path -LiteralPath $source) { Copy-Item -LiteralPath $source -Destination (Join-Path $run $relative) }
}
$optionsPath = Join-Path $run 'options.txt'
if (Test-Path -LiteralPath $optionsPath) {
    $options = [IO.File]::ReadAllText($optionsPath) -replace '(?m)^fullscreen:.*$', 'fullscreen:false'
    [IO.File]::WriteAllText($optionsPath, $options)
}
$configPath = Join-Path $run 'config/prime_pt.properties'
if (Test-Path -LiteralPath $configPath) {
    $config = [IO.File]::ReadAllText($configPath) -replace '(?m)^quality=.*$', 'quality=dlaa'
    [IO.File]::WriteAllText($configPath, $config)
}
$capturePath = Join-Path $output 'capture'
$arguments = @('--no-daemon', '--console=plain', '--offline', 'runClient', '-PnativeProfile=release', '-PwindowDiagnostic=true', "-PdiagnosticRunDir=$run", "-PdiagnosticWorld=$(Split-Path $world -Leaf)")
[ordered]@{ source = $world; copy = $copy; run = $run; capture = $capturePath; frames = $Frames; arguments = $arguments } |
    ConvertTo-Json -Depth 4 | Set-Content -LiteralPath (Join-Path $output 'launch.json') -Encoding utf8
Write-Host "准备完成：$output"
if ($PrepareOnly) { return }
$oldCapture = $env:PRIME_PT_CAPTURE
$oldFrames = $env:PRIME_PT_CAPTURE_FRAMES
try {
    $env:PRIME_PT_CAPTURE = $capturePath
    $env:PRIME_PT_CAPTURE_FRAMES = "$Frames"
    Push-Location $project
    try { & .\gradlew.bat @arguments; $code = $LASTEXITCODE } finally { Pop-Location }
    if ($code -ne 0) { throw "客户端退出码 $code；请保留整个诊断目录。" }
} finally {
    $env:PRIME_PT_CAPTURE = $oldCapture
    $env:PRIME_PT_CAPTURE_FRAMES = $oldFrames
}
