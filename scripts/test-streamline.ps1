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
if (!$SdkRoot) { $SdkRoot = $env:STREAMLINE_SDK }
if (!$SdkRoot) { $SdkRoot = Join-Path $repository 'target/streamline-sdk' }
& (Join-Path $repository 'scripts/test_streamline.ps1') -SdkRoot $SdkRoot -Manual:$Manual -InitializationOnly:$InitializationOnly -ReportOnlyValidation:$ReportOnlyValidation -StandardValidationOnly:$StandardValidationOnly -Output $Output
