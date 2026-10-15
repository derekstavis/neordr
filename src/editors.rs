//! Editor discovery and process lifetime, independent of session/client lifetime.
use crate::{api, backend, contracts::api::*, nvim, session};
use anyhow::{Context, Result, ensure};
use serde::{Deserialize, Serialize};
use std::{
    fs,
    io::Write,
    os::unix::{
        fs::{DirBuilderExt, OpenOptionsExt},
        process::CommandExt,
    },
    path::{Path, PathBuf},
    process::{Child, Command, Stdio},
    thread,
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};

#[derive(Serialize, Deserialize)]
struct Record {
    key: String,
    instance: nvim::Instance,
    managed: bool,
}

pub fn record(config: &Path, key: &str, instance: &nvim::Instance) -> Result<()> {
    session::validate_name(key)?;
    let root = config.join("herdr/editors");
    fs::DirBuilder::new()
        .recursive(true)
        .mode(0o700)
        .create(&root)?;
    let managed = nvim::read_lua(
        &instance.socket,
        "return vim.env.NEORDR_WORKSPACE_ID == ...",
        vec![key.into()],
    )?
    .as_bool()
        == Some(true);
    let temporary = root.join(format!(".{key}-{}.tmp", std::process::id()));
    let mut file = fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(&temporary)?;
    let result = (|| -> Result<()> {
        serde_json::to_writer(
            &mut file,
            &Record {
                key: key.into(),
                instance: instance.clone(),
                managed,
            },
        )?;
        file.flush()?;
        fs::rename(&temporary, root.join(format!("{key}.json")))?;
        Ok(())
    })();
    if result.is_err() {
        let _ = fs::remove_file(temporary);
    }
    result
}

/// Builds the headless Neovim command, started through the user's login shell
/// so `PATH` and other environment from fish, zsh, bash, etc. are respected
/// even when neordr itself was launched outside a shell session.
fn editor_command(socket: &Path) -> Command {
    let shell = std::env::var_os("SHELL")
        .map(PathBuf::from)
        .filter(|shell| shell.is_absolute() && shell.is_file());
    let Some(shell) = shell else {
        let mut command = Command::new("nvim");
        command.args(["--headless", "--listen"]).arg(socket);
        return command;
    };
    // The socket travels through the environment because the quoting rules of
    // the command string differ between shells, while "$VAR" works in all of
    // the common ones (POSIX shells and fish).
    let mut command = Command::new(shell);
    command
        .args([
            "-l",
            "-i",
            "-c",
            r#"exec nvim --headless --listen "$NEORDR_NVIM_SOCKET""#,
        ])
        .env("NEORDR_NVIM_SOCKET", socket);
    command
}

fn detached(command: &mut Command) -> Result<Child> {
    command
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null());
    unsafe {
        command.pre_exec(|| {
            if libc::setsid() < 0 {
                return Err(std::io::Error::last_os_error());
            }
            Ok(())
        });
    }
    Ok(command.spawn()?)
}

fn start_bridge(config: &Path, key: &str, socket: &Path) -> Result<()> {
    if session::inspect_bridge(&session::socket_path(config, key)?).is_ok() {
        return Ok(());
    }
    let plugin_starting = nvim::read_lua(socket,
        "local p=package.loaded['neordr']; return p ~= nil and (p.bridge_starting == true or p.job ~= nil)", vec![])
        .is_ok_and(|r| r.as_bool() == Some(true));
    if plugin_starting {
        let deadline = Instant::now() + Duration::from_secs(5);
        while Instant::now() < deadline {
            if session::inspect_bridge(&session::socket_path(config, key)?).is_ok() {
                return Ok(());
            }
            thread::sleep(Duration::from_millis(30));
        }
    }
    let mut cmd = Command::new(std::env::current_exe()?);
    cmd.args(["--session", key, "serve", "--nvim"])
        .arg(socket)
        .env("XDG_CONFIG_HOME", config);
    let mut child = detached(&mut cmd)?;
    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        if session::inspect_bridge(&session::socket_path(config, key)?).is_ok() {
            break;
        }
        // The plugin may have launched the winning bridge concurrently.
        let status = child.try_wait()?;
        if Instant::now() >= deadline {
            let _ = child.kill();
            let _ = child.wait();
            anyhow::bail!("editor bridge did not become ready (launcher: {status:?})");
        }
        thread::sleep(Duration::from_millis(30));
    }
    // Reap an eventual exit without binding the child to this process's lifetime.
    thread::spawn(move || {
        let _ = child.wait();
    });
    Ok(())
}

