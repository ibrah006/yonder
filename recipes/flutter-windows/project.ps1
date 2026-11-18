# Yonder recipe: Flutter for Windows (with Shorebird), project part.
#
# `yonder setup win` runs this as the build account, in the machine's copy of
# the project. Safe to run again. It needs the machine part first (machine.ps1,
# once, as admin), then:
#
#   - the Flutter version the project pins (.fvmrc) through fvm, linked at
#     .fvm\flutter_sdk, with the Windows build artifacts and packages
#   - Shorebird, when the project has shorebird.yaml
#   - checks: Inno Setup for projects with an .iss, and the git-ignored files
#     .yonderinclude lists
# Continue: Windows PowerShell turns a tool's stderr into errors, and these tools
# report progress there. Each step checks the exit code instead.
$ErrorActionPreference = 'Continue'
$ProgressPreference = 'SilentlyContinue'
# Tools skip their first-run questions; nobody is there to answer.
$env:CI = 'true'

$todo = New-Object System.Collections.Generic.List[string]
function Step($m) { Write-Host "`n> $m" -ForegroundColor Cyan }
function Ok($m) { Write-Host "  OK $m" -ForegroundColor Green }
function Note($m) { Write-Host "  - $m" }
function Need($m) { Write-Host "  ! $m" -ForegroundColor Yellow; $todo.Add($m) }
function Has($command) { [bool](Get-Command $command -ErrorAction SilentlyContinue) }
function Done {
  if ($todo.Count -eq 0) {
    Write-Host "`nThis machine is ready to build $(Split-Path -Leaf (Get-Location))." -ForegroundColor Green
    exit 0
  }
  Write-Host "`nStill needed:" -ForegroundColor Yellow
  foreach ($item in $todo) { Write-Host "  - $item" }
  exit 2
}

Write-Host "Setting up $(Split-Path -Leaf (Get-Location)) for $env:USERNAME on $env:COMPUTERNAME"

Step 'Machine tools'
$machinePart = 'run recipes\flutter-windows\machine.ps1 on the PC as admin'
foreach ($tool in 'git', 'fvm', 'nuget') {
  if (Has $tool) { Ok $tool } else { Need "$tool isn't installed for every account ($machinePart)" }
}
$vswhere = Join-Path ${env:ProgramFiles(x86)} 'Microsoft Visual Studio\Installer\vswhere.exe'
$vs = if (Test-Path $vswhere) {
  & $vswhere -products * -latest -requires Microsoft.VisualStudio.Component.VC.Tools.x86.x64 -property installationPath
}
if ($vs) { Ok "Visual Studio C++ at $vs" } else { Need "Visual Studio's C++ build tools are missing ($machinePart)" }
$devMode = (Get-ItemProperty 'HKLM:\SOFTWARE\Microsoft\Windows\CurrentVersion\AppModelUnlock' -ErrorAction SilentlyContinue).AllowDevelopmentWithoutDevLicense
if ($devMode -eq 1) { Ok 'Developer Mode' } else { Need "Developer Mode is off; fvm can't link the SDK ($machinePart)" }
if ($todo.Count -gt 0) { Done }

Step 'Flutter (fvm)'
$version = $null
if (Test-Path .fvmrc) { $version = (Get-Content .fvmrc -Raw | ConvertFrom-Json).flutter }
if (-not $version -and (Test-Path .fvm\fvm_config.json)) {
  $version = (Get-Content .fvm\fvm_config.json -Raw | ConvertFrom-Json).flutterSdkVersion
}
if (-not $version) {
  Need 'the project pins no Flutter version; run `fvm use <version>` in it on your Mac and commit .fvmrc'
  Done
}
Note "project pins Flutter $version"
& fvm install $version --setup
if ($LASTEXITCODE -ne 0) { Need "fvm couldn't install Flutter $version (output above)"; Done }
& fvm use $version --force --skip-pub-get
$flutter = '.fvm\flutter_sdk\bin\flutter.bat'
if ($LASTEXITCODE -ne 0 -or -not (Test-Path $flutter)) { Need "fvm couldn't link .fvm\flutter_sdk (output above)"; Done }
Ok "Flutter $version linked at .fvm\flutter_sdk"
& $flutter config --no-analytics | Out-Null
& $flutter precache --windows
if ($LASTEXITCODE -ne 0) { Need "flutter precache --windows failed (output above)" }
& $flutter pub get
if ($LASTEXITCODE -ne 0) { Need 'flutter pub get failed (output above)' } else { Ok 'packages' }

