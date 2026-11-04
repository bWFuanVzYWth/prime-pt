param([switch]$Offline)

$ErrorActionPreference = 'Stop'
$project = Join-Path $PSScriptRoot '..'
$engine = [IO.Path]::GetFullPath((Join-Path $PSScriptRoot '../../..'))
if (!$env:SLANGC) {
    $tools = Get-Content -LiteralPath (Join-Path $engine 'scripts/tools.json') -Raw | ConvertFrom-Json
    $compiler = Join-Path $engine "target/tools/slang/$($tools.slang.version)/windows-x86_64/bin/slangc.exe"
    if (!(Test-Path -LiteralPath $compiler -PathType Leaf)) {
        if ($Offline) { throw '离线构建需要完整 Slang 发行包，请先运行引擎的 install_slang.ps1 或设置 SLANGC。' }
        & (Join-Path $engine 'scripts/install_slang.ps1')
    }
}
$buildArgs = @('--console=plain', 'buildRelease')
if ($Offline) { $buildArgs += '--offline' }

Push-Location -LiteralPath $project
try {
    & .\gradlew.bat @buildArgs
    if ($LASTEXITCODE -ne 0) { throw "Prime PT 构建失败（退出码 $LASTEXITCODE）" }
} finally {
    Pop-Location
}
