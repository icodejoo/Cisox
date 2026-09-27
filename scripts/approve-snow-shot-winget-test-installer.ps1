# This helper runs with Windows PowerShell's UI Automation assemblies in the disposable VM.
[CmdletBinding()]
param(
    [Parameter(Mandatory)][int]$WingetProcessId,
    [Parameter(Mandatory)][string]$InstallerPath,
    [Parameter(Mandatory)][ValidatePattern('^[A-Fa-f0-9]{64}$')][string]$InstallerSha256
)
$ErrorActionPreference = 'Stop'
if ($env:RUNNER_ENVIRONMENT -ne 'github-hosted' -or $env:RUNNER_OS -ne 'Windows') {
    throw 'Installer consent is restricted to the disposable GitHub-hosted test VM.'
}
$installer = [IO.Path]::GetFullPath($InstallerPath)
$cache = [IO.Path]::GetFullPath((Join-Path $env:TEMP 'WinGet')) + [IO.Path]::DirectorySeparatorChar
if (-not $installer.StartsWith($cache, [StringComparison]::OrdinalIgnoreCase) -or
    (Split-Path -Leaf $installer) -notmatch '^snow-shot-[0-9A-Za-z.-]+-windows-x64-offline\.exe$' -or
    (Get-FileHash -LiteralPath $installer -Algorithm SHA256).Hash -ine $InstallerSha256) {
    throw 'Only the exact hash-verified offline release in the WinGet cache may be approved.'
}
$process = Get-Process -Id $WingetProcessId
if ($process.ProcessName -ne 'winget' -or $process.MainWindowTitle -ne 'Window Dialog') {
    throw 'Expected the launch dialog belonging to the test WinGet process.'
}
Add-Type -AssemblyName UIAutomationClient
Add-Type -AssemblyName UIAutomationTypes
$window = [Windows.Automation.AutomationElement]::FromHandle($process.MainWindowHandle)
function Find-Control([string]$Name) {
    $condition = New-Object Windows.Automation.PropertyCondition(
        [Windows.Automation.AutomationElement]::NameProperty, $Name)
    return $window.FindFirst([Windows.Automation.TreeScope]::Descendants, $condition)
}
$moreInfo = Find-Control 'More info'
if (-not $moreInfo) { throw 'The dialog is not the expected SmartScreen reputation prompt.' }
$invoke = $moreInfo.GetCurrentPattern([Windows.Automation.InvokePattern]::Pattern)
$invoke.Invoke()
$deadline = [DateTime]::UtcNow.AddSeconds(10)
do {
    $run = Find-Control 'Run anyway'
    if ($run) { break }
    Start-Sleep -Milliseconds 100
} while ([DateTime]::UtcNow -lt $deadline)
if (-not $run) { throw 'SmartScreen did not offer per-file consent.' }
$names = @($window.FindAll([Windows.Automation.TreeScope]::Descendants,
    [Windows.Automation.Condition]::TrueCondition) | ForEach-Object { $_.Current.Name })
if (-not ($names -like "*$(Split-Path -Leaf $installer)*")) {
    throw 'The SmartScreen dialog does not identify the expected release installer.'
}
$run.GetCurrentPattern([Windows.Automation.InvokePattern]::Pattern).Invoke()
Write-Output "Approved this hash-verified release in the disposable VM: $(Split-Path -Leaf $installer)"
