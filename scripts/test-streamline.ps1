param(
    [string]$SdkRoot,
    [switch]$Manual,
    [switch]$InitializationOnly,
    [switch]$ReportOnlyValidation,
    [switch]$StandardValidationOnly,
    [string]$Output
)

$ErrorActionPreference = 'Stop'
$repository = [IO.Path]::GetFullPath((Join-Path $PSScriptRoot '../../..'))
& (Join-Path $repository 'crates/vulkan/streamline/test_gpu.ps1') @PSBoundParameters
