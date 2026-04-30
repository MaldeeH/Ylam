use std::collections::BTreeMap;
use std::env;
use std::error::Error;
use std::ffi::OsStr;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::{SystemTime, UNIX_EPOCH};

type Result<T> = std::result::Result<T, Box<dyn Error>>;

struct Config {
    default_agent: String,
    editor: String,
    main_branch: String,
    dotfiles: Vec<String>,
    done_color: String,
    attention_color: String,
    running_color: String,
    failed_color: String,
    agents: BTreeMap<String, String>,
}

struct Repo {
    name: String,
    root: PathBuf,
    key: String,
    state: PathBuf,
    worktrees: PathBuf,
}

struct State {
    repo_name: String,
    repo_root: PathBuf,
    repo_key: String,
    worktree_root: PathBuf,
    workspaces: BTreeMap<String, Workspace>,
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
        Some("list") => cmd_list(),
        Some("refresh") => cmd_refresh(&config),
        Some("event") => cmd_event(&config, args.get(2).ok_or("missing event")?),
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
    println!("  ylam list");
    println!("  ylam refresh");
    println!("  ylam event <event>");
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

fn cmd_new(config: &Config, agent: Option<String>) -> Result<()> {
    require_tmux()?;
    let repo = repo()?;
    let mut state = load_state(&repo)?;
    let id = next_id(&state);
    let branch = format!("{}-{}", id, clean_name(&repo.name));
    let path = repo.worktrees.join(&id);
    let agent = agent.unwrap_or_else(|| config.default_agent.clone());
    let agent_cmd = config
        .agents
        .get(&agent)
        .ok_or_else(|| format!("unknown agent: {agent}"))?
        .clone();

    if branch_exists(&repo.root, &branch)? {
        return Err(format!("branch already exists: {branch}").into());
    }
    if path.exists() {
        return Err(format!("worktree already exists: {}", path.display()).into());
    }

    fs::create_dir_all(&repo.worktrees)?;
    sh(Command::new("git")
        .arg("-C")
        .arg(&repo.root)
        .arg("worktree")
        .arg("add")
        .arg("-b")
        .arg(&branch)
        .arg(&path)
        .arg(&config.main_branch))?;
    copy_dotfiles(&repo.root, &path, &config.dotfiles)?;

    let name = format!("{}:{}", id, repo.name);
    let window = tmux_new(
        &name,
        &path,
        &config.editor,
        &agent_cmd,
        &config.running_color,
    )?;
    let now = now();
    state.workspaces.insert(
        id.clone(),
        Workspace {
            branch: branch.clone(),
            path: path.clone(),
            agent,
            tmux_window_id: window.clone(),
            status: "running".into(),
            created_at: now.clone(),
            updated_at: now,
        },
    );
    save_state(&repo.state, &state)?;
    println!("created {id} {branch} {} {window}", path.display());
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
    let state = read_state_file(&repo.state)?;
    for (id, w) in state.workspaces {
        println!("refreshing {id}: {} <- {}", w.branch, config.main_branch);
        sh(Command::new("git")
            .arg("-C")
            .arg(&w.path)
            .arg("checkout")
            .arg(&w.branch))?;
        sh(Command::new("git")
            .arg("-C")
            .arg(&w.path)
            .arg("merge")
            .arg(&config.main_branch))?;
    }
    Ok(())
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
    let color = match event {
        "agent-done" | "done" => &config.done_color,
        "permission-requested" | "attention" => &config.attention_color,
        "failed" | "fail" => &config.failed_color,
        "running" => &config.running_color,
        _ => &config.attention_color,
    };
    let name = match marker {
        Some(m) => format!("{id}:{repo_name}:{m}"),
        None => format!("{id}:{repo_name}"),
    };

    tmux_color(&window, color, &name)?;
    save_state(&state_path, &state)?;
    println!("updated {id} to {event}");
    Ok(())
}

fn load_config() -> Config {
    let mut c = Config {
        default_agent: "claude".into(),
        editor: "nvim".into(),
        main_branch: "main".into(),
        dotfiles: vec![
            ".env".into(),
            ".envrc".into(),
            ".claude".into(),
            ".codex".into(),
        ],
        done_color: "green".into(),
        attention_color: "yellow".into(),
        running_color: "cyan".into(),
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
            ("dotfiles", "paths") => c.dotfiles = parse_array(value),
            ("tmux", "done_color") => c.done_color = unquote(value),
            ("tmux", "attention_color") => c.attention_color = unquote(value),
            ("tmux", "running_color") => c.running_color = unquote(value),
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
    let root = PathBuf::from(out(Command::new("git")
        .arg("rev-parse")
        .arg("--show-toplevel"))?)
    .canonicalize()?;
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
    Ok(Repo {
        name,
        root,
        key,
        state: base.join("state.txt"),
        worktrees: base.join("worktrees"),
    })
}

fn load_state(repo: &Repo) -> Result<State> {
    if repo.state.exists() {
        read_state_file(&repo.state)
    } else {
        Ok(State {
            repo_name: repo.name.clone(),
            repo_root: repo.root.clone(),
            repo_key: repo.key.clone(),
            worktree_root: repo.worktrees.clone(),
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

fn next_id(state: &State) -> String {
    for n in 1.. {
        let id = format!("wt{n}");
        if !state.workspaces.contains_key(&id) {
            return id;
        }
    }
    unreachable!()
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

fn tmux_new(name: &str, dir: &Path, editor: &str, agent: &str, color: &str) -> Result<String> {
    let window = out(Command::new("tmux")
        .arg("new-window")
        .arg("-P")
        .arg("-F")
        .arg("#{window_id}")
        .arg("-n")
        .arg(name)
        .arg("-c")
        .arg(dir))?;
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
    sh(Command::new("tmux")
        .arg("send-keys")
        .arg("-t")
        .arg(&left)
        .arg(editor)
        .arg("C-m"))?;
    sh(Command::new("tmux")
        .arg("send-keys")
        .arg("-t")
        .arg(&right)
        .arg(agent)
        .arg("C-m"))?;
    tmux_color(&window, color, name)?;
    sh(Command::new("tmux").arg("select-pane").arg("-t").arg(left))?;
    sh(Command::new("tmux")
        .arg("select-window")
        .arg("-t")
        .arg(&window))?;
    Ok(window)
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
    sh(Command::new("tmux")
        .arg("rename-window")
        .arg("-t")
        .arg(window)
        .arg(name))?;
    Ok(())
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
