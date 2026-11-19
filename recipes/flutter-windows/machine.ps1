# Yonder recipe: Flutter for Windows (with Shorebird), machine part.
#
# Runs with administrator rights. From your Mac, once the agent is installed and
# the machine's owner allowed it (`yonder-agent allow <device> admin` on the PC):
#
#   yonder prep win
#
# Or on the PC itself, in an Administrator PowerShell, from nothing:
#
#   Set-ExecutionPolicy Bypass -Scope Process -Force
#   .\recipes\flutter-windows\machine.ps1
#
# Safe to run again: anything already there is skipped. It installs what every
# account needs (so jobs running as yonder-build see it) and sets the PC up to
# build while you're away:
#
#   - Chocolatey, then Git, fvm, NuGet, Inno Setup 6
#   - Visual Studio 2022 Build Tools with "Desktop development with C++"
#     (MSVC, CMake, the Windows SDK), unless a Visual Studio with C++ is there
#   - Developer Mode (fvm links its Flutter SDK into the project), long paths
#   - PowerShell may run local scripts (scripts\release_windows.ps1)
#   - No sleep or hibernate on mains power, so the PC stays reachable
#   - Defender skips Yonder's workspaces and the build account's tool caches
#
# Then, from your Mac in the project:  yonder setup win
# (installs the project's Flutter and Shorebird for the build account).
param(
  # Leave Visual Studio alone (you'll install it yourself).
  [switch]$NoVisualStudio,
  # Don't install Inno Setup (projects without an installer).
  [switch]$NoInnoSetup,
  # Keep Windows' sleep settings.
  [switch]$KeepSleep,
  # Don't add Defender exclusions (builds are slower with scanning).
  [switch]$NoDefenderExclusions
)
$ErrorActionPreference = 'Stop'

function Step($m) { Write-Host "`n> $m" -ForegroundColor Cyan }
function Ok($m) { Write-Host "  OK $m" -ForegroundColor Green }
function Note($m) { Write-Host "  - $m" }
function Fail($m) { Write-Host "`nX $m`n" -ForegroundColor Red; exit 1 }

$principal = New-Object Security.Principal.WindowsPrincipal([Security.Principal.WindowsIdentity]::GetCurrent())
if (-not $principal.IsInRole([Security.Principal.WindowsBuiltInRole]::Administrator)) {
  Fail 'Run this in an Administrator PowerShell (right-click PowerShell > Run as administrator).'
}
if (-not [Environment]::Is64BitOperatingSystem) { Fail 'Flutter for Windows needs 64-bit Windows.' }

# Makes tools installed in this run visible without opening a new window.
function Refresh-Path {
  $machine = [Environment]::GetEnvironmentVariable('Path', 'Machine')
  $user = [Environment]::GetEnvironmentVariable('Path', 'User')
  $env:Path = "$machine;$user"
}
function Has($command) { [bool](Get-Command $command -ErrorAction SilentlyContinue) }
# Installed for every account: not from a user's own folder (the build account can't
# see tools installed only for you).
function Shared($command) {
  $found = Get-Command $command -ErrorAction SilentlyContinue | Select-Object -First 1
  $users = Join-Path $env:SystemDrive 'Users'
  [bool]($found -and $found.Source -and -not $found.Source.StartsWith($users, [StringComparison]::OrdinalIgnoreCase))
}
function Set-Reg($path, $name, $value) {
  if (-not (Test-Path $path)) { New-Item -Path $path -Force | Out-Null }
  New-ItemProperty -Path $path -Name $name -Value $value -PropertyType DWord -Force | Out-Null
}

Step 'Chocolatey (installs the tools below for every account)'
if (Has choco) {
  Ok "already installed ($(choco --version))"
} else {
  [Net.ServicePointManager]::SecurityProtocol = [Net.ServicePointManager]::SecurityProtocol -bor 3072
  Invoke-Expression ((New-Object Net.WebClient).DownloadString('https://community.chocolatey.org/install.ps1'))
  Refresh-Path
  if (-not (Has choco)) { Fail 'Chocolatey did not install; see the output above.' }
  Ok 'installed'
}

function Choco-Install($package, $command, $label, [string[]]$extra = @()) {
  if ($command -and (Shared $command)) { Ok "$label already installed"; return }
  if ($command -and (Has $command)) { Note "$label is only installed for $env:USERNAME; installing it for every account" }
  if ((choco list --limit-output --exact $package) -match "^$package\|") { Ok "$label already installed"; return }
  Note "installing $label..."
  & choco install $package -y --no-progress @extra
  # 3010: installed, a restart finishes it.
  if ($LASTEXITCODE -ne 0 -and $LASTEXITCODE -ne 3010) { Fail "Installing $label failed (choco exit $LASTEXITCODE)." }
  Refresh-Path
  Ok "$label installed"
}

Step 'Tools'
Choco-Install 'git' 'git' 'Git' @('--params', '/NoShellIntegration')
Choco-Install 'fvm' 'fvm' 'fvm (Flutter Version Management)'
Choco-Install 'nuget.commandline' 'nuget' 'NuGet'
if (-not $NoInnoSetup) {
  $iscc = Join-Path ${env:ProgramFiles(x86)} 'Inno Setup 6\ISCC.exe'
  if (Test-Path $iscc) { Ok 'Inno Setup 6 already installed' }
  else { Choco-Install 'innosetup' $null 'Inno Setup 6' }
}

