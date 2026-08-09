use std::collections::BTreeMap;
use std::env;
use std::error::Error;
use std::ffi::OsStr;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::time::{SystemTime, UNIX_EPOCH};

type Result<T> = std::result::Result<T, Box<dyn Error>>;

struct Config {
    default_agent: String,
    editor: String,
    main_branch: String,
    new_command: Option<String>,
    close_strategy: String,
    close_key: Option<String>,
    dotfiles: Vec<String>,
    done_color: String,
    attention_color: String,
    failed_color: String,
    agents: BTreeMap<String, String>,
}

struct Repo {
    name: String,
    root: PathBuf,
    key: String,
    state: PathBuf,
    parent: PathBuf,
}

struct State {
    repo_name: String,
    repo_root: PathBuf,
    repo_key: String,
    worktree_root: PathBuf,
    workspaces: BTreeMap<String, Workspace>,
}

struct TmuxWindow {
    id: String,
    workspace_key: String,
    name: String,
}

struct Workspace {
    branch: String,
    path: PathBuf,
    agent: String,
    tmux_window_id: String,
    status: String,
    created_at: String,
    updated_at: String,
}

#[derive(Clone, Copy)]
enum CloseStrategy {
    Merge,
    Rebase,
    Squash,
    PrAdminMerge,
}

impl CloseStrategy {
    fn parse(value: &str) -> Result<Self> {
        match value {
            "merge" => Ok(Self::Merge),
            "rebase" => Ok(Self::Rebase),
            "squash" => Ok(Self::Squash),
            "pr-admin-merge" => Ok(Self::PrAdminMerge),
            _ => Err(format!(
                "unknown close_strategy: {value} (expected merge, rebase, squash, or pr-admin-merge)"
            )
            .into()),
        }
    }
}

fn main() {
    if let Err(error) = run() {
        eprintln!("error: {error}");
        std::process::exit(1);
    }
}

fn run() -> Result<()> {
    let config = load_config();
    let args: Vec<String> = env::args().collect();

    match args.get(1).map(String::as_str) {
        Some("new") => cmd_new(&config, parse_agent(&args)?),
        Some("close") => cmd_close(&config, parse_close_window(&args)?),
        Some("list") => cmd_list(),
        Some("refresh") => cmd_refresh(&config),
        Some("event") => cmd_event(&config, args.get(2).ok_or("missing event")?),
        Some("remove") => cmd_remove(args.get(2).ok_or("missing workspace id or all")?),
        Some("-h") | Some("--help") | None => {
            print_help();
            Ok(())
        }
        Some(other) => Err(format!("unknown command: {other}").into()),
    }
}

fn print_help() {
    println!("ylam");
    println!("  ylam new [--agent claude|codex]");
    println!("  ylam close");
    println!("  ylam list");
    println!("  ylam refresh");
    println!("  ylam event <event>");
    println!("  ylam remove <wtN|all>");
}

fn parse_agent(args: &[String]) -> Result<Option<String>> {
    let mut agent = None;
    let mut i = 2;
    while i < args.len() {
        if args[i] == "--agent" {
            agent = Some(args.get(i + 1).ok_or("missing value for --agent")?.clone());
            i += 2;
        } else {
            return Err(format!("unknown option: {}", args[i]).into());
        }
    }
    Ok(agent)
}

fn parse_close_window(args: &[String]) -> Result<Option<String>> {
    let mut window = None;
    let mut i = 2;
    while i < args.len() {
        if args[i] == "--window" {
            window = Some(args.get(i + 1).ok_or("missing value for --window")?.clone());
            i += 2;
        } else {
            return Err(format!("unknown option: {}", args[i]).into());
        }
    }
    Ok(window)
}

fn cmd_new(config: &Config, agent: Option<String>) -> Result<()> {
    require_tmux()?;
    let repo = repo()?;
    let main_branch = resolve_main_branch(&repo.root, &config.main_branch)?;
    let mut state = load_state(&repo)?;
    if remove_missing_workspaces(&mut state) {
        prune_git_worktrees(&repo.root)?;
        save_state(&repo.state, &state)?;
    }
    let agent = agent.unwrap_or_else(|| config.default_agent.clone());
    let agent_cmd = config
        .agents
        .get(&agent)
        .ok_or_else(|| format!("unknown agent: {agent}"))?
        .clone();
    let live_windows = tmux_windows()?;
    let id = next_workspace_id(&state, &live_windows);
    let mut should_run_new_command = false;

    if !state.workspaces.contains_key(&id) {
        if id == "main" {
            let now = now();
            state.workspaces.insert(
                id.clone(),
                Workspace {
                    branch: main_branch.clone(),
                    path: repo.root.clone(),
                    agent: agent.clone(),
                    tmux_window_id: String::new(),
                    status: "created".into(),
                    created_at: now.clone(),
                    updated_at: now,
                },
            );
        } else {
            prepare_worktree(&repo, &main_branch, &id, &config.dotfiles)?;
            should_run_new_command = true;
            let now = now();
            state.workspaces.insert(
                id.clone(),
                Workspace {
                    branch: format!("{}-{}", id, clean_name(&repo.name)),
                    path: repo.parent.join(format!("{}-{}", repo.name, id)),
                    agent: agent.clone(),
                    tmux_window_id: String::new(),
                    status: "created".into(),
                    created_at: now.clone(),
                    updated_at: now,
                },
            );
        }
    }

    let workspace = state.workspaces.get_mut(&id).ok_or("workspace missing")?;
    if id != "main" && !workspace.path.exists() {
        prepare_worktree(&repo, &main_branch, &id, &config.dotfiles)?;
        should_run_new_command = true;
        workspace.branch = format!("{}-{}", id, clean_name(&repo.name));
        workspace.path = repo.parent.join(format!("{}-{}", repo.name, id));
    }

    let name = workspace_window_name(&id, &repo.name);
    let window = tmux_new(
        &name,
        &workspace_key(&repo.key, &id),
        &workspace.path,
        &config.editor,
        &agent_cmd,
        config.close_key.as_deref(),
    )?;
    if should_run_new_command {
        if let Some(command) = &config.new_command {
            start_new_command(&workspace.path, &window, &name, command)?;
        }
    }
    let now = now();
    workspace.agent = agent;
    workspace.tmux_window_id = window.clone();
    workspace.status = "running".into();
    workspace.updated_at = now;
    let path = workspace.path.clone();
    let branch = workspace.branch.clone();
    save_state(&repo.state, &state)?;
    println!("opened {id} {branch} {} {window}", path.display());
    Ok(())
}

