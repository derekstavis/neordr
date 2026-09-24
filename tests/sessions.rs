use std::{
    fs,
    io::Cursor,
    os::unix::fs::{PermissionsExt, symlink},
    path::{Path, PathBuf},
    process::{Child, Command, Stdio},
    thread,
    time::{Duration, Instant},
};

use neordr::{nvim, protocol, session};
use serde_json::{Value, json};

const BIN: &str = env!("CARGO_BIN_EXE_neordr");

struct Process(Child);
impl Drop for Process {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

impl Drop for Fixture {
    fn drop(&mut self) {
        if let Ok(reply) = protocol::call(
            &self.config.join("herdr/herdr.sock"),
            &protocol::Request {
                id: "cleanup".into(),
                method: "ping".into(),
                params: json!({}),
            },
        ) && reply["neordr_router"] == true
            && let Some(pid) = reply["pid"].as_u64()
        {
            unsafe {
                libc::kill(pid as i32, libc::SIGTERM);
            }
        }
    }
}

struct Fixture {
    _dir: tempfile::TempDir,
    root: PathBuf,
    socket: PathBuf,
    config: PathBuf,
    session: String,
    nvim: Process,
}

fn wait_for(mut ready: impl FnMut() -> bool) {
    let deadline = Instant::now() + Duration::from_secs(5);
    while !ready() {
        assert!(Instant::now() < deadline, "timed out waiting for fixture");
        thread::sleep(Duration::from_millis(20));
    }
}

impl Fixture {
    fn rpc(&self, method: &str, params: Value) -> Value {
        protocol::call(
            &self.endpoint(),
            &protocol::Request {
                id: "test".into(),
                method: method.into(),
                params,
            },
        )
        .unwrap()
    }