/// Recover recorded editors first, then discover sockets in this config namespace.
/// A registry entry is never authority to address a different PID/generation.
pub fn discover(config: &Path) -> Result<()> {
    let known = session::bridges(config)?;
    let mut seen: Vec<u64> = known
        .iter()
        .filter_map(|b| session::inspect_bridge(&b.socket_path).ok().map(|i| i.pid))
        .collect();
    if let Ok(entries) = fs::read_dir(config.join("herdr/editors")) {
        for entry in entries.flatten() {
            let Ok(bytes) = fs::read(entry.path()) else {
                continue;
            };
            let Ok(record) = serde_json::from_slice::<Record>(&bytes) else {
                continue;
            };
            if session::validate_name(&record.key).is_err() || seen.contains(&record.instance.pid) {
                continue;
            }
            let Ok(current) = nvim::inspect(&record.instance.socket) else {
                continue;
            };
            if current.pid != record.instance.pid
                || current.generation != record.instance.generation
            {
                continue;
            }
            if start_bridge(config, &record.key, &current.socket).is_ok() {
                seen.push(current.pid);
            }
        }
    }
    for instance in nvim::discover(&[]) {
        if seen.contains(&instance.pid) {
            continue;
        }
        let Ok(root) = nvim::read_lua(
            &instance.socket,
            "return vim.env.XDG_CONFIG_HOME and vim.env.XDG_CONFIG_HOME ~= '' and vim.env.XDG_CONFIG_HOME or (vim.env.HOME .. '/.config')",
            vec![],
        ) else {
            continue;
        };
        if root.as_str() != config.to_str() {
            continue;
        }
        let Ok(key) = nvim::read_lua(
            &instance.socket,
            "if vim.v.vim_did_enter ~= 1 then return nil end; local b=package.loaded['neordr.backend']; local managed=vim.env.NEORDR_WORKSPACE_ID; return b and b.session or (managed and managed ~= '' and managed) or ('editor-' .. vim.fn.getpid())",
            vec![],
        ) else {
            continue;
        };
        if let Some(key) = key.as_str() {
            let _ = start_bridge(config, key, &instance.socket);
        }
    }
    Ok(())
}

pub fn create(
    config: &Path,
    params: &WorkspaceCreateParams,
    source: Option<&nvim::Instance>,
) -> Result<api::Reply> {
    let cwd = params
        .cwd
        .as_ref()
        .map(PathBuf::from)
        .or_else(|| source.map(|s| PathBuf::from(&s.cwd)))
        .unwrap_or(std::env::current_dir()?);
    ensure!(
        cwd.is_absolute() && cwd.is_dir(),
        "workspace cwd must be an existing absolute directory"
    );
    ensure!(
        params
            .label
            .as_ref()
            .is_none_or(|s| !s.chars().any(char::is_control)),
        "workspace label contains control characters"
    );
    for (key, value) in &params.env {
        ensure!(
            !key.is_empty() && !key.contains(['=', '\0']) && !value.contains('\0'),
            "invalid workspace environment"
        );
        ensure!(
            !matches!(
                key.as_str(),
                "NVIM"
                    | "NEORDR_WORKSPACE_ID"
                    | "XDG_CONFIG_HOME"
                    | "HERDR_SESSION"
                    | "HERDR_SOCKET_PATH"
            ),
            "cannot override workspace identity environment"
        );
    }
    let nonce = SystemTime::now().duration_since(UNIX_EPOCH)?.as_nanos();
    let key = format!("e{nonce:x}");
    // Short private directory keeps Unix socket names valid on macOS as well.
    let runtime = PathBuf::from(format!("/tmp/neordr-{}-{nonce:x}", std::process::id()));
    fs::DirBuilder::new().mode(0o700).create(&runtime)?;
    let socket = runtime.join("nvim.sock");
    let mut command = editor_command(&socket);
    command
        .current_dir(&cwd)
        .envs(&params.env)
        .env("XDG_CONFIG_HOME", config)
        .env("NEORDR_WORKSPACE_ID", &key)
        .env_remove("NVIM")
        .env_remove("HERDR_SESSION")
        .env_remove("HERDR_PANE_ID")
        .env_remove("HERDR_SOCKET_PATH");
    let mut child = match detached(&mut command) {
        Ok(child) => child,
        Err(error) => {
            let _ = fs::remove_dir_all(&runtime);
            return Err(error);
        }
    };
    let result = (|| -> Result<api::Reply> {
        let deadline = Instant::now() + Duration::from_secs(10);
        loop {
            if nvim::read_lua(&socket, "return vim.v.vim_did_enter == 1", vec![])
                .is_ok_and(|r| r.as_bool() == Some(true))
            {
                break;
            }
            if let Some(status) = child.try_wait()? {
                anyhow::bail!("headless Neovim exited: {status}");
            }
            ensure!(
                Instant::now() < deadline,
                "headless Neovim did not become ready"
            );
            thread::sleep(Duration::from_millis(30));
        }
        start_bridge(config, &key, &socket)?;
        let instance = session::inspect_bridge(&session::socket_path(config, &key)?)?;
        if let Some(label) = &params.label {
            nvim::exec_lua(
                &socket,
                "require('neordr.backend').label=...; return true",
                vec![label.as_str().into()],
            )?;
        }
        // A fresh workspace starts with a shell, like Herdr; the editor remains
        // available alongside it and retains its normal configured layout.
        let ResponseResult::TabCreated { tab, root_pane } = backend::dispatch(
            &instance,
            &api::Command::Herdr(Method::TabCreate(TabCreateParams {
                workspace_id: None,
                cwd: Some(cwd.to_string_lossy().into()),
                label: None,
                env: Default::default(),
                focus: true,
            })),
        )?
        else {
            anyhow::bail!("invalid terminal creation response")
        };
        let ResponseResult::SessionSnapshot { snapshot } = backend::dispatch(
            &instance,
            &api::Command::Herdr(Method::SessionSnapshot(EmptyParams {})),
        )?
        else {
            unreachable!()
        };
        if params.focus {
            crate::selection::remember(config, &key)?;
        }
        Ok(ResponseResult::WorkspaceCreated {
            workspace: snapshot.workspaces[0].clone(),
            tab,
            root_pane,
        }
        .into())
    })();
    if result.is_err() {
        // Only this just-created child is rolled back on startup failure.
        let _ = child.kill();
        let _ = child.wait();
        let _ = fs::remove_dir_all(&runtime);
    } else {
        thread::spawn(move || {
            let _ = child.wait();
            let _ = fs::remove_dir_all(runtime);
        });
    }
    result
}