fn cmd_list() -> Result<()> {
    let repos = data_root().join("repos");
    if !repos.exists() {
        return Ok(());
    }
    println!(
        "{:<18} {:<8} {:<24} {:<8} {:<18} {}",
        "Repo", "ID", "Branch", "Agent", "Status", "Window"
    );
    for entry in fs::read_dir(repos)? {
        let state_path = entry?.path().join("state.txt");
        if !state_path.exists() {
            continue;
        }
        let state = read_state_file(&state_path)?;
        for (id, w) in state.workspaces {
            println!(
                "{:<18} {:<8} {:<24} {:<8} {:<18} {}",
                state.repo_name, id, w.branch, w.agent, w.status, w.tmux_window_id
            );
        }
    }
    Ok(())
}

fn cmd_refresh(config: &Config) -> Result<()> {
    let repo = repo()?;
    let main_branch = resolve_main_branch(&repo.root, &config.main_branch)?;
    let state = read_state_file(&repo.state)?;
    for (id, w) in state.workspaces {
        if id == "main" {
            continue;
        }
        println!("refreshing {id}: {} <- {}", w.branch, main_branch);
        sh(Command::new("git")
            .arg("-C")
            .arg(&w.path)
            .arg("checkout")
            .arg(&w.branch))?;
        sh(Command::new("git")
            .arg("-C")
            .arg(&w.path)
            .arg("merge")
            .arg(&main_branch))?;
        sh(Command::new("git")
            .arg("-C")
            .arg(&w.path)
            .arg("push")
            .arg("-u")
            .arg("origin")
            .arg(&w.branch))?;
    }
    Ok(())
}

fn cmd_remove(target: &str) -> Result<()> {
    let repo = repo()?;
    let mut state = read_state_file(&repo.state)?;
    let ids: Vec<String> = if target == "all" {
        state.workspaces.keys().cloned().collect()
    } else if state.workspaces.contains_key(target) {
        vec![target.to_string()]
    } else {
        return Err(format!("unknown workspace: {target}").into());
    };

    for id in ids {
        let w = state.workspaces.remove(&id).ok_or("workspace missing")?;
        remove_worktree(&repo.root, &w)?;
        if id == "main" {
            println!("removed main from ylam state");
        } else {
            println!("removed {id} {}", w.path.display());
        }
    }

    save_state(&repo.state, &state)?;
    Ok(())
}

fn cmd_close(config: &Config, window: Option<String>) -> Result<()> {
    require_tmux()?;
    let window = match window {
        Some(window) => window,
        None => out(Command::new("tmux")
            .arg("display-message")
            .arg("-p")
            .arg("#{window_id}"))?,
    };
    let (state_path, mut state, id) =
        find_window(&window)?.ok_or_else(|| format!("no workspace for tmux window {window}"))?;
    let name = format!("{}:{}", id, state.repo_name);

    if id == "main" {
        let message = "the main Ylam window cannot be closed with ylam close";
        tmux_message(message)?;
        return Err(message.into());
    }

    let strategy = match CloseStrategy::parse(&config.close_strategy) {
        Ok(strategy) => strategy,
        Err(error) => {
            tmux_message(&format!("ylam close failed: {error}"))?;
            return Err(error);
        }
    };
    let workspace = state.workspaces.get(&id).ok_or("workspace missing")?;
    let branch = workspace.branch.clone();
    let worktree = workspace.path.clone();
    let preflight = (|| {
        let main_branch = resolve_main_branch(&state.repo_root, &config.main_branch)?;
        validate_close_worktrees(&state.repo_root, &main_branch, &worktree, &branch)?;
        Ok(main_branch)
    })();
    let main_branch = match preflight {
        Ok(main_branch) => main_branch,
        Err(error) => {
            let _ = tmux_message(&format!("ylam close failed: {error}"));
            return Err(error);
        }
    };

    let mut spinner = start_close_spinner(&window, &name)?;
    let result = (|| {
        integrate_workspace(strategy, &state.repo_root, &main_branch, &worktree, &branch)?;
        let workspace = state.workspaces.get(&id).ok_or("workspace missing")?;
        remove_worktree(&state.repo_root, workspace)?;
        state.workspaces.remove(&id);
        save_state(&state_path, &state)?;
        Ok(())
    })();
    stop_spinner(&mut spinner);

    if let Err(error) = result {
        let message = format!("ylam close failed: {error}");
        let _ = tmux_rename(&window, &format!("{name}:CLOSEFAIL"));
        let _ = tmux_message(&message);
        return Err(error);
    }

    sh(Command::new("tmux")
        .arg("kill-window")
        .arg("-t")
        .arg(&window))?;
    println!("closed {id} after integrating {branch} into {main_branch}");
    Ok(())
}

fn validate_close_worktrees(
    repo: &Path,
    main_branch: &str,
    worktree: &Path,
    worker_branch: &str,
) -> Result<()> {
    let checked_out_main = out(Command::new("git")
        .arg("-C")
        .arg(repo)
        .arg("branch")
        .arg("--show-current"))?;
    if checked_out_main != main_branch {
        return Err(format!(
            "main worktree has {checked_out_main} checked out; expected {main_branch}"
        )
        .into());
    }

    let checked_out_worker = out(Command::new("git")
        .arg("-C")
        .arg(worktree)
        .arg("branch")
        .arg("--show-current"))?;
    if checked_out_worker != worker_branch {
        return Err(format!(
            "worker worktree has {checked_out_worker} checked out; expected {worker_branch}"
        )
        .into());
    }

    ensure_no_tracked_changes(repo, "main")?;
    ensure_no_tracked_changes(worktree, "worker")?;
    Ok(())
}