if (Test-Path shorebird.yaml) {
  Step 'Shorebird'
  $shorebirdBin = Join-Path $HOME '.shorebird\bin'
  if (-not (Has shorebird) -and (Test-Path (Join-Path $shorebirdBin 'shorebird.bat'))) { $env:Path += ";$shorebirdBin" }
  if (Has shorebird) {
    Ok 'already installed'
  } else {
    Note 'installing for this account...'
    # Its own process, so an `exit` in the installer can't end this script.
    $installer = Join-Path $env:TEMP 'shorebird-install.ps1'
    try {
      Invoke-WebRequest -UseBasicParsing 'https://raw.githubusercontent.com/shorebirdtech/install/main/install.ps1' -OutFile $installer
      & powershell.exe -NoProfile -NonInteractive -ExecutionPolicy Bypass -File $installer
    } catch {
      Note "couldn't download the installer: $($_.Exception.Message)"
    }
    $env:Path += ";$shorebirdBin"
  }
  if (Has shorebird) {
    # The first run downloads Shorebird's own Flutter.
    & shorebird --version
    # No stderr redirect: Windows PowerShell turns the progress Shorebird writes there
    # into an error that stops its own script.
    $versions = ''
    try { $versions = (& shorebird flutter versions list) -join "`n" } catch { Note "couldn't list Shorebird's Flutter versions: $($_.Exception.Message)" }
    if ($versions -and $versions -notmatch [regex]::Escape($version)) {
      Need "Shorebird has no Flutter $version yet; update it: yonder run win -- shorebird upgrade"
    }
    if ($env:SHOREBIRD_TOKEN) {
      Ok 'signed in (SHOREBIRD_TOKEN secret)'
    } else {
      Need 'save the Shorebird token as a secret: on your Mac run `shorebird login:ci`, then `yonder secret set win SHOREBIRD_TOKEN` and paste it (jobs get it with --secret SHOREBIRD_TOKEN)'
    }
    & shorebird doctor
  } else {
    Need "Shorebird didn't install (output above)"
  }
}

Step 'Release extras'
$iss = Get-ChildItem windows -Recurse -Filter *.iss -ErrorAction SilentlyContinue | Select-Object -First 1
if ($iss) {
  $iscc = Join-Path ${env:ProgramFiles(x86)} 'Inno Setup 6\ISCC.exe'
  if ($env:INNO_SETUP_PATH -or (Test-Path $iscc)) { Ok "Inno Setup 6 (for $($iss.Name))" }
  else { Need "Inno Setup 6 is missing ($machinePart)" }
}
if (Test-Path .yonderinclude) {
  $lines = Get-Content .yonderinclude | ForEach-Object { $_.Trim() } |
    Where-Object { $_ -and -not $_.StartsWith('#') -and $_ -notmatch '[*?\[{]' }
  foreach ($line in $lines) {
    $path = $line.TrimStart('/')
    if (Test-Path -LiteralPath $path) { Ok "$path (from .yonderinclude)" }
    else { Need "$path is missing; put it in the project on your Mac (git ignores it, Yonder sends it)" }
  }
} else {
  Note 'no .yonderinclude: git-ignored files (.env, signing keys) are not sent to this machine'
}

Step 'flutter doctor'
& $flutter doctor
Done
