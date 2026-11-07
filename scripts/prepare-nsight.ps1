[CmdletBinding()]
param(
    [ValidateSet('release', 'release-lto')]
    [string]$Profile = 'release',
    [switch]$Offline,
    [switch]$SkipBuild
)

$ErrorActionPreference = 'Stop'
Set-StrictMode -Version Latest
$project = [IO.Path]::GetFullPath((Join-Path $PSScriptRoot '..'))
$gradle = Join-Path $project 'gradlew.bat'
$taskTarget = if ($env:CARGO_TARGET_DIR) {
    [IO.Path]::GetFullPath($env:CARGO_TARGET_DIR, $project)
} else {
    Join-Path $project 'target'
}
$native = Join-Path $taskTarget "$Profile/prime_pt.dll"

if (!$SkipBuild) {
    $buildArgs = @('--console=plain', 'buildNative', "-PnativeProfile=$Profile")
    if ($Offline) { $buildArgs += '--offline' }
    Push-Location -LiteralPath $project
    try {
        & $gradle @buildArgs
        if ($LASTEXITCODE -ne 0) { throw "原生库构建失败（退出码 $LASTEXITCODE）" }
    } finally {
        Pop-Location
    }
}
if (!(Test-Path -LiteralPath $native -PathType Leaf)) {
    throw "找不到 $native；去掉 -SkipBuild 重新准备。"
}

$application = Join-Path $env:SystemRoot 'System32/cmd.exe'
$offlineArgument = if ($Offline) { '--offline ' } else { '' }
$arguments = '/d /s /c ""{0}" {1}--no-daemon runClient "-PnativeProfile={2}" "-PnativeLibrary={3}""' -f $gradle, $offlineArgument, $Profile, $native
$output = Join-Path $project 'target/nsight-launch'
New-Item -ItemType Directory -Path $output -Force | Out-Null
$values = [ordered]@{
    'Application Executable' = $application
    'Working Directory' = $project
    'Command Line Arguments' = $arguments
}
$files = @('application.txt', 'directory.txt', 'arguments.txt')
$index = 0
foreach ($item in $values.GetEnumerator()) {
    [IO.File]::WriteAllText((Join-Path $output $files[$index]), $item.Value)
    Write-Host "`n$($item.Key)："
    Write-Host $item.Value
    ++$index
}
[ordered]@{
    application = $application
    directory = $project
    arguments = $arguments
    profile = $Profile
    native = $native
    nativeSha256 = (Get-FileHash -LiteralPath $native -Algorithm SHA256).Hash
} | ConvertTo-Json | Set-Content -LiteralPath (Join-Path $output 'launch.json') -Encoding utf8
Write-Host "`n已准备，尚未启动游戏。三个文本文件可直接复制到Nsight对应输入框：$output"