fn ensure_no_tracked_changes(repo: &Path, label: &str) -> Result<()> {
    if git_diff_has_changes(repo, false)? || git_diff_has_changes(repo, true)? {
        return Err(format!("{label} worktree has uncommitted tracked changes").into());
    }
    Ok(())
}

fn git_diff_has_changes(repo: &Path, cached: bool) -> Result<bool> {
    let mut command = Command::new("git");
    command.arg("-C").arg(repo).arg("diff");
    if cached {
        command.arg("--cached");
    }
    let output = command.arg("--quiet").output()?;
    match output.status.code() {
        Some(0) => Ok(false),
        Some(1) => Ok(true),
        _ => Err(String::from_utf8_lossy(&output.stderr).trim().into()),
    }
}

fn integrate_workspace(
    strategy: CloseStrategy,
    repo: &Path,
    main_branch: &str,
    worktree: &Path,
    worker_branch: &str,
) -> Result<()> {
    match strategy {
        CloseStrategy::Merge => merge_workspace(repo, worker_branch),
        CloseStrategy::Rebase => rebase_workspace(repo, main_branch, worktree, worker_branch),
        CloseStrategy::Squash => squash_workspace(repo, worker_branch),
        CloseStrategy::PrAdminMerge => {
            pr_admin_merge_workspace(repo, main_branch, worktree, worker_branch)
        }
    }
}

fn merge_workspace(repo: &Path, worker_branch: &str) -> Result<()> {
    let result = sh(Command::new("git")
        .arg("-C")
        .arg(repo)
        .arg("merge")
        .arg("--no-edit")
        .arg(worker_branch));
    if result.is_err() {
        let _ = sh(Command::new("git")
            .arg("-C")
            .arg(repo)
            .arg("merge")
            .arg("--abort"));
    }
    result
}

fn rebase_workspace(
    repo: &Path,
    main_branch: &str,
    worktree: &Path,
    worker_branch: &str,
) -> Result<()> {
    let result = sh(Command::new("git")
        .arg("-C")
        .arg(worktree)
        .arg("rebase")
        .arg(main_branch));
    if let Err(error) = result {
        let _ = sh(Command::new("git")
            .arg("-C")
            .arg(worktree)
            .arg("rebase")
            .arg("--abort"));
        return Err(error);
    }

    sh(Command::new("git")
        .arg("-C")
        .arg(repo)
        .arg("merge")
        .arg("--ff-only")
        .arg(worker_branch))
}

fn squash_workspace(repo: &Path, worker_branch: &str) -> Result<()> {
    let squash = sh(Command::new("git")
        .arg("-C")
        .arg(repo)
        .arg("merge")
        .arg("--squash")
        .arg(worker_branch));
    if let Err(error) = squash {
        rollback_squash(repo);
        return Err(error);
    }
    if !git_diff_has_changes(repo, true)? {
        return Ok(());
    }

    let commit = sh(Command::new("git")
        .arg("-C")
        .arg(repo)
        .arg("commit")
        .arg("-m")
        .arg(format!("Squash merge {worker_branch}")));
    if commit.is_err() {
        rollback_squash(repo);
    }
    commit
}

fn rollback_squash(repo: &Path) {
    let _ = sh(Command::new("git")
        .arg("-C")
        .arg(repo)
        .arg("reset")
        .arg("--merge")
        .arg("HEAD"));
}

fn pr_admin_merge_workspace(
    repo: &Path,
    main_branch: &str,
    worktree: &Path,
    worker_branch: &str,
) -> Result<()> {
    sh(Command::new("git")
        .arg("-C")
        .arg(worktree)
        .arg("push")
        .arg("-u")
        .arg("origin")
        .arg(worker_branch))?;

    let open_prs = out(Command::new("gh")
        .current_dir(repo)
        .arg("pr")
        .arg("list")
        .arg("--state")
        .arg("open")
        .arg("--base")
        .arg(main_branch)
        .arg("--head")
        .arg(worker_branch)
        .arg("--json")
        .arg("number")
        .arg("--jq")
        .arg("length"))?;
    if open_prs == "0" {
        sh(Command::new("gh")
            .current_dir(worktree)
            .arg("pr")
            .arg("create")
            .arg("--base")
            .arg(main_branch)
            .arg("--head")
            .arg(worker_branch)
            .arg("--fill"))?;
    }

    sh(Command::new("gh")
        .current_dir(repo)
        .arg("pr")
        .arg("merge")
        .arg(worker_branch)
        .arg("--admin")
        .arg("--merge"))?;
    sh(Command::new("git")
        .arg("-C")
        .arg(repo)
        .arg("fetch")
        .arg("origin")
        .arg(main_branch))?;
    sh(Command::new("git")
        .arg("-C")
        .arg(repo)
        .arg("merge")
        .arg("--ff-only")
        .arg(format!("origin/{main_branch}")))
}

fn start_close_spinner(window: &str, name: &str) -> Result<Child> {
    let window = shell_quote(window);
    let name = shell_quote(name);
    let script = format!(
        r#"window={window}
name={name}
i=0
while :; do
  case $((i % 4)) in
    0) frame="◐";;
    1) frame="◓";;
    2) frame="◑";;
    *) frame="◒";;
  esac
  tmux rename-window -t "$window" "$name:CLOSE $frame" 2>/dev/null || exit 0
  i=$((i + 1))
  sleep 0.12
done
"#
    );
    Ok(Command::new("zsh")
        .arg("-lc")
        .arg(script)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()?)
}

fn stop_spinner(spinner: &mut Child) {
    let _ = spinner.kill();
    let _ = spinner.wait();
}