pub fn close(instance: &nvim::Instance, id: &str) -> Result<api::Reply> {
    let readiness = nvim::read_lua(
        &instance.socket,
        "return { vim.api.nvim_get_mode().blocking, vim.uv.hrtime() }",
        vec![],
    )?;
    let readiness = readiness.as_array().context("invalid close readiness")?;
    ensure!(
        readiness[0].as_bool() == Some(false),
        "Neovim is waiting at a prompt; dismiss it before closing the workspace"
    );
    // Expire a deferred request before the RPC read timeout: answering a modal
    // prompt later must never execute a close the caller saw fail.
    let expires = readiness[1].as_u64().context("invalid editor clock")? + 400_000_000;
    let result = nvim::exec_lua(
        &instance.socket,
        r#"
local generation,pid,id,expires = ...
assert(vim.uv.hrtime() <= expires, 'Workspace close expired; retry')
assert(vim.g.neordr_generation == generation and vim.fn.getpid() == pid, 'Neovim instance changed')
local b=require('neordr.backend')
assert(b.snapshot().workspaces[1].workspace_id == id, 'Unknown workspace')
for _,buf in ipairs(vim.api.nvim_list_bufs()) do
  if vim.bo[buf].buftype ~= 'terminal' and vim.bo[buf].modified then
    return 'modified_buffer'
  end
end
-- The explicit workspace close ends terminal jobs, after checking every buffer.
vim.cmd('qall!')
return true
"#,
        vec![
            instance
                .generation
                .as_deref()
                .context("missing generation")?
                .into(),
            instance.pid.into(),
            id.into(),
            expires.into(),
        ],
    );
    if result
        .as_ref()
        .is_ok_and(|v| v.as_str() == Some("modified_buffer"))
    {
        return Err(crate::protocol::RemoteError(ErrorBody {
            code: "modified_buffer".into(),
            message: "Save modified editor buffers before closing this workspace".into(),
        })
        .into());
    }
    let deadline = Instant::now() + Duration::from_secs(2);
    loop {
        if !instance.socket.exists() {
            return Ok(ResponseResult::Ok {}.into());
        }
        if let Err(error) = &result {
            // A normal Lua rejection is not the expected RPC EOF during exit.
            if error.to_string().starts_with("Neovim:") {
                return Err(anyhow::anyhow!("{error:#}"));
            }
        }
        ensure!(
            Instant::now() < deadline,
            "Neovim did not exit; workspace remains open"
        );
        thread::sleep(Duration::from_millis(20));
    }
}
