# Yonder: rebuilds the agent from the copy of Yonder's source this job runs in, then
# reinstalls the service. Runs as an admin job (SYSTEM). The reinstall stops the agent,
# and Windows ends a job's processes when the agent stops, so a scheduled task does it
# a few seconds after this job has reported back.
$ErrorActionPreference = 'Continue'
$ProgressPreference = 'SilentlyContinue'
function Fail($m) { Write-Host "X $m" -ForegroundColor Red; exit 1 }

# SYSTEM has no Rust of its own: use the toolchain someone installed with rustup.
$cargo = Get-Command cargo -ErrorAction SilentlyContinue | Select-Object -First 1 -ExpandProperty Source
if (-not $cargo) {
  $found = Get-ChildItem (Join-Path $env:SystemDrive 'Users') -Directory -ErrorAction SilentlyContinue |
    ForEach-Object { Join-Path $_.FullName '.cargo\bin\cargo.exe' } |
    Where-Object { Test-Path $_ } | Select-Object -First 1
  if (-not $found) { Fail 'no Rust on this machine; install it from https://rustup.rs and try again' }
  $cargo = $found
  $env:RUSTUP_HOME = Join-Path (Split-Path (Split-Path (Split-Path $found))) '.rustup'
}
# Everything happens in a folder only SYSTEM and Administrators can change: the synced
# copy is writable by the build account, and what's built here runs as SYSTEM.
$home_ = Join-Path $env:ProgramData 'Yonder\upgrade'
New-Item -ItemType Directory -Force -Path $home_ | Out-Null
& icacls $home_ /inheritance:r /grant:r '*S-1-5-18:(OI)(CI)F' /grant:r '*S-1-5-32-544:(OI)(CI)F' /Q | Out-Null
if ($LASTEXITCODE -ne 0) { Fail "couldn't lock $home_" }
$src = Join-Path $home_ 'src'
# /MIR keeps it equal to the synced copy, /XJ skips junctions a job may have left.
& robocopy (Get-Location).Path $src /MIR /XJ /XD target .git /NFL /NDL /NJH /NJS /NP | Out-Null
if ($LASTEXITCODE -ge 8) { Fail "couldn't copy the source (robocopy exit $LASTEXITCODE)" }
Set-Location $src
# Downloads go here too, not into that person's profile.
$env:CARGO_HOME = Join-Path $home_ 'cargo'
$env:CARGO_TARGET_DIR = Join-Path $home_ 'target'
Write-Host "> building the agent with $cargo"
& $cargo build --release -p yonder-agent
if ($LASTEXITCODE -ne 0) { Fail "the build failed (cargo exit $LASTEXITCODE)" }

$built = Join-Path $env:CARGO_TARGET_DIR 'release\yonder-agent.exe'
$staged = Join-Path $home_ 'yonder-agent.exe'
Copy-Item -Force $built $staged
$log = Join-Path $home_ 'install.log'
Remove-Item -Force $log -ErrorAction SilentlyContinue

Write-Host '> reinstalling the service in a few seconds'
$action = New-ScheduledTaskAction -Execute 'cmd.exe' -Argument "/c ping -n 6 127.0.0.1 >nul & `"$staged`" service install > `"$log`" 2>&1"
$principal = New-ScheduledTaskPrincipal -UserId 'SYSTEM' -LogonType ServiceAccount -RunLevel Highest
Register-ScheduledTask -TaskName 'Yonder upgrade' -Action $action -Principal $principal -Force | Out-Null
Start-ScheduledTask -TaskName 'Yonder upgrade'
Write-Host 'OK the agent restarts shortly'