Step 'Visual Studio C++ build tools (Flutter compiles the Windows runner with them)'
$vswhere = Join-Path ${env:ProgramFiles(x86)} 'Microsoft Visual Studio\Installer\vswhere.exe'
function Find-VisualCpp {
  if (-not (Test-Path $vswhere)) { return $null }
  & $vswhere -products * -latest -requires Microsoft.VisualStudio.Component.VC.Tools.x86.x64 -property installationPath
}
$vs = Find-VisualCpp
if ($vs) {
  Ok "found at $vs"
} elseif ($NoVisualStudio) {
  Note 'skipped (-NoVisualStudio); install "Desktop development with C++" yourself'
} else {
  Note 'installing Build Tools 2022 with Desktop development with C++ (several GB; can take 20+ minutes)...'
  $components = @(
    '--add Microsoft.VisualStudio.Workload.VCTools',
    '--add Microsoft.VisualStudio.Component.VC.Tools.x86.x64',
    '--add Microsoft.VisualStudio.Component.VC.CMake.Project',
    '--add Microsoft.VisualStudio.Component.Windows11SDK.22621',
    '--includeRecommended', '--quiet', '--norestart', '--locale en-US'
  ) -join ' '
  & choco install visualstudio2022buildtools -y --no-progress --package-parameters $components
  if ($LASTEXITCODE -ne 0 -and $LASTEXITCODE -ne 3010) { Fail "Installing the Build Tools failed (choco exit $LASTEXITCODE)." }
  $vs = Find-VisualCpp
  if (-not $vs) { Fail 'The Build Tools installed without the C++ compiler; open Visual Studio Installer and add "Desktop development with C++".' }
  Ok "installed at $vs"
}

Step 'Windows settings'
# fvm links .fvm\flutter_sdk to its cache; without Developer Mode only admins may make links.
Set-Reg 'HKLM:\SOFTWARE\Microsoft\Windows\CurrentVersion\AppModelUnlock' 'AllowDevelopmentWithoutDevLicense' 1
Ok 'Developer Mode on'
# Flutter, pub and CMake paths often pass Windows' old 260-character limit.
Set-Reg 'HKLM:\SYSTEM\CurrentControlSet\Control\FileSystem' 'LongPathsEnabled' 1
& git config --system core.longpaths true
Ok 'long paths on (Windows and Git)'
# Local scripts run; downloaded ones still need a signature.
try {
  Set-ExecutionPolicy RemoteSigned -Scope LocalMachine -Force
  Ok 'PowerShell runs local scripts'
} catch {
  # A Group Policy setting wins over this one; that's fine if it allows local scripts.
  Note "execution policy unchanged: $($_.Exception.Message)"
}
if ($KeepSleep) {
  Note 'sleep settings left alone (-KeepSleep)'
} else {
  & powercfg /change standby-timeout-ac 0
  & powercfg /change hibernate-timeout-ac 0
  Ok 'no sleep on mains power (the screen may still turn off)'
}

Step 'Defender'
$state = Join-Path $env:ProgramData 'Yonder\agent'
$buildHome = Join-Path $env:SystemDrive 'Users\yonder-build'
$excluded = @(
  (Join-Path $state 'workspaces'),
  (Join-Path $buildHome 'AppData\Local\fvm'),
  (Join-Path $buildHome 'AppData\Local\Pub\Cache'),
  (Join-Path $buildHome '.shorebird')
)
if ($NoDefenderExclusions) {
  Note 'no exclusions (-NoDefenderExclusions); builds will be slower'
} else {
  try {
    foreach ($path in $excluded) { Add-MpPreference -ExclusionPath $path }
    Ok 'skips Yonder workspaces and the build account''s Flutter, pub and Shorebird caches'
  } catch {
    Note "couldn't add exclusions ($($_.Exception.Message)); another antivirus may be in charge"
  }
}

Step 'Yonder'
$service = Get-Service -Name YonderAgent -ErrorAction SilentlyContinue
$buildUser = Get-LocalUser -Name yonder-build -ErrorAction SilentlyContinue
if ($service -and $buildUser) {
  Ok "agent service is $($service.Status.ToString().ToLower()); jobs run as yonder-build"
} elseif ($service) {
  Note 'the agent runs jobs as SYSTEM; reinstall it to get the yonder-build account:'
  Note '  yonder-agent service install'
} else {
  Note 'the agent isn''t installed yet. Build it and install the service:'
  Note '  cargo build --release; .\target\release\yonder-agent.exe service install'
  Note '  then pair it with your Mac: yonder-agent pair'
}

Step 'Versions'
foreach ($tool in 'git', 'fvm', 'nuget') {
  if (Has $tool) {
    $line = switch ($tool) {
      'git' { (& git --version) }
      'fvm' { "fvm $(& fvm --version)" }
      'nuget' { ((& nuget help) | Select-Object -First 1) }
    }
    Note $line
  } else {
    Note "$tool missing"
  }
}
if ($vs) { Note "Visual Studio C++: $vs" }

$pendingReboot = (Test-Path 'HKLM:\SOFTWARE\Microsoft\Windows\CurrentVersion\Component Based Servicing\RebootPending') -or
  (Test-Path 'HKLM:\SOFTWARE\Microsoft\Windows\CurrentVersion\WindowsUpdate\Auto Update\RebootRequired')
Write-Host "`nThe machine part is done." -ForegroundColor Green
if ($pendingReboot) { Write-Host '  Windows wants a restart to finish installing; restart before the first build.' -ForegroundColor Yellow }
Write-Host '  Next, on your Mac in the project folder:  yonder setup win'