fn cmd_event(config: &Config, event: &str) -> Result<()> {
    require_tmux()?;
    let window = out(Command::new("tmux")
        .arg("display-message")
        .arg("-p")
        .arg("#{window_id}"))?;
    let (state_path, mut state, id) =
        find_window(&window)?.ok_or_else(|| format!("no workspace for tmux window {window}"))?;

    let repo_name = state.repo_name.clone();
    let w = state.workspaces.get_mut(&id).ok_or("workspace missing")?;
    w.status = event.into();
    w.updated_at = now();

    let marker = match event {
        "agent-done" | "done" => Some("DONE"),
        "permission-requested" | "attention" => Some("PERM"),
        "failed" | "fail" => Some("FAIL"),
        "running" => None,
        _ => Some("ATTN"),
    };
    let name = match marker {
        Some(m) => format!("{id}:{repo_name}:{m}"),
        None => format!("{id}:{repo_name}"),
    };

    if event == "running" {
        tmux_rename(&window, &name)?;
    } else {
        let color = match event {
            "agent-done" | "done" => &config.done_color,
            "permission-requested" | "attention" => &config.attention_color,
            "failed" | "fail" => &config.failed_color,
            _ => &config.attention_color,
        };
        tmux_color(&window, color, &name)?;
    }
    save_state(&state_path, &state)?;
    println!("updated {id} to {event}");
    Ok(())
}

fn load_config() -> Config {
    let mut c = Config {
        default_agent: "claude".into(),
        editor: "nvim".into(),
        main_branch: "main".into(),
        new_command: None,
        close_strategy: "merge".into(),
        close_key: Some("q".into()),
        dotfiles: vec![
            ".env".into(),
            ".envrc".into(),
            ".claude".into(),
            ".codex".into(),
        ],
        done_color: "brightyellow".into(),
        attention_color: "yellow".into(),
        failed_color: "red".into(),
        agents: BTreeMap::from([
            ("claude".into(), "claude".into()),
            ("codex".into(), "codex".into()),
        ]),
    };

    let path = home().join(".config/ylam/config.toml");
    let Ok(text) = fs::read_to_string(path) else {
        return c;
    };

    let mut section = String::new();
    for raw in text.lines() {
        let line = raw.split('#').next().unwrap_or("").trim();
        if line.is_empty() {
            continue;
        }
        if line.starts_with('[') && line.ends_with(']') {
            section = line.trim_matches(&['[', ']'][..]).to_string();
            continue;
        }
        let Some((key, value)) = line.split_once('=') else {
            continue;
        };
        let key = key.trim();
        let value = value.trim();
        match (section.as_str(), key) {
            ("", "default_agent") => c.default_agent = unquote(value),
            ("", "editor") => c.editor = unquote(value),
            ("", "main_branch") => c.main_branch = unquote(value),
            ("", "new_command") => {
                let command = unquote(value);
                c.new_command = (!command.is_empty()).then_some(command);
            }
            ("", "close_strategy") => c.close_strategy = unquote(value),
            ("dotfiles", "paths") => c.dotfiles = parse_array(value),
            ("tmux", "close_key") => {
                let key = unquote(value);
                c.close_key = (!key.is_empty()).then_some(key);
            }
            ("tmux", "done_color") => c.done_color = unquote(value),
            ("tmux", "attention_color") => c.attention_color = unquote(value),
            ("tmux", "failed_color") => c.failed_color = unquote(value),
            (s, "command") if s.starts_with("agents.") => {
                c.agents
                    .insert(s.trim_start_matches("agents.").into(), unquote(value));
            }
            _ => {}
        }
    }
    c
}

fn repo() -> Result<Repo> {
    let root = main_worktree_root(&env::current_dir()?)?;
    let name = root
        .file_name()
        .and_then(OsStr::to_str)
        .ok_or("repo has no directory name")?
        .to_string();
    let key = format!(
        "{}-{:x}",
        clean_name(&name),
        hash(root.to_string_lossy().as_ref())
    );
    let base = data_root().join("repos").join(&key);
    let parent = root
        .parent()
        .ok_or("repo root has no parent directory")?
        .to_path_buf();
    Ok(Repo {
        name,
        root,
        key,
        state: base.join("state.txt"),
        parent,
    })
}

// `git rev-parse --show-toplevel` returns the *current* worktree, so running ylam
// from inside a worktree would register that worktree as a separate repo. The
// common git dir always points at the main worktree's .git, for every worktree.
fn main_worktree_root(dir: &Path) -> Result<PathBuf> {
    let common = PathBuf::from(out(Command::new("git")
        .current_dir(dir)
        .arg("rev-parse")
        .arg("--path-format=absolute")
        .arg("--git-common-dir"))?);
    let root = match common.file_name() {
        Some(name) if name == OsStr::new(".git") => common
            .parent()
            .ok_or("git common dir has no parent directory")?
            .to_path_buf(),
        // Bare repo or a separate git dir: no main worktree to derive, use the current one.
        _ => PathBuf::from(out(Command::new("git")
            .current_dir(dir)
            .arg("rev-parse")
            .arg("--show-toplevel"))?),
    };
    Ok(root.canonicalize()?)
}

fn load_state(repo: &Repo) -> Result<State> {
    if repo.state.exists() {
        read_state_file(&repo.state)
    } else {
        Ok(State {
            repo_name: repo.name.clone(),
            repo_root: repo.root.clone(),
            repo_key: repo.key.clone(),
            worktree_root: repo.parent.clone(),
            workspaces: BTreeMap::new(),
        })
    }
}

fn read_state_file(path: &Path) -> Result<State> {
    let mut state = State {
        repo_name: String::new(),
        repo_root: PathBuf::new(),
        repo_key: String::new(),
        worktree_root: PathBuf::new(),
        workspaces: BTreeMap::new(),
    };
    for line in fs::read_to_string(path)?.lines() {
        if let Some((k, v)) = line.split_once('=') {
            match k {
                "repo_name" => state.repo_name = v.into(),
                "repo_root" => state.repo_root = v.into(),
                "repo_key" => state.repo_key = v.into(),
                "worktree_root" => state.worktree_root = v.into(),
                _ => {}
            }
        } else if let Some(rest) = line.strip_prefix("workspace\t") {
            let p: Vec<&str> = rest.split('\t').collect();
            if p.len() == 8 {
                state.workspaces.insert(
                    p[0].into(),
                    Workspace {
                        branch: p[1].into(),
                        path: p[2].into(),
                        agent: p[3].into(),
                        tmux_window_id: p[4].into(),
                        status: p[5].into(),
                        created_at: p[6].into(),
                        updated_at: p[7].into(),
                    },
                );
            }
        }
    }
    Ok(state)
}

