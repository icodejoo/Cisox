#Requires -Version 7.0
# Provision the client version verified with manifest schema 1.12.0 on hosted runners.
$ErrorActionPreference = 'Stop'
$requiredVersion = [version]'1.29.380'
$command = Get-Command winget.exe -ErrorAction SilentlyContinue
$installedVersion = if ($command) { (& $command.Source --version).Trim().TrimStart('v') } else { '0.0' }
if ([version]$installedVersion -lt $requiredVersion) {
    Install-Module Microsoft.WinGet.Client -Scope CurrentUser -Force -Repository PSGallery
    # Without -Version, Repair can retain an older client bundled with the runner/module.
    Repair-WinGetPackageManager -Version $requiredVersion.ToString() -AllUsers | Out-Host
}
$package = Get-AppxPackage Microsoft.DesktopAppInstaller | Sort-Object Version -Descending | Select-Object -First 1
$executable = if ($package) { Join-Path $package.InstallLocation 'winget.exe' } else { $command.Source }
$actualVersion = (& $executable --version).Trim().TrimStart('v')
if ($LASTEXITCODE -ne 0 -or [version]$actualVersion -lt $requiredVersion) {
    throw "WinGet $requiredVersion or newer is required; found $actualVersion."
}
Write-Output $executable