    fn terminal(&self, name: &str) -> Value {
        let path = self.root.join(name);
        fs::write(
            &path,
            r#"import os, sys, tty
tty.setraw(0)
os.write(1, b'\x1b[2J\x1b[HREADY\r\n')
while True:
    data = os.read(0, 4096)
    if not data: break
    if data == b'SCREEN':
        os.write(1, b'\x1b[2J\x1b[H' + b''.join(b'ROW%03d\r\n' % i for i in range(80)) + b'MAIN_SCREEN')
        continue
    if data == b'ALT':
        os.write(1, b'\x1b[?1049h\x1b[2J\x1b[HALTERNATE_SCREEN')
        continue
    if data == b'MAIN':
        os.write(1, b'\x1b[?1049l')
        continue
    os.write(1, b'RECEIVED ' + data.hex().encode() + b'\r\n')
"#,
        )
        .unwrap();
        let response = nvim::exec_lua(&self.socket, r#"
            local path = ...
            vim.cmd.enew()
            local buf = vim.api.nvim_get_current_buf()
            local env = require('neordr.backend').prepare(buf)
            local job = vim.fn.jobstart({'python3', path}, {term=true, env=env})
            vim.cmd.enew()
            vim.api.nvim_buf_set_lines(0, 0, -1, false, {'EDITOR UNCHANGED'})
            return vim.json.encode({buf=buf, job=job, pane=env.HERDR_PANE_ID, editor=vim.api.nvim_get_current_buf()})
        "#, vec![path.to_string_lossy().as_ref().into()]).unwrap();
        let info: Value = serde_json::from_str(response.as_str().unwrap()).unwrap();
        wait_for(|| {
            self.rpc(
                "pane.read",
                json!({"pane_id":info["pane"],"source":"visible"}),
            )["read"]["text"]
                .as_str()
                .unwrap()
                .contains("READY")
        });
        info
    }

    fn new() -> Self {
        // Keep Unix socket names short on macOS regardless of TMPDIR length.
        let dir = tempfile::Builder::new()
            .prefix("neordr-test-")
            .tempdir_in("/tmp")
            .unwrap();
        let root = fs::canonicalize(dir.path()).unwrap();
        let socket = root.join("nvim.sock");
        let config = root.join("config");
        let child = Command::new("nvim")
            .args(["--headless", "-u", "NONE", "-i", "NONE", "--listen"])
            .arg(&socket)
            .env("NVIM_LOG_FILE", root.join("nvim.log"))
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(fs::File::create(root.join("nvim.stderr")).unwrap())
            .spawn()
            .expect("integration tests require nvim on PATH");
        let mut fixture = Self {
            _dir: dir,
            root,
            socket,
            config,
            session: "demo".into(),
            nvim: Process(child),
        };
        wait_for(|| {
            assert!(
                fixture.nvim.0.try_wait().unwrap().is_none(),
                "Neovim exited: {}",
                fs::read_to_string(fixture.root.join("nvim.stderr")).unwrap()
            );
            nvim::inspect(&fixture.socket).is_ok()
        });
        fixture
    }

    fn command(&self) -> Command {
        let mut command = Command::new(BIN);
        command
            .env("XDG_CONFIG_HOME", &self.config)
            .env_remove("HERDR_SESSION");
        command
    }

    fn bridge(&self) -> Process {
        let child = self
            .command()
            .args(["--session", &self.session, "serve", "--nvim"])
            .arg(&self.socket)
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .unwrap();
        let child = Process(child);
        wait_for(|| session::inspect_bridge(&self.endpoint()).is_ok());
        child
    }

    fn endpoint(&self) -> PathBuf {
        session::socket_path(&self.config, &self.session).unwrap()
    }

    fn listed(&self) -> Value {
        let output = self
            .command()
            .args(["session", "list", "--json"])
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        serde_json::from_slice(&output.stdout).unwrap()
    }
}

#[test]
fn hidden_terminals_are_independent_and_input_never_touches_the_editor() {
    let fixture = Fixture::new();
    let _bridge = fixture.bridge();
    let first = fixture.terminal("claude");
    let second = fixture.terminal("codex");
    let panes = fixture.rpc("pane.list", json!({}));
    assert_eq!(panes["panes"].as_array().unwrap().len(), 2);
    let agents = fixture.rpc("agent.list", json!({}));
    let names: Vec<_> = agents["agents"]
        .as_array()
        .unwrap()
        .iter()
        .map(|a| a["agent"].as_str().unwrap())
        .collect();
    assert!(
        names.contains(&"claude") && names.contains(&"codex"),
        "{agents}"
    );
    fixture.rpc(
        "pane.send_text",
        json!({"pane_id": first["pane"], "text": "hello"}),
    );
    wait_for(|| {
        fixture.rpc(
            "pane.read",
            json!({"pane_id":first["pane"],"source":"visible"}),
        )["read"]["text"]
            .as_str()
            .unwrap()
            .contains("68656c6c6f")
    });
    fixture.rpc(
        "pane.send_keys",
        json!({"pane_id": first["pane"], "keys": ["Enter", "Ctrl-c"]}),
    );
    wait_for(|| {
        fixture.rpc(
            "pane.read",
            json!({"pane_id":first["pane"],"source":"visible"}),
        )["read"]["text"]
            .as_str()
            .unwrap()
            .contains("0d03")
    });
    let untouched = fixture.rpc(
        "pane.read",
        json!({"pane_id":second["pane"],"source":"visible"}),
    );
    assert!(
        !untouched["read"]["text"]
            .as_str()
            .unwrap()
            .contains("RECEIVED")
    );
    let editor = nvim::exec_lua(&fixture.socket,
        "return vim.json.encode({buf=vim.api.nvim_get_current_buf(),line=vim.api.nvim_get_current_line()})",vec![]).unwrap();
    let editor: Value = serde_json::from_str(editor.as_str().unwrap()).unwrap();
    assert_eq!(editor["buf"], second["editor"]);
    assert_eq!(editor["line"], "EDITOR UNCHANGED");
    let info = fixture.rpc("pane.process_info", json!({"pane_id": first["pane"]}));
    assert!(info["process_info"]["shell_pid"].as_u64().unwrap() > 0);
    assert!(
        !info["process_info"]["foreground_processes"]
            .as_array()
            .unwrap()
            .is_empty()
    );
    fixture.rpc("agent.focus", json!({"target":first["pane"]}));
    let focused = nvim::exec_lua(
        &fixture.socket,
        "return vim.api.nvim_get_current_buf()",
        vec![],
    )
    .unwrap();
    assert_eq!(focused.as_u64(), first["buf"].as_u64());
}

#[test]
fn exited_panes_cannot_receive_input_and_stale_generation_cannot_mutate_neovim() {
    let fixture = Fixture::new();
    let _bridge = fixture.bridge();
    let term = fixture.terminal("claude");
    nvim::exec_lua(
        &fixture.socket,
        "vim.fn.jobstop(...); return true",
        vec![term["job"].as_u64().unwrap().into()],
    )
    .unwrap();
    wait_for(|| {
        fixture.rpc("agent.list", json!({}))["agents"]
            .as_array()
            .unwrap()
            .is_empty()
    });
    let request = protocol::Request {
        id: "test".into(),
        method: "pane.send_text".into(),
        params: json!({"pane_id":term["pane"],"text":"bad"}),
    };
    assert!(
        protocol::call(&fixture.endpoint(), &request)
            .unwrap_err()
            .to_string()
            .contains("not_found")
    );
    nvim::exec_lua(
        &fixture.socket,
        "vim.g.neordr_generation='changed'; return true",
        vec![],
    )
    .unwrap();
    let request = protocol::Request {
        id: "test".into(),
        method: "workspace.focus".into(),
        params: json!({"workspace_id":"w1"}),
    };
    assert!(
        protocol::call(&fixture.endpoint(), &request)
            .unwrap_err()
            .to_string()
            .contains("stale_instance")
    );
}

#[test]
fn screen_capture_tracks_the_live_screen_and_alternate_screen() {
    let fixture = Fixture::new();
    let _bridge = fixture.bridge();
    let term = fixture.terminal("claude");
    let read = || {
        fixture.rpc(
            "pane.read",
            json!({"pane_id":term["pane"],"source":"visible"}),
        )["read"]["text"]
            .as_str()
            .unwrap()
            .to_owned()
    };
    fixture.rpc(
        "pane.send_text",
        json!({"pane_id":term["pane"],"text":"SCREEN"}),
    );
    wait_for(|| read().contains("MAIN_SCREEN"));
    assert!(
        !read().contains("ROW000"),
        "visible must exclude old scrollback"
    );
    fixture.rpc("pane.focus", json!({"pane_id":term["pane"]}));
    // Explicit remote focus now resumes terminal input. Leave it as a user
    // would before inspecting scrollback with Normal-mode commands.
    nvim::exec_lua(
        &fixture.socket,
        r"vim.api.nvim_input('<C-\\><C-N>'); return true",
        vec![],
    )
    .unwrap();
    wait_for(|| {
        nvim::read_lua(
            &fixture.socket,
            "return vim.api.nvim_get_mode().mode",
            vec![],
        )
        .unwrap()
        .as_str()
        .unwrap()
        .starts_with('n')
    });
    nvim::exec_lua(
        &fixture.socket,
        "vim.cmd('normal! gg'); return true",
        vec![],
    )
    .unwrap();
    assert!(
        read().contains("MAIN_SCREEN"),
        "user scroll position must not change detection capture"
    );
    fixture.rpc(
        "pane.send_text",
        json!({"pane_id":term["pane"],"text":"ALT"}),
    );
    wait_for(|| read().contains("ALTERNATE_SCREEN"));
    assert!(!read().contains("MAIN_SCREEN"));
    fixture.rpc(
        "pane.send_text",
        json!({"pane_id":term["pane"],"text":"MAIN"}),
    );
    wait_for(|| read().contains("MAIN_SCREEN"));
}

#[test]
fn discovery_checks_live_rpc_and_supports_custom_sockets() {
    let fixture = Fixture::new();
    let bad = fixture.root.join("not-a-socket");
    fs::write(&bad, "not a server").unwrap();
    let instances = nvim::discover(&[bad, fixture.socket.clone()]);
    assert_eq!(instances.len(), 1);
    assert_eq!(instances[0].pid, u64::from(fixture.nvim.0.id()));
    assert_eq!(instances[0].socket, fixture.socket);
    assert!(
        instances[0].generation.is_none(),
        "discovery must not register an instance"
    );
}

#[test]
fn herdr_list_shape_and_structured_unsupported_methods() {
    let fixture = Fixture::new();
    assert!(session::bridges(&fixture.config).unwrap().is_empty());
    let _bridge = fixture.bridge();
    let version = neordr::contracts::wire::PROTOCOL_VERSION;
    let ping = fixture.rpc("ping", json!({}));
    assert_eq!(ping["protocol"], version);
    assert_eq!(ping["capabilities"]["endpoint_protocol_generation"], 1);
    assert_eq!(ping["capabilities"]["live_handoff"], false);
    assert_eq!(
        fixture.rpc("session.snapshot", json!({}))["snapshot"]["protocol"],
        version
    );
    for method in ["ping", "session.snapshot"] {
        let reply = neordr::router::dispatch(
            &fixture.config,
            &protocol::Request {
                id: "version".into(),
                method: method.into(),
                params: json!({}),
            },
        );
        let result = &reply["result"];
        assert_eq!(
            if method == "ping" {
                &result["protocol"]
            } else {
                &result["snapshot"]["protocol"]
            },
            version
        );
    }
    assert_eq!(
        fixture.listed(),
        json!({"sessions": [{
            "name": "default", "default": true, "running": true,
            "socket_path": fixture.config.join("herdr/herdr.sock"),
            "session_dir": fixture.config.join("herdr"),
        }]})
    );
    let result = protocol::call(
        &fixture.endpoint(),
        &protocol::Request {
            id: "moshi-hook".into(),
            method: "not.implemented".into(),
            params: json!({}),
        },
    );
    assert!(result.unwrap_err().to_string().contains("invalid_request"));
    assert_eq!(
        fs::metadata(fixture.endpoint())
            .unwrap()
            .permissions()
            .mode()
            & 0o777,
        0o600
    );
}

#[test]
fn registration_rejects_duplicate_and_changed_instances() {
    let fixture = Fixture::new();
    let _bridge = fixture.bridge();
    let duplicate = fixture
        .command()
        .args(["--session", "demo", "serve", "--nvim"])
        .arg(&fixture.socket)
        .output()
        .unwrap();
    assert!(!duplicate.status.success());
    assert!(
        session::inspect_bridge(&fixture.endpoint()).is_ok(),
        "existing bridge must survive"
    );
    nvim::exec_lua(
        &fixture.socket,
        "vim.g.neordr_generation = 'replacement'; return true",
        vec![],
    )
    .unwrap();
    assert!(session::bridges(&fixture.config).unwrap().is_empty());
    assert!(session::inspect_bridge(&fixture.endpoint()).is_err());
}

#[test]
fn dead_neovim_disappears_and_sigterm_removes_owned_socket() {
    let mut fixture = Fixture::new();
    let mut bridge = fixture.bridge();
    fixture.nvim.0.kill().unwrap();
    fixture.nvim.0.wait().unwrap();
    assert!(session::bridges(&fixture.config).unwrap().is_empty());
    assert!(
        Command::new("kill")
            .args(["-TERM", &bridge.0.id().to_string()])
            .status()
            .unwrap()
            .success()
    );
    wait_for(|| bridge.0.try_wait().unwrap().is_some());
    assert!(!fixture.endpoint().exists());
}

#[test]
fn herdr_symlink_translates_both_attach_forms_and_preserves_session_context() {
    use std::os::fd::FromRawFd;
    let fixture = Fixture::new();
    let _bridge = fixture.bridge();
    let bin = fixture.root.join("bin");
    fs::create_dir(&bin).unwrap();
    symlink(BIN, bin.join("herdr")).unwrap();
    let capture = fixture.root.join("attach.txt");
    let fake = bin.join("nvim");
    fs::write(&fake, "#!/bin/sh\nprintf '%s\\n' \"$@\" \"$HERDR_ENV\" \"$HERDR_SESSION\" > \"$NEORDR_TEST_CAPTURE\"\n/bin/ps -p \"$PPID\" -o command= > \"$NEORDR_TEST_CAPTURE.parent\"\n").unwrap();
    fs::set_permissions(&fake, fs::Permissions::from_mode(0o755)).unwrap();
    for args in [
        &["--session", "demo"][..],
        &["session", "attach", "demo"][..],
    ] {
        // Attachment now intercepts remote gestures, so exercise its real TTY
        // contract while the fake UI records argv and ancestry.
        let (mut master, mut slave) = (-1, -1);
        let mut size = libc::winsize {
            ws_row: 24,
            ws_col: 80,
            ws_xpixel: 0,
            ws_ypixel: 0,
        };
        assert_eq!(
            unsafe {
                libc::openpty(
                    &mut master,
                    &mut slave,
                    std::ptr::null_mut(),
                    std::ptr::null_mut(),
                    &raw mut size,
                )
            },
            0
        );
        let _master = unsafe { fs::File::from_raw_fd(master) };
        let slave = unsafe { fs::File::from_raw_fd(slave) };
        let output = Command::new(bin.join("herdr"))
            .args(args)
            .env("PATH", &bin)
            .env("XDG_CONFIG_HOME", &fixture.config)
            .env("NEORDR_TEST_CAPTURE", &capture)
            .env_remove("HERDR_SESSION")
            .stdin(Stdio::from(slave))
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        assert_eq!(
            fs::read_to_string(&capture).unwrap(),
            format!(
                "--server\n{}\n--remote-ui\n1\ndemo\n",
                fixture.socket.display()
            )
        );
        assert!(
            fs::read_to_string(fixture.root.join("attach.txt.parent"))
                .unwrap()
                .contains("herdr --session demo"),
            "attach must retain a Herdr-named parent"
        );
    }
}

#[test]
fn invalid_names_cannot_escape_the_session_directory() {
    for name in ["", "..", "../outside", "a/b", "a b", "é"] {
        assert!(session::socket_path(Path::new("/tmp/config"), name).is_err());
    }
    assert!(session::validate_name("default").is_err());
    assert_eq!(
        session::socket_path(Path::new("/tmp/config"), "default").unwrap(),
        Path::new("/tmp/config/herdr/herdr.sock")
    );
    assert!(session::validate_name(&"x".repeat(65)).is_err());
    assert!(session::validate_name("nvim-project_1").is_ok());
}

#[test]
fn default_router_targets_exactly_one_of_two_neovim_instances() {
    let first = Fixture::new();
    let mut second = Fixture::new();
    second.config = first.config.clone();
    second.session = "other".into();
    let _first_bridge = first.bridge();
    let _second_bridge = second.bridge();
    let editor = first.rpc("pane.list", json!({}))["panes"][0]["pane_id"].clone();
    let request = protocol::Request {
        id: "test".into(),
        method: "pane.send_text".into(),
        params: json!({"pane_id":editor,"text":"must-not-send"}),
    };
    assert!(
        neordr::router::dispatch(&first.config, &request)["error"]["message"]
            .as_str()
            .unwrap()
            .contains("supported foreground agent")
    );
    let a = first.terminal("claude");
    let b = second.terminal("claude");
    assert_ne!(a["pane"], b["pane"]);
    let snapshot = neordr::router::dispatch(
        &first.config,
        &protocol::Request {
            id: "snapshot".into(),
            method: "session.snapshot".into(),
            params: json!({}),
        },
    );
    assert_eq!(snapshot["result"]["type"], "session_snapshot");
    for field in ["workspaces", "tabs", "panes", "layouts", "agents"] {
        assert_eq!(
            snapshot["result"]["snapshot"][field]
                .as_array()
                .unwrap()
                .len(),
            2,
            "{field}: {snapshot}"
        );
    }
    for pane in snapshot["result"]["snapshot"]["panes"].as_array().unwrap() {
        assert!(pane.get("neordr").is_none());
    }

    let request = protocol::Request {
        id: "test".into(),
        method: "pane.send_text".into(),
        params: json!({"pane_id":b["pane"],"text":"second-only"}),
    };
    let result = neordr::router::dispatch(&first.config, &request);
    assert!(result.get("error").is_none(), "{result}");
    wait_for(|| {
        second.rpc("pane.read", json!({"pane_id":b["pane"],"source":"visible"}))["read"]["text"]
            .as_str()
            .unwrap()
            .contains("7365636f6e642d6f6e6c79")
    });
    assert!(
        !first.rpc("pane.read", json!({"pane_id":a["pane"],"source":"visible"}))["read"]["text"]
            .as_str()
            .unwrap()
            .contains("RECEIVED")
    );
    let request = protocol::Request {
        id: "test".into(),
        method: "pane.send_text".into(),
        params: json!({"pane_id":"missing:w1:p1","text":"must-not-send"}),
    };
    assert!(
        neordr::router::dispatch(&first.config, &request)
            .get("error")
            .is_some()
    );
}

#[test]
fn framing_rejects_unterminated_and_oversized_requests() {
    assert!(protocol::read_frame(&mut Cursor::new(b"{}".to_vec())).is_err());
    assert!(protocol::read_frame(&mut Cursor::new(vec![b'x'; protocol::MAX_FRAME + 1])).is_err());
    assert_eq!(
        protocol::read_frame(&mut Cursor::new(b"{}\nnext\n")).unwrap(),
        b"{}\n"
    );
}

#[test]
fn create_named_terminals_preserves_editor_and_uses_literal_cwd_and_environment() {
    let fixture = Fixture::new();
    let _bridge = fixture.bridge();
    nvim::exec_lua(&fixture.socket, "vim.o.shell='/bin/sh'; vim.api.nvim_buf_set_lines(0,0,-1,false,{'KEEP EDITOR'}); return true", vec![]).unwrap();
    let original = nvim::exec_lua(
        &fixture.socket,
        "return vim.api.nvim_get_current_win()",
        vec![],
    )
    .unwrap();
    let cwd = fixture.root.join("project with ' quotes | and spaces");
    fs::create_dir(&cwd).unwrap();
    let created = fixture.rpc(
        "tab.create",
        json!({"cwd":cwd,"label":"API","env":{"NEORDR_PROBE":"kept","HERDR_PANE_ID":"forged"}}),
    );
    let ws = &created["root_pane"]["workspace_id"];
    let pane = &created["root_pane"]["pane_id"];
    assert_eq!(created["type"], "tab_created");
    assert_eq!(created["tab"]["label"], "API");
    assert_eq!(created["root_pane"]["cwd"], cwd.to_str().unwrap());
    assert_eq!(
        nvim::exec_lua(
            &fixture.socket,
            "return vim.api.nvim_get_current_win()",
            vec![]
        )
        .unwrap(),
        original
    );
    assert_eq!(
        nvim::exec_lua(
            &fixture.socket,
            "return vim.api.nvim_get_current_line()",
            vec![]
        )
        .unwrap()
        .as_str(),
        Some("KEEP EDITOR")
    );
    let captured = fixture.root.join("created-env");
    fixture.rpc("pane.send_text", json!({"pane_id":pane,"text":format!("printf '%s\\n' \"$PWD\" \"$NEORDR_PROBE\" \"$HERDR_PANE_ID\" > '{}'\r",captured.display())}));
    wait_for(|| fs::read_to_string(&captured).is_ok_and(|s| s.contains(pane.as_str().unwrap())));
    assert_eq!(
        fs::read_to_string(captured).unwrap(),
        format!("{}\nkept\n{}\n", cwd.display(), pane.as_str().unwrap())
    );
    let terminal = fixture.rpc(
        "tab.create",
        json!({"workspace_id":ws,"cwd":cwd,"label":"Tests","focus":true}),
    );
    assert_eq!(terminal["type"], "tab_created");
    assert_eq!(terminal["root_pane"]["cwd"], cwd.to_str().unwrap());
    assert_eq!(
        fixture.rpc("pane.list", json!({"workspace_id":ws}))["panes"]
            .as_array()
            .unwrap()
            .len(),
        2
    );
    fixture.rpc(
        "tab.rename",
        json!({"tab_id":terminal["tab"]["tab_id"],"label":"Unit tests"}),
    );
    assert_eq!(
        fixture.rpc(
            "pane.get",
            json!({"pane_id":terminal["root_pane"]["pane_id"]})
        )["pane"]["label"],
        "Unit tests"
    );
    let before = fixture.rpc("workspace.list", json!({}));
    assert!(
        protocol::call(
            &fixture.endpoint(),
            &protocol::Request {
                id: "bad".into(),
                method: "workspace.create".into(),
                params: json!({"cwd":"/does-not-exist-neordr"})
            }
        )
        .is_err()
    );
    assert_eq!(before, fixture.rpc("workspace.list", json!({})));
}

#[test]
fn default_creation_uses_selection_and_launch_does_not_allow_arbitrary_shell_input() {
    let first = Fixture::new();
    let mut second = Fixture::new();
    second.config = first.config.clone();
    second.session = "other".into();
    let _a = first.bridge();
    let _b = second.bridge();
    let request = |method: &str, params| {
        neordr::router::dispatch(
            &first.config,
            &protocol::Request {
                id: "create".into(),
                method: method.into(),
                params,
            },
        )
    };
    assert!(
        request("tab.create", json!({"cwd":first.root}))
            .get("error")
            .is_some()
    );
    let ws = second.rpc("workspace.list", json!({}))["workspaces"][0]["workspace_id"].clone();
    request("workspace.focus", json!({"workspace_id":ws}));
    nvim::exec_lua(&second.socket, "vim.o.shell='/bin/sh'; return true", vec![]).unwrap();
    let created = request(
        "tab.create",
        json!({"cwd":second.root,"label":"Remote","focus":true}),
    );
    assert!(created.get("error").is_none(), "{created}");
    let pane = &created["result"]["root_pane"]["pane_id"];
    assert!(pane.as_str().unwrap().starts_with("other:"));
    assert_eq!(
        first.rpc("workspace.list", json!({}))["workspaces"]
            .as_array()
            .unwrap()
            .len(),
        1
    );
    let blocked = request(
        "pane.send_text",
        json!({"pane_id":pane,"text":"echo should-not-run"}),
    );
    assert!(blocked.get("error").is_some(), "{blocked}");
    // A launch is staged without sending command bytes or executing anything.
    wait_for(|| {
        request("pane.send_text", json!({"pane_id":pane,"text":"codex"}))
            .get("error")
            .is_none()
    });
    assert!(
        !second.rpc("pane.read", json!({"pane_id":pane}))["read"]["text"]
            .as_str()
            .unwrap()
            .contains("codex")
    );
    // Expiry cancels even a staged launch; an old Chat View's Enter stays blocked.
    nvim::exec_lua(
        &second.socket,
        "for _,v in pairs(require('neordr.backend').launches) do v.expires=0 end;return true",
        vec![],
    )
    .unwrap();
    assert!(
        request("pane.send_keys", json!({"pane_id":pane,"keys":["enter"]}))
            .get("error")
            .is_some()
    );
    // A failing terminal adapter cannot leave a new workspace behind.
    nvim::exec_lua(&second.socket,"require('neordr.backend').create_terminal=function() error('adapter failed') end;return true",vec![]).unwrap();
    let before = second.rpc("workspace.list", json!({}));
    assert!(
        request("tab.create", json!({"cwd":second.root}))
            .get("error")
            .is_some()
    );
    assert_eq!(before, second.rpc("workspace.list", json!({})));
}

#[test]
fn default_selection_requires_successful_focus_and_matching_generation() {
    let fixture = Fixture::new();
    let _bridge = fixture.bridge();
    assert!(
        neordr::selection::current(&fixture.config)
            .unwrap()
            .is_none()
    );
    let workspace =
        fixture.rpc("workspace.list", json!({}))["workspaces"][0]["workspace_id"].clone();
    let mut request = protocol::Request {
        id: "focus".into(),
        method: "workspace.focus".into(),
        params: json!({"workspace_id": "demo:missing"}),
    };
    assert!(
        neordr::router::dispatch(&fixture.config, &request)
            .get("error")
            .is_some()
    );
    assert!(
        neordr::selection::current(&fixture.config)
            .unwrap()
            .is_none()
    );
    request.params = json!({"workspace_id": workspace});
    assert!(
        neordr::router::dispatch(&fixture.config, &request)
            .get("error")
            .is_none()
    );
    assert_eq!(
        neordr::selection::current(&fixture.config)
            .unwrap()
            .as_deref(),
        Some("demo")
    );
    // Read-only discovery must not reset a user's selection.
    request.method = "workspace.list".into();
    request.params = json!({});
    assert!(
        neordr::router::dispatch(&fixture.config, &request)
            .get("error")
            .is_none()
    );
    assert_eq!(
        neordr::selection::current(&fixture.config)
            .unwrap()
            .as_deref(),
        Some("demo")
    );
    nvim::exec_lua(
        &fixture.socket,
        "vim.g.neordr_generation='replacement'; return true",
        vec![],
    )
    .unwrap();
    assert!(
        neordr::selection::current(&fixture.config)
            .unwrap()
            .is_none()
    );
}

#[test]
fn modal_prompt_keeps_sessions_and_workspaces_visible_without_dismissing_it() {
    use std::os::unix::net::UnixStream;
    let fixture = Fixture::new();
    let _bridge = fixture.bridge();
    let terminal = fixture.terminal("claude");
    let before = fixture.rpc("workspace.list", json!({}));
    // Attach is deferred but nvim_input is a fast RPC. Wait for both the attach
    // reply and the initial redraw before injecting input, so the command cannot
    // run while Neovim still considers this editor headless.
    let mut modal = UnixStream::connect(&fixture.socket).unwrap();
    modal
        .set_read_timeout(Some(Duration::from_secs(2)))
        .unwrap();
    rmpv::encode::write_value(
        &mut modal,
        &rmpv::Value::Array(vec![
            0.into(),
            1.into(),
            "nvim_ui_attach".into(),
            rmpv::Value::Array(vec![
                80.into(),
                24.into(),
                rmpv::Value::Map(vec![("rgb".into(), true.into())]),
            ]),
        ]),
    )
    .unwrap();
    let (mut attached, mut flushed) = (false, false);
    while !attached || !flushed {
        let message = rmpv::decode::read_value(&mut modal).unwrap();
        let fields = message.as_array().unwrap();
        if fields[0].as_u64() == Some(1) {
            assert_eq!(fields[1].as_u64(), Some(1));
            assert!(fields[2].is_nil(), "UI attachment failed: {}", fields[2]);
            attached = true;
        } else if fields[0].as_u64() == Some(2) && fields[1].as_str() == Some("redraw") {
            flushed |= fields[2]
                .as_array()
                .unwrap()
                .iter()
                .any(|event| event.as_array().unwrap()[0].as_str() == Some("flush"));
        }
    }
    assert_eq!(
        nvim::read_lua(
            &fixture.socket,
            "return vim.api.nvim_get_mode().mode",
            vec![],
        )
        .unwrap()
        .as_str(),
        Some("n"),
        "modal fixture must start in normal mode"
    );
    // A UI error creates the same hit-enter prompt seen in the live editors.
    let input = ":lua error('NEORDR_MODAL_TEST')\r";
    rmpv::encode::write_value(
        &mut modal,
        &rmpv::Value::Array(vec![
            0.into(),
            2.into(),
            "nvim_input".into(),
            rmpv::Value::Array(vec![input.into()]),
        ]),
    )
    .unwrap();
    loop {
        let message = rmpv::decode::read_value(&mut modal).unwrap();
        let fields = message.as_array().unwrap();
        if fields[0].as_u64() == Some(1) {
            assert_eq!(fields[1].as_u64(), Some(2));
            assert!(fields[2].is_nil(), "modal input failed: {}", fields[2]);
            assert_eq!(fields[3].as_u64(), Some(input.len() as u64));
            break;
        }
    }
    let blocked = || {
        nvim::read_lua(
            &fixture.socket,
            "return vim.api.nvim_get_mode().blocking",
            vec![],
        )
        .unwrap()
        .as_bool()
            == Some(true)
    };
    wait_for(blocked);
    assert!(
        nvim::read_lua(&fixture.socket, "return vim.v.errmsg", vec![])
            .unwrap()
            .as_str()
            .unwrap()
            .contains("NEORDR_MODAL_TEST"),
        "the requested error must be the source of the hit-enter prompt"
    );
    assert_eq!(fixture.listed()["sessions"].as_array().unwrap().len(), 1);
    assert_eq!(
        nvim::discover(std::slice::from_ref(&fixture.socket)).len(),
        1
    );
    assert_eq!(fixture.rpc("workspace.list", json!({})), before);
    assert_eq!(
        fixture.rpc("session.snapshot", json!({}))["snapshot"]["workspaces"],
        before["workspaces"]
    );
    assert!(
        fixture.rpc("pane.read", json!({"pane_id":terminal["pane"]}))["read"]["text"]
            .as_str()
            .unwrap()
            .contains("READY")
    );
    let list = neordr::router::dispatch(
        &fixture.config,
        &protocol::Request {
            id: "modal".into(),
            method: "workspace.list".into(),
            params: json!({}),
        },
    );
    assert_eq!(list["result"], before);
    assert!(
        blocked(),
        "discovery must not send Enter or cancel user prompts"
    );
}

#[test]
fn partial_router_request_does_not_block_health_checks() {
    use std::{io::Write, os::unix::net::UnixStream};
    let fixture = Fixture::new();
    let _router = Process(
        fixture
            .command()
            .arg("router")
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .unwrap(),
    );
    let socket = fixture.config.join("herdr/herdr.sock");
    wait_for(|| socket.exists());
    let mut partial = UnixStream::connect(&socket).unwrap();
    partial.write_all(b"{\"id\":").unwrap();
    thread::sleep(Duration::from_millis(50));
    let now = Instant::now();
    let reply = protocol::call(
        &socket,
        &protocol::Request {
            id: "health".into(),
            method: "ping".into(),
            params: json!({}),
        },
    )
    .unwrap();
    assert_eq!(reply["neordr_router"], true);
    assert!(now.elapsed() < Duration::from_millis(500));
}

#[test]
fn moshi_capture_keys_and_errors_work_through_the_default_router() {
    let fixture = Fixture::new();
    let _bridge = fixture.bridge();
    let term = fixture.terminal("codex");
    let request = |method: &str, params: Value| {
        neordr::router::dispatch(
            &fixture.config,
            &protocol::Request {
                id: "contract".into(),
                method: method.into(),
                params,
            },
        )
    };
    let result = request(
        "pane.send_keys",
        json!({"pane_id":term["pane"],"keys":["BTab","PageDn"]}),
    );
    assert!(result.get("error").is_none(), "{result}");
    wait_for(|| {
        fixture.rpc(
            "pane.read",
            json!({"pane_id":term["pane"],"source":"recent_unwrapped","lines":80}),
        )["read"]["text"]
            .as_str()
            .unwrap()
            .contains("1b5b5a1b5b367e")
    });
    let unknown = request("future.method", json!({"pane_id":term["pane"]}));
    assert_eq!(unknown["error"]["code"], "invalid_request");
    assert_eq!(unknown["id"], "contract");
    assert_eq!(
        request("future.method", json!({}))["error"]["code"],
        "invalid_request"
    );
    let missing = request("pane.close", json!({"pane_id":"demo:missing"}));
    assert_eq!(missing["error"]["code"], "not_found");
    assert_eq!(
        request(
            "pane.read",
            json!({"pane_id":term["pane"],"source":"unsupported"})
        )["error"]["code"],
        "unsupported_source"
    );
}

#[test]
fn move_and_close_terminals_keep_surviving_jobs_and_editor_buffers() {
    let fixture = Fixture::new();
    let _bridge = fixture.bridge();
    let a = fixture.terminal("claude");
    let b = fixture.terminal("codex");
    let c = fixture.terminal("shell");
    let tabs = || {
        fixture.rpc("tab.list", json!({}))["tabs"]
            .as_array()
            .unwrap()
            .iter()
            .map(|t| t["tab_id"].clone())
            .collect::<Vec<_>>()
    };
    let before = tabs();
    fixture.rpc("tab.move", json!({"tab_id":before[0],"insert_index":3}));
    assert_eq!(
        tabs(),
        vec![before[1].clone(), before[2].clone(), before[0].clone()]
    );
    fixture.rpc("tab.move", json!({"tab_id":before[0],"insert_index":0}));
    assert_eq!(tabs(), before);
    let error = neordr::router::dispatch(
        &fixture.config,
        &protocol::Request {
            id: "bounds".into(),
            method: "tab.move".into(),
            params: json!({"tab_id":before[0],"insert_index":4}),
        },
    );
    assert_eq!(error["error"]["code"], "invalid_params");
    assert_eq!(tabs(), before);
    fixture.rpc("tab.close", json!({"tab_id":before[1]}));
    fixture.rpc("pane.close", json!({"pane_id":a["pane"]}));
    assert_eq!(tabs(), vec![before[2].clone()]);
    let state = nvim::exec_lua(&fixture.socket, "local a,b,c,editor=...; return vim.json.encode({dead=not vim.api.nvim_buf_is_valid(a) and not vim.api.nvim_buf_is_valid(b), alive=vim.fn.jobwait({c},0)[1]==-1, text=vim.api.nvim_buf_get_lines(editor,0,-1,false)})",
        vec![a["buf"].as_i64().unwrap().into(),b["buf"].as_i64().unwrap().into(),c["job"].as_i64().unwrap().into(),c["editor"].as_i64().unwrap().into()]).unwrap();
    let state: Value = serde_json::from_str(state.as_str().unwrap()).unwrap();
    assert_eq!(
        state,
        json!({"dead":true,"alive":true,"text":["EDITOR UNCHANGED"]})
    );
    // Closing a visible terminal which owns the final window must leave the editor alive.
    nvim::exec_lua(
        &fixture.socket,
        "vim.bo.modified=false; vim.api.nvim_win_set_buf(0,...); return true",
        vec![c["buf"].as_i64().unwrap().into()],
    )
    .unwrap();
    fixture.rpc("pane.close", json!({"pane_id":c["pane"]}));
    assert_eq!(
        fixture.rpc("pane.list", json!({}))["panes"][0]["label"],
        "Editor"
    );
    assert!(nvim::inspect(&fixture.socket).is_ok());
}

#[test]
fn workspace_close_preflights_edits_and_exits_even_an_adopted_editor() {
    let fixture = Fixture::new();
    let _bridge = fixture.bridge();
    let term = fixture.terminal("codex");
    let ws = fixture.rpc("workspace.list", json!({}))["workspaces"][0]["workspace_id"].clone();
    let request = protocol::Request {
        id: "close".into(),
        method: "workspace.close".into(),
        params: json!({"workspace_id":ws}),
    };
    assert_eq!(
        neordr::router::dispatch(&fixture.config, &request)["error"]["code"],
        "modified_buffer"
    );
    assert_eq!(
        fixture.rpc("pane.get", json!({"pane_id":term["pane"]}))["pane"]["agent"],
        "codex"
    );
    nvim::exec_lua(
        &fixture.socket,
        "vim.bo.modified=false; return true",
        vec![],
    )
    .unwrap();
    let reply = neordr::router::dispatch(&fixture.config, &request);
    assert!(reply.get("error").is_none(), "{reply}");
    wait_for(|| !fixture.socket.exists());
    assert!(nvim::inspect(&fixture.socket).is_err());
    assert!(session::bridges(&fixture.config).unwrap().is_empty());
}

#[test]
fn workspace_order_spans_editors_and_preserves_ids_and_focus() {
    let first = Fixture::new();
    let _first_bridge = first.bridge();
    let mut second = Fixture::new();
    second.config = first.config.clone();
    second.session = "second".into();
    let _second_bridge = second.bridge();
    for fixture in [&first, &second] {
        nvim::exec_lua(&fixture.socket, "vim.cmd('tabnew'); return true", vec![]).unwrap();
    }
    let request = |method: &str, params: Value| {
        neordr::router::dispatch(
            &first.config,
            &protocol::Request {
                id: "order".into(),
                method: method.into(),
                params,
            },
        )
    };
    let list = || {
        request("workspace.list", json!({}))["result"]["workspaces"]
            .as_array()
            .unwrap()
            .iter()
            .map(|w| w["workspace_id"].clone())
            .collect::<Vec<_>>()
    };
    let original = list();
    assert_eq!(original.len(), 2);
    let focus = nvim::exec_lua(
        &first.socket,
        "return vim.api.nvim_get_current_tabpage()",
        vec![],
    )
    .unwrap();
    let reply = request(
        "workspace.move",
        json!({"workspace_id":original[0],"insert_index":2}),
    );
    assert!(reply.get("error").is_none(), "{reply}");
    assert_eq!(list(), vec![original[1].clone(), original[0].clone()]);
    assert_eq!(
        nvim::exec_lua(
            &first.socket,
            "return vim.api.nvim_get_current_tabpage()",
            vec![]
        )
        .unwrap(),
        focus
    );
    assert_eq!(
        first.rpc("workspace.list", json!({}))["workspaces"][0]["workspace_id"],
        original[0]
    );
    let snapshot = request("session.snapshot", json!({}));
    assert_eq!(
        snapshot["result"]["snapshot"]["workspaces"][1]["workspace_id"],
        original[0]
    );
    let reply = request(
        "workspace.move",
        json!({"workspace_id":original[0],"insert_index":0}),
    );
    assert!(reply.get("error").is_none(), "{reply}");
    assert_eq!(list(), original);
    assert_eq!(
        first.rpc("workspace.list", json!({}))["workspaces"][0]["workspace_id"],
        original[0]
    );
}