fn save_state(path: &Path, state: &State) -> Result<()> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    let mut s = String::new();
    s.push_str(&format!("repo_name={}\n", state.repo_name));
    s.push_str(&format!("repo_root={}\n", state.repo_root.display()));
    s.push_str(&format!("repo_key={}\n", state.repo_key));
    s.push_str(&format!(
        "worktree_root={}\n",
        state.worktree_root.display()
    ));
    for (id, w) in &state.workspaces {
        s.push_str(&format!(
            "workspace\t{}\t{}\t{}\t{}\t{}\t{}\t{}\t{}\n",
            id,
            w.branch,
            w.path.display(),
            w.agent,
            w.tmux_window_id,
            w.status,
            w.created_at,
            w.updated_at
        ));
    }
    fs::write(path, s)?;
    Ok(())
}

fn next_workspace_id(state: &State, live_windows: &[TmuxWindow]) -> String {
    if !workspace_is_open(state, "main", live_windows) {
        return "main".into();
    }

    for n in 1.. {
        let id = format!("wt{n}");
        if !workspace_is_open(state, &id, live_windows) {
            return id;
        }
    }
    unreachable!()
}

fn workspace_key(repo_key: &str, id: &str) -> String {
    format!("{repo_key}:{id}")
}

fn workspace_window_name(id: &str, repo_name: &str) -> String {
    format!("{id}:{repo_name}")
}

// A stored tmux window id is not proof the workspace is still open: tmux restarts
// its @N counter with the server, so an unrelated window can inherit the id. Also
// require the window to identify itself as this workspace, via the @ylam_workspace
// option, or via its name for windows opened by an older ylam.
fn workspace_is_open(state: &State, id: &str, live_windows: &[TmuxWindow]) -> bool {
    let Some(workspace) = state.workspaces.get(id) else {
        return false;
    };
    if workspace.tmux_window_id.is_empty() {
        return false;
    }
    let key = workspace_key(&state.repo_key, id);
    let name = workspace_window_name(id, &state.repo_name);
    live_windows.iter().any(|window| {
        window.id == workspace.tmux_window_id
            && if window.workspace_key.is_empty() {
                // ylam appends a status marker as `name:MARKER` in cmd_event.
                window.name == name || window.name.starts_with(&format!("{name}:"))
            } else {
                window.workspace_key == key
            }
    })
}

fn remove_missing_workspaces(state: &mut State) -> bool {
    let before = state.workspaces.len();
    state
        .workspaces
        .retain(|id, workspace| id == "main" || workspace.path.exists());
    state.workspaces.len() != before
}

fn prune_git_worktrees(repo: &Path) -> Result<()> {
    sh(Command::new("git")
        .arg("-C")
        .arg(repo)
        .arg("worktree")
        .arg("prune"))
}

fn branch_exists(repo: &Path, branch: &str) -> Result<bool> {
    Ok(Command::new("git")
        .arg("-C")
        .arg(repo)
        .arg("show-ref")
        .arg("--verify")
        .arg("--quiet")
        .arg(format!("refs/heads/{branch}"))
        .status()?
        .success())
}

fn resolve_main_branch(repo: &Path, configured: &str) -> Result<String> {
    if configured == "main" || configured == "master" {
        if branch_exists(repo, "main")? {
            return Ok("main".into());
        }
        if branch_exists(repo, "master")? {
            return Ok("master".into());
        }
        return Err("neither main nor master branch exists".into());
    }

    if branch_exists(repo, configured)? {
        return Ok(configured.into());
    }

    Err(format!("configured main_branch does not exist: {configured}").into())
}

fn create_worktree(repo: &Repo, main_branch: &str, id: &str) -> Result<()> {
    let branch = format!("{}-{}", id, clean_name(&repo.name));
    let path = repo.parent.join(format!("{}-{}", repo.name, id));
    if path.exists() {
        return Err(format!("worktree already exists: {}", path.display()).into());
    }

    if branch_exists(&repo.root, &branch)? {
        sh(Command::new("git")
            .arg("-C")
            .arg(&repo.root)
            .arg("worktree")
            .arg("add")
            .arg(&path)
            .arg(&branch))
    } else {
        sh(Command::new("git")
            .arg("-C")
            .arg(&repo.root)
            .arg("worktree")
            .arg("add")
            .arg("-b")
            .arg(&branch)
            .arg(&path)
            .arg(main_branch))
    }
}

fn prepare_worktree(repo: &Repo, main_branch: &str, id: &str, dotfiles: &[String]) -> Result<()> {
    let path = repo.parent.join(format!("{}-{}", repo.name, id));
    create_worktree(repo, main_branch, id)?;
    copy_dotfiles(&repo.root, &path, dotfiles)?;
    copy_justfiles(&repo.root, &path)?;
    Ok(())
}

fn copy_justfiles(repo: &Path, worktree: &Path) -> Result<()> {
    copy_existing(repo, worktree, "justfile")?;
    copy_existing(repo, worktree, "Justfile")?;
    Ok(())
}

fn copy_existing(repo: &Path, worktree: &Path, name: &str) -> Result<()> {
    let src = repo.join(name);
    if src.exists() {
        copy(&src, &worktree.join(name))?;
    }
    Ok(())
}

fn start_new_command(worktree: &Path, window: &str, name: &str, command: &str) -> Result<()> {
    let log = shell_quote(&worktree.join(".ylam-new-command.log").to_string_lossy());
    let worktree = shell_quote(&worktree.to_string_lossy());
    let window = shell_quote(window);
    let name = shell_quote(name);
    let command = shell_quote(command);
    let script = format!(
        r#"worktree={worktree}
log={log}
window={window}
name={name}
command={command}
cd "$worktree" || exit 1
(
  i=0
  while :; do
    case $((i % 10)) in
      0) frame="⠋";;
      1) frame="⠙";;
      2) frame="⠹";;
      3) frame="⠸";;
      4) frame="⠼";;
      5) frame="⠴";;
      6) frame="⠦";;
      7) frame="⠧";;
      8) frame="⠇";;
      *) frame="⠏";;
    esac
    tmux rename-window -t "$window" "$name:BOOT $frame" 2>/dev/null || exit 0
    i=$((i + 1))
    sleep 0.08
  done
) &
spinner=$!
zsh -lc "$command" > "$log" 2>&1
status=$?
kill "$spinner" 2>/dev/null
wait "$spinner" 2>/dev/null
if [ "$status" -eq 0 ]; then
  tmux rename-window -t "$window" "$name" 2>/dev/null
