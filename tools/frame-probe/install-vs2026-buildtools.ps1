# 安装 VS 2026 Build Tools（MSVC 14.51 + Windows SDK），供参照版 Snow Shot 构建使用。
# 用法：以【管理员身份】打开 PowerShell，执行：
#   powershell -ExecutionPolicy Bypass -File E:\workspaces\Cisox\tools\frame-probe\install-vs2026-buildtools.ps1
# 与已有的 VS2022 BuildTools 并存，不会覆盖它。

$ErrorActionPreference = 'Stop'

# 安装器下载地址（stable 通道，下载后会校验是否为 VS 18）
$BootstrapperUrl = 'https://aka.ms/vs/stable/vs_BuildTools.exe'
# 下载落地路径
$Bootstrapper = Join-Path $env:TEMP 'vs_BuildTools_2026.exe'
# 目标 MSVC 工具集前缀（仓库脚本写死要求 14.51）
$RequiredToolset = '14.51'

# 校验管理员权限
$principal = New-Object Security.Principal.WindowsPrincipal([Security.Principal.WindowsIdentity]::GetCurrent())
if (-not $principal.IsInRole([Security.Principal.WindowsBuiltInRole]::Administrator)) {
    Write-Host '请用管理员身份重新运行本脚本。' -ForegroundColor Red
    exit 1
}

Write-Host '[1/3] 下载安装器...'
Invoke-WebRequest -Uri $BootstrapperUrl -OutFile $Bootstrapper -UseBasicParsing
$ver = (Get-Item $Bootstrapper).VersionInfo.FileVersion
Write-Host "安装器版本：$ver"
if (-not $ver.StartsWith('18.')) {
    Write-Host "这不是 VS 2026（应为 18.x）。请把这行输出发给我，不要继续。" -ForegroundColor Red
    exit 2
}

Write-Host '[2/3] 安装 C++ 工作负载 + Windows SDK（体量约 8-10GB，需要几十分钟，请耐心等）...'
$installArgs = @(
    '--passive', '--norestart', '--wait',
    '--add', 'Microsoft.VisualStudio.Workload.VCTools',
    '--add', 'Microsoft.VisualStudio.Component.VC.Tools.x86.x64',
    '--add', 'Microsoft.VisualStudio.Component.Windows11SDK.26100',
    '--includeRecommended'
)
$proc = Start-Process -FilePath $Bootstrapper -ArgumentList $installArgs -Wait -PassThru
Write-Host "安装器退出码：$($proc.ExitCode)（0 成功，3010 成功但需重启）"

Write-Host '[3/3] 校验安装结果...'
$vswhere = Join-Path ${env:ProgramFiles(x86)} 'Microsoft Visual Studio\Installer\vswhere.exe'
$root = & $vswhere -latest -products * -version '[18.0,19.0)' `
    -requires Microsoft.VisualStudio.Component.VC.Tools.x86.x64 -property installationPath
if (-not $root) {
    Write-Host '未找到 VS 18 安装，请把上面的输出发给我。' -ForegroundColor Red
    exit 3
}
Write-Host "VS 2026 路径：$root"
$toolsets = Get-ChildItem (Join-Path $root 'VC\Tools\MSVC') -Directory | Select-Object -ExpandProperty Name
Write-Host "已装 MSVC 工具集：$($toolsets -join ', ')"
if (-not ($toolsets | Where-Object { $_ -like "$RequiredToolset*" })) {
    Write-Host "缺少 $RequiredToolset 工具集。请把上面的输出发给我，我再给出补装该组件的命令。" -ForegroundColor Yellow
    exit 4
}
Write-Host "完成：MSVC $RequiredToolset 已就绪。回来告诉我一声即可。" -ForegroundColor Green
