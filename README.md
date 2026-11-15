# Yonder

Use your other computers as if they were part of this one.

Yonder lets you run a project's command on another machine you own, from wherever you
are — build and sign your Windows app from a Mac, use the big desktop at home from a
laptop on hotel wifi — without remote desktop and without a VM heating up your laptop.

```sh
cd my-app
yonder run win -- flutter build windows
```

- **Jobs keep running when your connection drops.** Reattach to the live log from anywhere.
- **Works while the machine is signed out.** The agent runs as a service.
- **Persistent terminals**, including on Windows.
- **No open ports, no account.** Devices pair with each other directly; every command is
  signed by the device that sent it.
- **You can see and remove everything Yonder leaves on a machine** — credentials, repo
  copies, caches — from the app, at any time.

## Status

Under active development. Not ready for use yet.

## Layout

| Path            | What it is                                              |
|-----------------|---------------------------------------------------------|
| `crates/core`   | Identity, connections and protocol shared by everything |
| `crates/agent`  | `yonder-agent`, runs on machines you want to use        |
| `crates/cli`    | `yonder`, the command line                              |
| `app/`          | Flutter app (macOS first, then mobile)                  |
| `recipes/`      | Built-in setup recipes for common toolchains            |

## Building

Requires Rust (the version is pinned in `rust-toolchain.toml`; rustup installs it).

```sh
cargo build --workspace
cargo test --workspace
cargo run -p yonder-cli -- --version
```

## License

Apache-2.0. See [LICENSE](LICENSE).