else
  tmux rename-window -t "$window" "$name:BOOTFAIL" 2>/dev/null
fi
"#
    );

    Command::new("zsh")
        .arg("-lc")
        .arg(script)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()?;
    println!(
        "running configured new command in {}",
        worktree.trim_matches('\'')
    );
    Ok(())
}

fn remove_worktree(repo: &Path, w: &Workspace) -> Result<()> {
    if w.path == repo {
        return Ok(());
    }
    if w.path.exists() {
        sh(Command::new("git")
            .arg("-C")
            .arg(repo)
            .arg("worktree")
            .arg("remove")
            .arg("--force")
            .arg(&w.path))?;
    }
    if branch_exists(repo, &w.branch)? {
        sh(Command::new("git")
            .arg("-C")
            .arg(repo)
            .arg("branch")
            .arg("-D")
            .arg(&w.branch))?;
    }
    Ok(())
}

fn copy_dotfiles(repo: &Path, worktree: &Path, paths: &[String]) -> Result<()> {
    for name in paths {
        let src = repo.join(name);
        let dst = worktree.join(name);
        if !src.exists() {
            continue;
        }
        if dst.exists() {
            if dst.is_dir() {
                fs::remove_dir_all(&dst)?;
            } else {
                fs::remove_file(&dst)?;
            }
        }
        copy(&src, &dst)?;
    }
    Ok(())
}

fn copy(src: &Path, dst: &Path) -> Result<()> {
    if src.is_dir() {
        fs::create_dir_all(dst)?;
        for entry in fs::read_dir(src)? {
            let entry = entry?;
            copy(&entry.path(), &dst.join(entry.file_name()))?;
        }
    } else {
        if let Some(parent) = dst.parent() {
            fs::create_dir_all(parent)?;
        }
        fs::copy(src, dst)?;
    }
    Ok(())
}

fn tmux_new(
    name: &str,
    workspace_key: &str,
    dir: &Path,
    editor: &str,
    agent: &str,
    close_key: Option<&str>,
) -> Result<String> {
    let window = out(Command::new("tmux")
        .arg("new-window")
        .arg("-P")
        .arg("-F")
        .arg("#{window_id}")
        .arg("-n")
        .arg(name)
        .arg("-c")
        .arg(dir))?;
    sh(Command::new("tmux")
        .arg("set-option")
        .arg("-w")
        .arg("-t")
        .arg(&window)
        .arg("@ylam_workspace")
        .arg(workspace_key))?;
    configure_close_shortcut(&window, close_key)?;
    let left = out(Command::new("tmux")
        .arg("list-panes")
        .arg("-t")
        .arg(&window)
        .arg("-F")
        .arg("#{pane_id}"))?;
    let right = out(Command::new("tmux")
        .arg("split-window")
        .arg("-h")
        .arg("-P")
        .arg("-F")
        .arg("#{pane_id}")
        .arg("-t")
        .arg(&window)
        .arg("-c")
        .arg(dir))?;
    let dir = shell_quote(&dir.to_string_lossy());
    sh(Command::new("tmux")
        .arg("send-keys")
        .arg("-t")
        .arg(&left)
        .arg(format!("cd {dir} && {editor} ."))
        .arg("C-m"))?;
    sh(Command::new("tmux")
        .arg("send-keys")
        .arg("-t")
        .arg(&right)
        .arg(format!("cd {dir} && {agent}"))
        .arg("C-m"))?;
    tmux_rename(&window, name)?;
    sh(Command::new("tmux").arg("select-pane").arg("-t").arg(left))?;
    sh(Command::new("tmux")
        .arg("select-window")
        .arg("-t")
        .arg(&window))?;
    Ok(window)
}

fn configure_close_shortcut(window: &str, close_key: Option<&str>) -> Result<()> {
    sh(Command::new("tmux")
        .arg("set-option")
        .arg("-w")
        .arg("-t")
        .arg(window)
        .arg("@ylam_tracked")
        .arg("1"))?;

    let Some(close_key) = close_key else {
        return Ok(());
    };
    let executable = env::current_exe()?;
    let close_command = format!(
        r##"if [ "#{{@ylam_tracked}}" = "1" ]; then
  {} close --window "#{{window_id}}"
else
  tmux display-panes
fi"##,
        shell_quote(&executable.to_string_lossy())
    );
    sh(Command::new("tmux")
        .arg("bind-key")
        .arg("-T")
        .arg("prefix")
        .arg(close_key)
        .arg("run-shell")
        .arg("-b")
        .arg(close_command))
}

fn tmux_color(window: &str, color: &str, name: &str) -> Result<()> {
    sh(Command::new("tmux")
        .arg("set-option")
        .arg("-w")
        .arg("-t")
        .arg(window)
        .arg("@ylam_status_color")
        .arg(color))?;
    sh(Command::new("tmux")
        .arg("set-option")
        .arg("-w")
        .arg("-t")
        .arg(window)
        .arg("window-status-style")
        .arg(format!("fg={color}")))?;
    sh(Command::new("tmux")
        .arg("set-option")
        .arg("-w")
        .arg("-t")
        .arg(window)
        .arg("window-status-current-style")
        .arg(format!("fg={color},bold")))?;
    tmux_rename(window, name)?;
    Ok(())
}

fn tmux_rename(window: &str, name: &str) -> Result<()> {
    sh(Command::new("tmux")
        .arg("rename-window")
        .arg("-t")
        .arg(window)
        .arg(name))?;
    Ok(())
}

fn tmux_message(message: &str) -> Result<()> {
    sh(Command::new("tmux").arg("display-message").arg(message))
}

