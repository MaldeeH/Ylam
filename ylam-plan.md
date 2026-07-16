# Ylam Minimal Plan

Ylam stands for **Your Local Agent Manager**.

It is a small Rust CLI invoked as:

```bash
ylam
```

Ylam starts local AI development workspaces by combining:

```text
git worktree
tmux
nvim
Claude / Codex
local dotfiles
```

The first version should stay intentionally small. It should create persistent numbered worker worktrees, prepare a tmux window, open `nvim` on the left, open an AI agent on the right, and update the tmux tab color when the agent finishes or needs attention.

---

## Current Direction

The first version is not a full workflow platform.

No automatic worker-to-main merge automation yet.
No `just` lifecycle yet.
No dashboard yet.
No dirty worktree policy yet.
No repo-local config files yet.

Those can be added later after the basic workflow feels good.

The first goal is:

```bash
ylam new
```

and Ylam should create a ready-to-use persistent worker workspace.

---

## Core Workflow

From inside a git repository:

```bash
ylam new
```

Example:

```bash
ylam new
```

Ylam should:

```text
detect the current git repository
allocate the next numbered worker: wt1, wt2, wt3
create a persistent worker branch
create a git worktree outside the repo
copy configured dotfiles into the worktree
start a tmux window
open nvim in the left pane
open Claude or Codex in the right pane
track the workspace in global Ylam state
```

When Claude or Codex finishes, a hook should call Ylam:

```bash
ylam event agent-done
```

Ylam should then change the current tmux window color.

When the user manually merges a finished worker branch into main with lazygit or regular git, Ylam should be able to sync main back out to all workers:

```bash
ylam refresh
```

`ylam refresh` should merge main into every Ylam worker branch. It should not merge worker branches back into main.

---

## State Model

Ylam should keep all of its own state globally.

Nothing should be created inside the target repository for v0.

Suggested location:

```text
~/.local/share/ylam/
```

Inside that folder, Ylam creates one folder per repository.

Example:

```text
~/.local/share/ylam/
  repos/
    myrepo-a13f9c/
      state.json
      worktrees/
        wt1/
        wt2/
    api-server-82c10b/
      state.json
      worktrees/
        wt1/
```

The repository folder name should include both:

```text
human-readable repo name
short stable hash of the repo root path
```

Example:

```text
myrepo-a13f9c
```

This avoids collisions when two repositories have the same directory name.

---

## Worktree Location

Worktrees should live under Ylam's global state directory, not inside the repository.

Example:

```text
repo:
  ~/code/myrepo/

ylam state:
  ~/.local/share/ylam/repos/myrepo-a13f9c/

worktrees:
  ~/.local/share/ylam/repos/myrepo-a13f9c/worktrees/wt1/
  ~/.local/share/ylam/repos/myrepo-a13f9c/worktrees/wt2/
```

This keeps target repositories completely untouched except for normal git branches and git worktree metadata.

---

## Global Config

Ylam should have one global config file.

Suggested location:

```text
~/.config/ylam/config.toml
```

Example:

```toml
default_agent = "claude"
editor = "nvim"
main_branch = "main"

[dotfiles]
paths = [
  ".envrc",
  ".env",
  ".claude",
  ".codex",
  ".editorconfig"
]

[tmux]
done_color = "green"
attention_color = "yellow"
running_color = "cyan"

[agents.claude]
command = "claude"

[agents.codex]
command = "codex"
```

The config should be optional at first. Ylam should run with sensible defaults if the file does not exist.

---

## Global State File

Each tracked repository has its own state file.

Example:

```text
~/.local/share/ylam/repos/myrepo-a13f9c/state.json
```

Example state:

```json
{
  "version": 1,
  "repo_name": "myrepo",
  "repo_root": "/Users/me/code/myrepo",
  "repo_key": "myrepo-a13f9c",
  "worktree_root": "/Users/me/.local/share/ylam/repos/myrepo-a13f9c/worktrees",
  "workspaces": {
    "wt1": {
      "branch": "wt1-myrepo",
      "path": "/Users/me/.local/share/ylam/repos/myrepo-a13f9c/worktrees/wt1",
      "agent": "claude",
      "tmux_window_id": "@12",
      "status": "running",
      "created_at": "2026-04-28T12:00:00Z",
      "updated_at": "2026-04-28T12:10:00Z"
    }
  }
}
```

For v0, the state only needs to support:

```text
repo identification
workspace id
branch name
worktree path
agent
tmux window id
status
timestamps
```

---

## Worker Branch Naming

Ylam-created branches are persistent worker branches.

Default:

```text
wt<number>-<repo-name>
```

Example:

```bash
ylam new
```

creates:

```text
wt1-myrepo
```

The next worker creates:

```text
wt2-myrepo
```

Each worker branch is a parallel instance of main. It is not a polished feature branch. The user can manually merge a worker branch into main when that worker contains useful finished work.

---

## Commands For V0

### Create Workspace

```bash
ylam new
```

Optional:

```bash
ylam new --agent claude
ylam new --agent codex
```

Behavior:

```text
detect repo root
compute repo key
load or create repo state
allocate next workspace id: wt1, wt2, wt3
create branch: wt<number>-<repo-name>
create git worktree in global Ylam state directory
copy configured dotfiles from repo root into worktree
create a new tmux window in the current tmux session
left pane: nvim
right pane: selected agent
store the tmux window id in state
write state
switch to the new tmux window
```

### List Workspaces

```bash
ylam list
```

For v0, this can be simple:

