---
title: Persistent Tmux Terminals - Zed Tmux
description: Attach local or SSH workspaces to existing tmux sessions and restore them with the workspace layout.
---

# Tmux Terminals

Zed Tmux adds a restricted terminal type that attaches to an existing tmux
session. It stores the terminal in the local workspace snapshot, so reopening
the same project restores the terminal tab, pane placement, and tmux target.

The terminal does not create tmux servers or sessions. It also does not accept
an arbitrary shell command.

## Attach a Session {#attach-a-session}

Install `tmux` on the machine that owns the project and start the session before
opening it in Zed Tmux. For an SSH project, that machine is the SSH server. For
a local project, it is your Mac.

To attach in the terminal panel:

1. Open the terminal panel menu.
2. Select **New tmux**.
3. Enter the exact session name and confirm.

To attach in a center pane, open a pane's tab menu and select **New tmux**. You
can also run {#action workspace::NewCenterTmux} from the command palette.

Zed Tmux sends the name directly to:

```sh
tmux -N attach-session -t '=SESSION_NAME'
```

The session name is one process argument, not executable shell input. The `=`
prefix requests an exact tmux target match, and `-N` prevents tmux from starting
a server. If the session does not exist, the attach fails without creating it.
Close the terminal or create the session outside Zed Tmux and reopen it.

Zed Tmux does not discover or list sessions. A batch session picker is outside
the current built-in feature.

## Workspace Restoration {#workspace-restoration}

The local workspace database stores the tmux session name with the normal Zed
pane and item layout. When you reopen the same local project or SSH workspace,
Zed Tmux creates the same restricted terminal and attempts the same attach
command again.

This restores:

- terminal panel and center-pane items
- splits and tab placement
- the active item
- the exact tmux session target

It does not copy terminal processes or shell state. The tmux server owns that
state and must remain running on the project machine. If the server or session
is gone, the restored terminal exits with the tmux error.

For the standalone macOS app, workspace state is stored under:

```text
~/Library/Application Support/Zed Tmux/db/0-dev/db.sqlite
```

The SSH connection identity and remote project roots select the matching local
workspace. Nothing is written to the project for layout persistence, and no
workspace snapshot is synchronized through the remote server.

The standalone build uses a separate application bundle and data directory
from standard Zed:

- application name: `Zed Tmux`
- bundle identifier: `dev.zed.Zed-Tmux`
- application data: `~/Library/Application Support/Zed Tmux`
- logs: `~/Library/Logs/Zed Tmux`

It currently shares the normal `~/.config/zed` configuration directory.

## Remote Operation {#remote-operation}

An SSH workspace runs `tmux` on the SSH server through Zed's existing remote
terminal transport. The macOS UI remains local. Zed's remote server still hosts
project services such as language servers, tasks, and terminal PTYs; tmux owns
the persistent shell sessions.

The macOS bundle includes a gzip-compressed Linux x86-64 Zed remote server. On
the first connection, Zed installs the commit-matched binary under
`~/.zed_server` on the SSH host. Other remote operating systems and
architectures fall back to Zed's existing remote-server handling.

## Build the macOS App {#build-macos}

Build the release bundle on macOS. You need Xcode and its Metal toolchain,
Rust, Zig, `cargo-zigbuild`, Node.js, and npm.

```sh
xcodebuild -downloadComponent MetalToolchain
cargo install cargo-zigbuild
brew install zig
./script/bundle-mac aarch64-apple-darwin
```

The script builds the macOS app and a static Linux x86-64 remote server, signs
the app ad hoc when release credentials are absent, and creates:

```text
target/aarch64-apple-darwin/release/Zed-Tmux-aarch64.dmg
```

To reuse an existing compressed Linux x86-64 remote-server binary:

```sh
ZED_BUNDLED_REMOTE_SERVER_PATH=/path/to/zed-remote-server-linux-x86_64.gz \
  ./script/bundle-mac aarch64-apple-darwin
```

The script verifies that the supplied file is gzip-compressed and contains a
Linux x86-64 ELF executable before embedding it.

## Implementation Boundaries {#implementation-boundaries}

The built-in feature owns the security-sensitive parts:

- constructing the fixed tmux command
- creating the local or SSH terminal PTY
- persisting the terminal source and exact session name
- restoring items through the normal workspace serialization chain

A future extension may provide a session list with multi-selection, but it
should call a narrow attach API. It should not receive arbitrary remote command
execution or unrestricted terminal creation capabilities.

The main implementation lives in:

- `crates/project/src/terminals.rs`
- `crates/terminal_view/src/remote_tmux.rs`
- `crates/terminal_view/src/persistence.rs`
- `crates/terminal_view/src/terminal_panel.rs`
- `crates/terminal_view/src/terminal_view.rs`
- `crates/remote/src/transport.rs`
- `crates/remote/src/transport/ssh.rs`
- `script/bundle-mac`
