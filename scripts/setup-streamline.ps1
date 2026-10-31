param(
    [string]$SdkRoot,
    [switch]$VerifyOnly,
    [string]$DeployTo
)

$ErrorActionPreference = 'Stop'
$engine = [IO.Path]::GetFullPath((Join-Path $PSScriptRoot '../../..'))
if (!$SdkRoot) { $SdkRoot = $env:STREAMLINE_SDK }
if (!$SdkRoot) { $SdkRoot = Join-Path $engine 'target/streamline-sdk' }
& (Join-Path $engine 'scripts/install_streamline.ps1') -SdkRoot $SdkRoot -VerifyOnly:$VerifyOnly -DeployTo $DeployTo
$env:STREAMLINE_SDK = [IO.Path]::GetFullPath($SdkRoot)