```text
Repo     Workspace  Branch      Agent   Status   Window
myrepo   wt1        wt1-myrepo  claude  running  @12
myrepo   wt2        wt2-myrepo  codex   done     @15
```

It can list all known global Ylam workspaces, not only the current repo.

### Refresh Workers

```bash
ylam refresh
```

Behavior:

```text
detect the current repo
load Ylam state for that repo
for every Ylam worker branch:
  switch to that worker worktree
  merge main into the worker branch
  leave main untouched
```

Important rule:

```text
ylam refresh never merges worker branches into main.
```

The intended workflow is:

```text
user manually merges useful worker work into main with lazygit
user runs ylam refresh
Ylam merges updated main into every worker branch
all persistent workers are back in sync with main
```

### Event

```bash
ylam event <event>
```

Examples:

```bash
ylam event running
ylam event agent-done
ylam event permission-requested
ylam event failed
```

Behavior:

```text
read the current tmux window id from tmux
resolve the matching workspace from global state
update global state
change the current tmux window color
optionally rename the tmux window with a status marker
```

This is the hook interface for Claude and Codex.

The hook does not need to know the workspace id. It only needs to run inside the same tmux window as the agent process.

---

## Tmux Layout

V0 should use a simple two-pane layout.

```text
┌──────────────────────┬──────────────────────┐
│ nvim                 │ claude / codex       │
│ editor               │ AI agent             │
└──────────────────────┴──────────────────────┘
```

Ylam should:

```text
create a tmux window in the current tmux session
start in the worktree directory
split into left and right panes
run nvim on the left
run the selected agent on the right
```

For v0, Ylam should expect to be run from inside tmux. Later it can grow a fallback mode that creates a tmux session when none exists.

Window name:

```text
wt1:myrepo
```

---

## Tmux Color Events

Ylam should map events to tmux colors.

Minimal mapping:

```text
running               cyan
agent-done            green
permission-requested  yellow
failed                red
```

The exact tmux implementation can be refined during development.

Possible approaches:

```text
set window option for active/inactive style
rename window with a status marker
set tmux user option on window
integrate with existing tmux statusline later
```

For v0, color change plus window rename is enough.

Example window names:

```text
wt1:myrepo
wt1:myrepo:DONE
wt1:myrepo:PERM
wt1:myrepo:FAIL
```

---

## Agent Event Hooks

Ylam should not require environment variables for v0.

Claude and Codex hooks should notify Ylam from inside the current tmux window:

```bash
ylam event agent-done
ylam event permission-requested
ylam event failed
```

Ylam should ask tmux which window the hook is running in:

```bash
tmux display-message -p '#{window_id}'
```

Then Ylam should:

```text
find the workspace with that tmux window id
update the workspace status in global state
change the current tmux window color
rename the current tmux window with a short status marker
```

This keeps hooks simple and avoids passing workspace ids, repo keys, or injected environment variables.

---

## Dotfile Copying

Ylam should copy configured files or folders from the repo root into the worktree.

Initial defaults:

```text
.env
.envrc
.claude
.codex
```

Behavior:

```text
if the path exists in the repo root, copy it to the worktree
if the path does not exist, skip it
if the destination already exists, overwrite for v0
```

This keeps agent and environment setup available in the generated worktree.

Later refinements:

```text
configurable overwrite policy
symlink mode
ignore secrets by default
interactive confirmation
```

For v0, simple copy behavior is acceptable.

---

## Rust Implementation

Ylam should be implemented as a small Rust CLI.

Suggested crates:

```text
clap        CLI parsing
serde       serialization
serde_json  state files
toml        config file
anyhow      application errors
chrono      timestamps
dirs        config/data directory lookup
sha1        stable repo key hash
which       dependency detection
```

Optional later:

```text
ratatui     dashboard UI
tracing     structured logging
notify      filesystem watching
```

### V0 Project Layout

```text
ylam/
  Cargo.toml
  src/
    main.rs
```

For v0, Ylam should be implemented in a single Rust file.

This keeps the project easy to change while the workflow is still being discovered.

`src/main.rs` can contain:

```text
CLI definitions
config defaults and loading
state structs and load/save helpers
repo detection and repo key generation
git command helpers
tmux command helpers
dotfile copying
agent command selection
command handlers for new/list/refresh/event
```

Split into modules only after the single file becomes painful to navigate.

Possible later module split:

```text
cli.rs
config.rs
state.rs
repo.rs
git.rs
tmux.rs
agents.rs
dotfiles.rs
commands/
```

## V0 Milestone

Implement only:

```bash
ylam new
ylam list
ylam refresh
ylam event <event>
```

Support only:

```text
global config
global state
git worktree creation
branch creation
main-to-worker refresh
dotfile copying
two-pane tmux layout
nvim left pane
Claude or Codex right pane
tmux event color updates
```

Do not implement yet:

```text
automatic worker-to-main merge automation
cleanup automation
just integration
dashboard
dirty worktree checks
PR creation
push support
multi-pane layouts
repo-local config
```

---

## Later Features

Add only after the basic loop feels good:

```text
ylam done
ylam abort
automatic merge into main
just setup / refresh / test integration
dirty worktree policy
tmux dashboard
Codex permission hooks
Claude permission hooks
shell completions
repo-specific config overrides
```

---

## Guiding Principle

Ylam should start as a small command that creates a useful AI coding workspace.

The initial mental model should be:

```bash
ylam new
```

and then work happens in tmux.

Everything else should be added only when repeated real usage makes it necessary.