fn tmux_windows() -> Result<Vec<TmuxWindow>> {
    Ok(out(Command::new("tmux")
        .arg("list-windows")
        .arg("-a")
        .arg("-F")
        .arg("#{window_id}\t#{@ylam_workspace}\t#{window_name}"))?
    .lines()
    .filter_map(|line| {
        let mut parts = line.splitn(3, '\t');
        Some(TmuxWindow {
            id: parts.next()?.to_string(),
            workspace_key: parts.next().unwrap_or_default().to_string(),
            name: parts.next().unwrap_or_default().to_string(),
        })
    })
    .collect())
}

fn find_window(window: &str) -> Result<Option<(PathBuf, State, String)>> {
    let repos = data_root().join("repos");
    if !repos.exists() {
        return Ok(None);
    }
    for entry in fs::read_dir(repos)? {
        let path = entry?.path().join("state.txt");
        if !path.exists() {
            continue;
        }
        let state = read_state_file(&path)?;
        let id = state
            .workspaces
            .iter()
            .find_map(|(id, w)| (w.tmux_window_id == window).then(|| id.clone()));
        if let Some(id) = id {
            return Ok(Some((path, state, id)));
        }
    }
    Ok(None)
}

fn require_tmux() -> Result<()> {
    if env::var_os("TMUX").is_none() {
        Err("ylam v0 expects to run inside tmux".into())
    } else {
        Ok(())
    }
}

fn sh(cmd: &mut Command) -> Result<()> {
    let output = cmd.output()?;
    if output.status.success() {
        Ok(())
    } else {
        Err(String::from_utf8_lossy(&output.stderr).trim().into())
    }
}

fn out(cmd: &mut Command) -> Result<String> {
    let output = cmd.output()?;
    if output.status.success() {
        Ok(String::from_utf8_lossy(&output.stdout).trim().into())
    } else {
        Err(String::from_utf8_lossy(&output.stderr).trim().into())
    }
}

fn data_root() -> PathBuf {
    home().join("Library/Application Support/ylam")
}

fn home() -> PathBuf {
    env::var_os("HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("."))
}

fn now() -> String {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
        .to_string()
}

fn clean_name(s: &str) -> String {
    s.chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == '-' || c == '_' || c == '.' {
                c
            } else {
                '-'
            }
        })
        .collect()
}

fn shell_quote(s: &str) -> String {
    format!("'{}'", s.replace('\'', "'\\''"))
}

fn hash(s: &str) -> u64 {
    let mut h = 0xcbf29ce484222325u64;
    for b in s.bytes() {
        h ^= b as u64;
        h = h.wrapping_mul(0x100000001b3);
    }
    h & 0xffffff
}

fn unquote(s: &str) -> String {
    s.trim().trim_matches('"').trim_matches('\'').to_string()
}

