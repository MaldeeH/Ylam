# Ylam

## Build And Install Locally

```bash
cargo install --path .
```

## Configuration

Ylam reads its optional global configuration from:

```text
~/.config/ylam/config.toml
```

If the file does not exist, Ylam uses its built-in defaults.

### Example

```toml
default_agent = "codex"
editor = "nvim"
main_branch = "main"
new_command = ""
close_strategy = "squash"

[dotfiles]
paths = [".env", ".envrc", ".claude", ".codex"]

[tmux]
close_key = "q"
done_color = "brightyellow"
attention_color = "yellow"
failed_color = "red"

[agents.claude]
command = "claude"

[agents.codex]
command = "codex"
```

### Options

| Option | Default | Description |
| --- | --- | --- |
| `default_agent` | `"claude"` | Agent used by `ylam new` when `--agent` is not supplied. It must have a matching `[agents.NAME]` section. |
| `editor` | `"nvim"` | Editor command started in the left tmux pane. |
| `main_branch` | `"main"` | Branch into which worker branches are integrated. The default falls back to `master` when the repository has no `main` branch. |
| `new_command` | unset | Optional shell command run in the background after creating a worker worktree. An unset or empty value runs nothing. Output is written to `.ylam-new-command.log` in the worktree. |
| `close_strategy` | `"merge"` | Integration strategy used by `ylam close`. Valid values are `merge`, `rebase`, `squash`, and `pr-admin-merge`. |
| `dotfiles.paths` | `[".env", ".envrc", ".claude", ".codex"]` | Files or directories copied from the main repository into each new worker worktree. Write this array on one line. Missing paths are ignored. |
| `tmux.close_key` | `"q"` | Tmux prefix key bound to `ylam close`. With the default tmux prefix, use `prefix + q`. Set it to an empty string to disable installing the shortcut. Outside Ylam-tracked windows, this retains tmux's normal `display-panes` behavior. |
| `tmux.done_color` | `"brightyellow"` | Tmux tab color used for completed agent events. |
| `tmux.attention_color` | `"yellow"` | Tmux tab color used when an agent needs attention or permission. |
| `tmux.failed_color` | `"red"` | Tmux tab color used for failed agent events. |
| `agents.NAME.command` | `"claude"` and `"codex"` | Command started in the right tmux pane for the named agent. Additional agent names can be added with more sections. |

To opt into a setup command for newly created worker worktrees, set it explicitly:

```toml
new_command = "just bootstrap"
```

### Close strategies

- `merge` performs a normal merge of the worker branch into main.
- `rebase` rebases the worker branch onto main, then fast-forwards main.
- `squash` creates one squash commit on main.
- `pr-admin-merge` pushes the worker branch, creates a GitHub pull request when needed, admin-merges it with `gh`, and then updates local main. This requires an authenticated GitHub CLI and an `origin` remote.

`ylam close` only works from a tracked worker window. It refuses to close the main window or integrate worktrees containing uncommitted tracked changes. During integration, the tmux tab displays a close spinner. If integration fails, the worktree and window remain open with a `CLOSEFAIL` marker. After successful integration, Ylam removes the worker worktree and branch before closing the tmux window.
