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

Shorebird signs in with a token, which you save as a Yonder secret. On your computer,
run `shorebird login:ci`, then `yonder secret set win SHOREBIRD_TOKEN` and paste the
token when asked. A job gets it only when it asks for it with
`yonder run win --secret SHOREBIRD_TOKEN -- …`, and `yonder setup` passes it along when
it's saved. Signing keys work the same way, as files:
`yonder secret set win dsa_priv.pem --file dsa_priv.pem`, then
`--secret-file dsa_priv.pem` on the job that signs.

## Files git ignores

Builds often need files that git ignores, such as `.env`. List them in
`.yonderinclude` at the project root, one path or glob per line, and Yonder sends them
with every sync:

```
# needed to build a release
.env
dsa_pub.pem
```

Private keys and tokens are better saved as secrets (above): a synced file stays in the
machine's copy of the project, while a secret file exists only while its job runs.