fn parse_array(s: &str) -> Vec<String> {
    s.trim_matches(&['[', ']'][..])
        .split(',')
        .map(unquote)
        .filter(|s| !s.is_empty())
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_repo(name: &str, initial_branch: &str) -> PathBuf {
        let unique = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_nanos();
        let path = env::temp_dir().join(format!("ylam-test-{name}-{unique}"));
        fs::create_dir_all(&path).unwrap();
        sh(Command::new("git")
            .arg("init")
            .arg("-b")
            .arg(initial_branch)
            .arg(&path))
        .unwrap();
        sh(Command::new("git")
            .arg("-C")
            .arg(&path)
            .arg("config")
            .arg("user.name")
            .arg("Ylam Test"))
        .unwrap();
        sh(Command::new("git")
            .arg("-C")
            .arg(&path)
            .arg("config")
            .arg("user.email")
            .arg("ylam-test@example.com"))
        .unwrap();
        fs::write(path.join("README.md"), "test\n").unwrap();
        sh(Command::new("git").arg("-C").arg(&path).arg("add").arg(".")).unwrap();
        sh(Command::new("git")
            .arg("-C")
            .arg(&path)
            .arg("commit")
            .arg("-m")
            .arg("initial"))
        .unwrap();
        path
    }

    fn test_workspace(id: &str, path: PathBuf) -> Workspace {
        Workspace {
            branch: format!("{id}-repo"),
            path,
            agent: "claude".into(),
            tmux_window_id: String::new(),
            status: "created".into(),
            created_at: "0".into(),
            updated_at: "0".into(),
        }
    }

    fn test_open_workspace(id: &str, path: PathBuf, window: &str) -> Workspace {
        let mut workspace = test_workspace(id, path);
        workspace.tmux_window_id = window.into();
        workspace
    }

    // A window ylam opened for `id` in the test repo below.
    fn ylam_window(id: &str, window: &str) -> TmuxWindow {
        TmuxWindow {
            id: window.into(),
            workspace_key: workspace_key("repo-key", id),
            name: workspace_window_name(id, "repo"),
        }
    }

    // A window ylam did not open that happens to hold `window`.
    fn foreign_window(name: &str, window: &str) -> TmuxWindow {
        TmuxWindow {
            id: window.into(),
            workspace_key: String::new(),
            name: name.into(),
        }
    }

    fn test_state() -> State {
        State {
            repo_name: "repo".into(),
            repo_root: PathBuf::from("/tmp/repo"),
            repo_key: "repo-key".into(),
            worktree_root: PathBuf::from("/tmp"),
            workspaces: BTreeMap::new(),
        }
    }

    #[test]
    fn next_workspace_id_uses_next_unused_worker_id() {
        let mut state = test_state();

        state.workspaces.insert(
            "main".into(),
            test_open_workspace("main", PathBuf::from("/tmp/repo"), "@1"),
        );
        state.workspaces.insert(
            "wt1".into(),
            test_open_workspace("wt1", PathBuf::from("/tmp/repo-wt1"), "@2"),
        );
        state.workspaces.insert(
            "wt2".into(),
            test_open_workspace("wt2", PathBuf::from("/tmp/repo-wt2"), "@3"),
        );

        assert_eq!(
            next_workspace_id(
                &state,
                &[
                    ylam_window("main", "@1"),
                    ylam_window("wt1", "@2"),
                    ylam_window("wt2", "@3"),
                ]
            ),
            "wt3"
        );
    }

    #[test]
    fn next_workspace_id_reopens_first_closed_workspace() {
        let mut state = test_state();

        state.workspaces.insert(
            "main".into(),
            test_open_workspace("main", PathBuf::from("/tmp/repo"), "@1"),
        );
        state.workspaces.insert(
            "wt1".into(),
            test_open_workspace("wt1", PathBuf::from("/tmp/repo-wt1"), "@2"),
        );
        state.workspaces.insert(
            "wt2".into(),
            test_open_workspace("wt2", PathBuf::from("/tmp/repo-wt2"), "@3"),
        );

        assert_eq!(
            next_workspace_id(
                &state,
                &[ylam_window("main", "@1"), ylam_window("wt2", "@3")]
            ),
            "wt1"
        );
    }

    #[test]
    fn next_workspace_id_reopens_main_before_workers() {
        let mut state = test_state();

        state.workspaces.insert(
            "main".into(),
            test_open_workspace("main", PathBuf::from("/tmp/repo"), "@1"),
        );
        state.workspaces.insert(
            "wt1".into(),
            test_open_workspace("wt1", PathBuf::from("/tmp/repo-wt1"), "@2"),
        );

        assert_eq!(
            next_workspace_id(&state, &[ylam_window("wt1", "@2")]),
            "main"
        );
    }

    #[test]
    fn next_workspace_id_ignores_foreign_window_reusing_a_stored_id() {
        let mut state = test_state();

        state.workspaces.insert(
            "main".into(),
            test_open_workspace("main", PathBuf::from("/tmp/repo"), "@1"),
        );

        assert_eq!(
            next_workspace_id(&state, &[foreign_window("lazygit:repo", "@1")]),
            "main"
        );
    }

    #[test]
    fn next_workspace_id_accepts_windows_opened_by_an_older_ylam() {
        let mut state = test_state();

        state.workspaces.insert(
            "main".into(),
            test_open_workspace("main", PathBuf::from("/tmp/repo"), "@1"),
        );

        // No @ylam_workspace option, and cmd_event has appended a status marker.
        assert_eq!(
            next_workspace_id(&state, &[foreign_window("main:repo:DONE", "@1")]),
            "wt1"
        );
    }

    #[test]
    fn remove_missing_workspaces_keeps_main_and_drops_deleted_workers() {
        let root = env::temp_dir().join(format!("ylam-state-test-{}", now()));
        let existing_worker = root.join("repo-wt2");
        fs::create_dir_all(&existing_worker).unwrap();

        let mut state = State {
            repo_name: "repo".into(),
            repo_root: root.join("repo"),
            repo_key: "repo-key".into(),
            worktree_root: root.clone(),
            workspaces: BTreeMap::new(),
        };

        state.workspaces.insert(
            "main".into(),
            test_open_workspace("main", root.join("repo-missing-but-main"), "@1"),
        );
        state.workspaces.insert(
            "wt1".into(),
            test_workspace("wt1", root.join("repo-wt1-missing")),
        );
        state.workspaces.insert(
            "wt2".into(),
            test_open_workspace("wt2", existing_worker.clone(), "@2"),
        );

        assert!(remove_missing_workspaces(&mut state));
        assert!(state.workspaces.contains_key("main"));
        assert!(!state.workspaces.contains_key("wt1"));
        assert!(state.workspaces.contains_key("wt2"));
        assert_eq!(
            next_workspace_id(
                &state,
                &[ylam_window("main", "@1"), ylam_window("wt2", "@2")]
            ),
            "wt1"
        );
    }

    #[test]
    fn resolve_main_branch_uses_main_when_both_main_and_master_exist() {
        let repo = temp_repo("both", "main");
        sh(Command::new("git")
            .arg("-C")
            .arg(&repo)
            .arg("branch")
            .arg("master"))
        .unwrap();

        assert_eq!(resolve_main_branch(&repo, "main").unwrap(), "main");
        assert_eq!(resolve_main_branch(&repo, "master").unwrap(), "main");
    }

    #[test]
    fn resolve_main_branch_falls_back_to_master() {
        let repo = temp_repo("master", "master");

        assert_eq!(resolve_main_branch(&repo, "main").unwrap(), "master");
    }

    #[test]
    fn resolve_main_branch_keeps_custom_configured_branch() {
        let repo = temp_repo("custom", "develop");

        assert_eq!(resolve_main_branch(&repo, "develop").unwrap(), "develop");
    }

    #[test]
    fn main_worktree_root_resolves_from_inside_a_worktree() {
        let root = temp_repo("worktree-root", "main").canonicalize().unwrap();
        let worktree = root.parent().unwrap().join(format!(
            "{}-wt1",
            root.file_name().unwrap().to_string_lossy()
        ));
        sh(Command::new("git")
            .arg("-C")
            .arg(&root)
            .arg("worktree")
            .arg("add")
            .arg("-b")
            .arg("wt1")
            .arg(&worktree))
        .unwrap();

        assert_eq!(main_worktree_root(&worktree).unwrap(), root);
        assert_eq!(main_worktree_root(&root).unwrap(), root);
        assert_eq!(main_worktree_root(&root.join(".git")).unwrap(), root);
    }

    #[test]
    fn prepare_worktree_copies_justfile_and_dotfiles() {
        let root = temp_repo("bootstrap", "main");
        fs::write(
            root.join("justfile"),
            "bootstrap:\n\tcp .env bootstrap.env\n",
        )
        .unwrap();
        fs::write(root.join(".env"), "TOKEN=test\n").unwrap();

        let repo = Repo {
            name: root.file_name().unwrap().to_string_lossy().into_owned(),
            root: root.clone(),
            key: "test".into(),
            state: root.join(".state"),
            parent: root.parent().unwrap().to_path_buf(),
        };

        prepare_worktree(&repo, "main", "wt1", &[".env".into()]).unwrap();

        let worktree = repo.parent.join(format!("{}-wt1", repo.name));
        assert!(worktree.join("justfile").exists());
        assert_eq!(
            fs::read_to_string(worktree.join(".env")).unwrap(),
            "TOKEN=test\n"
        );
    }
}
