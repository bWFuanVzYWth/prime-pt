param(
    [string]$SdkRoot,
    [switch]$VerifyOnly,
    [string]$DeployTo
)

$ErrorActionPreference = 'Stop'
$engine = [IO.Path]::GetFullPath((Join-Path $PSScriptRoot '../../..'))
& (Join-Path $engine 'crates/vulkan/streamline/setup.ps1') @PSBoundParameters
