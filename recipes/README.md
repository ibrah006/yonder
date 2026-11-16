# Recipes

Setup recipes bring a machine to the state a project needs, starting from a fresh install.
Each recipe has two parts:

- **Machine part** (`machine.ps1`): tools for every account, which needs an administrator.
  Run it once on the machine itself. It's safe to run again; anything already there is
  skipped.
- **Project part** (`project.ps1`): the project's own versions, installed for the build
  account that jobs run as. `yonder setup <machine>` runs it from your computer, in the
  machine's copy of the project, and lists anything still missing.

## flutter-windows

Flutter apps for Windows, with Shorebird when the project has `shorebird.yaml`.

**Machine part** (once, on the PC, in an Administrator PowerShell, from a Yonder checkout):

```powershell
Set-ExecutionPolicy Bypass -Scope Process -Force
.\recipes\flutter-windows\machine.ps1
```

| Installs or sets | Why |
| --- | --- |
| Chocolatey | installs everything below for every account |
| Git, with long paths | Flutter, fvm and pub need it |
| fvm | installs the Flutter version the project pins |
| NuGet | plugins that pull NuGet packages during the build |
| Inno Setup 6 | projects that package an installer (`-NoInnoSetup` to skip) |
| VS 2022 Build Tools, Desktop development with C++ | MSVC, CMake and the Windows SDK for the Windows runner (skipped if a Visual Studio with C++ is there; `-NoVisualStudio`) |
| Developer Mode | fvm links `.fvm\flutter_sdk`; only admins can make links otherwise |
| Long paths | pub and CMake paths pass 260 characters |
| PowerShell execution policy RemoteSigned | local scripts such as `scripts\release_windows.ps1` run |
| No sleep on mains power | the PC stays reachable (`-KeepSleep` to skip) |
| Defender exclusions | Yonder's workspaces and the build account's fvm, pub and Shorebird caches (`-NoDefenderExclusions` to skip) |

**Project part** (from your computer, in the project):

```sh
yonder setup win
```

This installs the project's Flutter (`.fvmrc`) through fvm and links it at
`.fvm\flutter_sdk`. It then precaches the Windows artifacts, runs `pub get` and installs
Shorebird for the build account. Finally it checks Inno Setup and the files
`.yonderinclude` lists, and ends with `flutter doctor`. If anything is still missing, it
exits with code 2 and lists it.

Shorebird signs in with a token. On your computer, run `shorebird login:ci`. Then open
`yonder shell win` and run:

```powershell
[Environment]::SetEnvironmentVariable('SHOREBIRD_TOKEN', '<token>', 'User')
```

Use the shell rather than a job: a job's command is written to the machine's audit log,
and keys typed into a shell are not.

## Files git ignores

Builds often need files that git ignores, such as `.env` or signing keys. List them in
`.yonderinclude` at the project root, one path or glob per line, and Yonder sends them
with every sync:

```
# needed to build and sign a release
.env
dsa_priv.pem
dsa_pub.pem
```
