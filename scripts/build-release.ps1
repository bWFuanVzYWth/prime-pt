param([switch]$Offline)

$ErrorActionPreference = 'Stop'
$project = Join-Path $PSScriptRoot '..'
$buildArgs = @('--console=plain', 'buildRelease')
if ($Offline) { $buildArgs += '--offline' }

Push-Location -LiteralPath $project
try {
    & .\gradlew.bat @buildArgs
    if ($LASTEXITCODE -ne 0) { throw "Prime PT 构建失败（退出码 $LASTEXITCODE）" }
} finally {
    Pop-Location
}
