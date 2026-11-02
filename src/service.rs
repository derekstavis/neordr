//! Run the router as a user service (launchd on macOS, systemd on Linux).
//!
//! A router forked from an ssh login or another service inherits that launch
//! context: macOS keeps it in the Background session (no display, no GUI
//! services) and every workspace it spawns inherits the same limits. A service
//! owned by the user's own manager always starts from a clean environment.
//!
//! Installing never touches a router that is already running. The router holds
//! an advisory lock and exits 0 when another router owns it, so the service
//! simply stays idle until `start` is requested after that router is gone.
use crate::{
    api,
    contracts::api::{Method, PingParams},
    protocol,
};
use anyhow::{Context, Result, bail, ensure};
use regex::Regex;
use std::{
    env, fs,
    path::{Path, PathBuf},
    process::{Command, Output},
    thread,
    time::{Duration, Instant},
};

const MACOS_LABEL: &str = "dev.neordr.router";
const LINUX_UNIT: &str = "neordr";

/// Everything a unit or plist needs. Rendering is pure so it can be tested.
pub struct Spec {
    pub label: String,
    pub exe: PathBuf,
    pub config: PathBuf,
    pub path: String,
    pub home: PathBuf,
    pub log: PathBuf,
}

fn macos() -> bool {
    cfg!(target_os = "macos")
}

/// `NEORDR_SERVICE_LABEL` isolates test installs from a real one.
fn label() -> String {
    match env::var("NEORDR_SERVICE_LABEL") {
        Ok(label) if !label.is_empty() => label,
        _ if macos() => MACOS_LABEL.into(),
        _ => LINUX_UNIT.into(),
    }
}

fn home() -> Result<PathBuf> {
    let home = PathBuf::from(env::var_os("HOME").context("HOME is unset")?);
    ensure!(home.is_absolute(), "HOME must be absolute");
    Ok(home)
}

fn uid() -> u32 {
    unsafe { libc::getuid() }
}

/// Where the manager looks for this service's definition.
pub fn definition_path() -> Result<PathBuf> {
    let label = label();
    if macos() {
        return Ok(home()?
            .join("Library/LaunchAgents")
            .join(format!("{label}.plist")));
    }
    let base = match env::var_os("XDG_CONFIG_HOME").filter(|p| !p.is_empty()) {
        Some(path) => PathBuf::from(path),
        None => home()?.join(".config"),
    };
    Ok(base.join("systemd/user").join(format!("{label}.service")))
}

/// Child processes need the user's tools, but a service starts with almost no
/// PATH. Keep the installer's entries and add the usual locations.
pub fn service_path(current: &str, home: &Path) -> String {
    let mut entries: Vec<String> = Vec::new();
    let defaults = [
        home.join(".local/bin").to_string_lossy().into_owned(),
        "/opt/homebrew/bin".into(),
        "/opt/homebrew/sbin".into(),
        "/usr/local/bin".into(),
        "/usr/bin".into(),
        "/bin".into(),
        "/usr/sbin".into(),
        "/sbin".into(),
    ];
    for entry in current.split(':').map(str::to_owned).chain(defaults) {
        // The service must not depend on the installer's working directory.
        if entry.starts_with('/') && !entries.contains(&entry) {
            entries.push(entry);
        }
    }
    entries.join(":")
}

fn xml_escape(value: &str) -> String {
    value
        .replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
}

/// systemd expands `%` specifiers and treats quotes and backslashes specially.
fn systemd_quote(value: &str) -> String {
    let escaped = value
        .replace('\\', "\\\\")
        .replace('"', "\\\"")
        .replace('%', "%%");
    format!("\"{escaped}\"")
}

fn config_line_launchd(config: &Path) -> String {
    format!(
        "<key>XDG_CONFIG_HOME</key><string>{}</string>",
        xml_escape(&config.to_string_lossy())
    )
}

fn config_line_systemd(config: &Path) -> String {
    format!(
        "Environment={}",
        systemd_quote(&format!("XDG_CONFIG_HOME={}", config.display()))
    )
}

pub fn render_launchd(spec: &Spec) -> String {
    let exe = xml_escape(&spec.exe.to_string_lossy());
    let log = xml_escape(&spec.log.to_string_lossy());
    format!(
        r#"<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
	<key>Label</key>
	<string>{label}</string>
	<key>ProgramArguments</key>
	<array>
		<string>{exe}</string>
		<string>router</string>
	</array>
	<key>EnvironmentVariables</key>
	<dict>
		<key>PATH</key><string>{path}</string>
		{config}
	</dict>
	<key>RunAtLoad</key>
	<true/>
	<!-- A router that finds another one running exits 0; do not respawn it. -->
	<key>KeepAlive</key>
	<dict>
		<key>SuccessfulExit</key>
		<false/>
	</dict>
	<!-- Workspaces are children of the router and must outlive a restart. -->
	<key>AbandonProcessGroup</key>
	<true/>
	<key>LimitLoadToSessionType</key>
	<string>Aqua</string>
	<key>WorkingDirectory</key>
	<string>{home}</string>
	<key>StandardOutPath</key>
	<string>{log}</string>
	<key>StandardErrorPath</key>
	<string>{log}</string>
</dict>
</plist>
"#,
        label = xml_escape(&spec.label),
        path = xml_escape(&spec.path),
        config = config_line_launchd(&spec.config),
        home = xml_escape(&spec.home.to_string_lossy()),
    )
}

pub fn render_systemd(spec: &Spec) -> String {
    format!(
        "[Unit]\n\
         Description=Neordr Herdr compatibility router\n\
         After=network-online.target\n\
         \n\
         [Service]\n\
         Type=simple\n\
         ExecStart={exe} router\n\
         Restart=on-failure\n\
         RestartSec=5\n\
         # Workspaces are children of the router and must outlive a restart.\n\
         KillMode=process\n\
         Environment={path}\n\
         {config}\n\
         WorkingDirectory=%h\n\
         \n\
         [Install]\n\
         WantedBy=default.target\n",
        exe = systemd_quote(&spec.exe.to_string_lossy()),
        path = systemd_quote(&format!("PATH={}", spec.path)),
        config = config_line_systemd(&spec.config),
    )
}

fn spec(config: &Path) -> Result<Spec> {
    let home = home()?;
    let exe = env::current_exe()
        .context("locate the running neordr binary")?
        .canonicalize()?;
    Ok(Spec {
        label: label(),
        exe,
        config: config.to_owned(),
        path: service_path(&env::var("PATH").unwrap_or_default(), &home),
        log: home.join("Library/Logs/neordr-router.log"),
        home,
    })
}

fn run(program: &str, args: &[&str]) -> Result<Output> {
    Command::new(program)
        .args(args)
        .output()
        .with_context(|| format!("run {program}"))
}

fn ok(program: &str, args: &[&str]) -> bool {
    run(program, args).is_ok_and(|output| output.status.success())
}

fn describe(output: &Output) -> String {
    let text = String::from_utf8_lossy(&output.stderr);
    let text = text.trim();
    if text.is_empty() {
        String::from_utf8_lossy(&output.stdout).trim().to_owned()
    } else {
        text.to_owned()
    }
}

fn require(program: &str, args: &[&str]) -> Result<()> {
    let output = run(program, args)?;
    ensure!(
        output.status.success(),
        "{program} {}: {}",
        args.join(" "),
        describe(&output)
    );
    Ok(())
}

/// A user's GUI session exists only while they are logged in at the console.
/// Without one, the per-user background domain is the closest equivalent.
fn launchd_domain() -> (String, bool) {
    let gui = format!("gui/{}", uid());
    if ok("launchctl", &["print", &gui]) {
        (gui, true)
    } else {
        (format!("user/{}", uid()), false)
    }
}

fn launchd_target() -> (String, String) {
    let (domain, _) = launchd_domain();
    let target = format!("{domain}/{}", label());
    (domain, target)
}

fn loaded() -> bool {
    if macos() {
        let (_, target) = launchd_target();
        ok("launchctl", &["print", &target])
    } else {
        ok("systemctl", &["--user", "is-enabled", &unit_name()])
    }
}

fn unit_name() -> String {
    format!("{}.service", label())
}

/// PID of the router this service currently runs, if any.
pub fn manager_pid() -> Option<u32> {
    if macos() {
        let (_, target) = launchd_target();
        let output = run("launchctl", &["print", &target]).ok()?;
        let text = String::from_utf8_lossy(&output.stdout);
        let pid = Regex::new(r"(?m)^\s*pid = (\d+)").ok()?;
        return pid.captures(&text)?[1].parse().ok();
    }
    let output = run(
        "systemctl",
        &["--user", "show", "-p", "MainPID", "--value", &unit_name()],
    )
    .ok()?;
    match String::from_utf8_lossy(&output.stdout).trim().parse() {
        Ok(0) | Err(_) => None,
        Ok(pid) => Some(pid),
    }
}

/// True when a service definition exists that serves this config root. Test
/// and alternate config roots must never kick the user's real service.
pub fn installed_for(config: &Path) -> bool {
    let Ok(path) = definition_path() else {
        return false;
    };
    let Ok(contents) = fs::read_to_string(path) else {
        return false;
    };
    contents.contains(&if macos() {
        config_line_launchd(config)
    } else {
        config_line_systemd(config)
    })
}

fn router_ready(config: &Path) -> bool {
    protocol::call_typed(
        &config.join("herdr/herdr.sock"),
        &api::request("neordr:service", Method::Ping(PingParams {})),
    )
    .is_ok_and(|reply| reply.identity.neordr_router == Some(true))
}

fn router_pid(config: &Path) -> Option<u32> {
    protocol::call_typed(
        &config.join("herdr/herdr.sock"),
        &api::request("neordr:service", Method::Ping(PingParams {})),
    )
    .ok()?
    .identity
    .pid
}

/// Ask the manager to run the router now. A no-op when it is already running.
pub fn start() -> Result<()> {
    if macos() {
        let (domain, target) = launchd_target();
        if !ok("launchctl", &["print", &target]) {
            let path = definition_path()?;
            require(
                "launchctl",
                &["bootstrap", &domain, &path.to_string_lossy()],
            )?;
        }
        return require("launchctl", &["kickstart", &target]);
    }
    require("systemctl", &["--user", "start", &unit_name()])
}

fn wait_ready(config: &Path, timeout: Duration) -> bool {
    let deadline = Instant::now() + timeout;
    while Instant::now() < deadline {
        if router_ready(config) {
            return true;
        }
        thread::sleep(Duration::from_millis(100));
    }
    false
}

/// Start the installed service and wait for its router. Used by the lazy
/// router start; callers fall back to a direct spawn if this fails.
pub fn start_and_wait(config: &Path) -> Result<()> {
    start()?;
    ensure!(
        wait_ready(config, Duration::from_secs(5)),
        "service started but the router did not become ready"
    );
    Ok(())
}

pub fn install(config: &Path) -> Result<()> {
    let spec = spec(config)?;
    let path = definition_path()?;
    let desired = if macos() {
        render_launchd(&spec)
    } else {
        render_systemd(&spec)
    };
    let changed = fs::read_to_string(&path).map_or(true, |current| current != desired);
    if changed {
        fs::create_dir_all(path.parent().context("definition has no parent")?)?;
        if macos() {
            fs::create_dir_all(spec.log.parent().context("log has no parent")?)?;
        }
        fs::write(&path, &desired).with_context(|| format!("write {}", path.display()))?;
        println!("wrote {}", path.display());
    } else {
        println!("{} is up to date", path.display());
    }
    // Capture the running router before changing anything so we can say whether
    // the service took over or is waiting behind it.
    let existing = router_pid(config);
    if macos() {
        let (domain, gui) = launchd_domain();
        if !gui {
            eprintln!(
                "warning: no GUI session for this user; using {domain}. Display tools such as \
                 screencapture stay unavailable until the service runs in the GUI session \
                 (re-run `neordr service install` after logging in)."
            );
        }
        if !loaded() {
            require(
                "launchctl",
                &["bootstrap", &domain, &path.to_string_lossy()],
            )?;
        } else if changed {
            println!(
                "service already loaded; run `neordr service restart` to apply the new definition"
            );
        }
    } else {
        require("systemctl", &["--user", "daemon-reload"])?;
        require("systemctl", &["--user", "enable", "--now", &unit_name()])?;
        if let Ok(user) = env::var("USER") {
            let linger = run("loginctl", &["show-user", &user, "-p", "Linger", "--value"]);
            if linger.is_ok_and(|o| String::from_utf8_lossy(&o.stdout).trim() == "no") {
                println!(
                    "note: lingering is off, so the service stops when you log out. \
                     Run `loginctl enable-linger {user}` to keep it running."
                );
            }
        }
    }
    match existing {
        Some(pid) if manager_pid() != Some(pid) => println!(
            "router pid {pid} is already running outside the service and keeps serving. \
             Its workspaces are untouched; the service takes over the next time the router \
             is needed after that process exits."
        ),
        Some(pid) => println!("service router already running as pid {pid}"),
        None => {
            if wait_ready(config, Duration::from_secs(5)) {
                println!(
                    "router running under the service (pid {})",
                    manager_pid().map_or("?".into(), |pid| pid.to_string())
                );
            } else {
                bail!(
                    "service installed but the router did not become ready; see `neordr service status`"
                );
            }
        }
    }
    Ok(())
}

pub fn uninstall() -> Result<()> {
    let path = definition_path()?;
    if macos() {
        let (_, target) = launchd_target();
        if ok("launchctl", &["print", &target]) {
            require("launchctl", &["bootout", &target])?;
        }
    } else if path.exists() {
        // Ignore failures: the unit may never have been enabled.
        let _ = run("systemctl", &["--user", "disable", "--now", &unit_name()]);
    }
    match fs::remove_file(&path) {
        Ok(()) => println!("removed {}", path.display()),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            println!("{} was not installed", path.display())
        }
        Err(error) => return Err(error).with_context(|| format!("remove {}", path.display())),
    }
    if !macos() {
        let _ = run("systemctl", &["--user", "daemon-reload"]);
    }
    println!("existing workspaces keep running; only the service's router was stopped");
    Ok(())
}

pub fn restart(config: &Path) -> Result<()> {
    ensure!(
        definition_path()?.exists(),
        "service is not installed; run `neordr service install`"
    );
    if macos() {
        if !loaded() {
            return start_and_wait(config);
        }
        let (_, target) = launchd_target();
        require("launchctl", &["kickstart", "-k", &target])?;
    } else {
        require("systemctl", &["--user", "restart", &unit_name()])?;
    }
    ensure!(
        wait_ready(config, Duration::from_secs(5)),
        "router did not become ready after restart"
    );
    Ok(())
}

pub fn status(config: &Path) -> Result<()> {
    let path = definition_path()?;
    println!(
        "definition: {} ({})",
        path.display(),
        if path.exists() { "present" } else { "missing" }
    );
    println!("serves this config root: {}", installed_for(config));
    println!("loaded: {}", loaded());
    let service_pid = manager_pid();
    println!(
        "service router pid: {}",
        service_pid.map_or("none".into(), |pid| pid.to_string())
    );
    match router_pid(config) {
        Some(pid) if Some(pid) == service_pid => println!("router: responding, run by the service"),
        Some(pid) => println!("router: responding as pid {pid}, started outside the service"),
        None => println!("router: not responding"),
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn spec() -> Spec {
        Spec {
            label: "dev.neordr.router".into(),
            exe: "/Users/me/.local/bin/neordr".into(),
            config: "/Users/me/.config".into(),
            path: "/opt/homebrew/bin:/usr/bin".into(),
            home: "/Users/me".into(),
            log: "/Users/me/Library/Logs/neordr-router.log".into(),
        }
    }

    #[test]
    fn path_keeps_absolute_entries_once_and_adds_defaults() {
        let path = service_path(
            "/custom/bin:relative:/usr/bin:/custom/bin",
            Path::new("/home/me"),
        );
        let entries: Vec<_> = path.split(':').collect();
        assert_eq!(entries[0], "/custom/bin");
        assert_eq!(entries.iter().filter(|e| **e == "/usr/bin").count(), 1);
        assert!(!entries.contains(&"relative"));
        assert!(entries.contains(&"/home/me/.local/bin"));
        assert!(entries.contains(&"/opt/homebrew/bin"));
    }

    #[test]
    fn launchd_plist_runs_router_in_the_gui_session_and_keeps_workspaces() {
        let plist = render_launchd(&spec());
        assert!(plist.contains("<string>/Users/me/.local/bin/neordr</string>"));
        assert!(plist.contains("<string>router</string>"));
        assert!(plist.contains("<key>LimitLoadToSessionType</key>\n\t<string>Aqua</string>"));
        assert!(plist.contains("<key>AbandonProcessGroup</key>\n\t<true/>"));
        assert!(plist.contains(&config_line_launchd(Path::new("/Users/me/.config"))));
        assert!(plist.contains("<key>SuccessfulExit</key>\n\t\t<false/>"));
    }

    #[test]
    fn launchd_plist_escapes_xml() {
        let mut spec = spec();
        spec.exe = "/tmp/a&b<c>/neordr".into();
        assert!(render_launchd(&spec).contains("/tmp/a&amp;b&lt;c&gt;/neordr"));
    }

    #[test]
    fn systemd_unit_keeps_workspaces_on_stop_and_quotes_values() {
        let mut spec = spec();
        spec.exe = "/home/me/my bin/100%/neordr".into();
        spec.path = "/usr/bin:/a b".into();
        let unit = render_systemd(&spec);
        assert!(unit.contains("ExecStart=\"/home/me/my bin/100%%/neordr\" router\n"));
        assert!(unit.contains("KillMode=process\n"));
        assert!(unit.contains("Environment=\"PATH=/usr/bin:/a b\"\n"));
        assert!(unit.contains(&config_line_systemd(Path::new("/Users/me/.config"))));
        assert!(unit.contains("WantedBy=default.target"));
    }

    #[test]
    fn config_marker_distinguishes_config_roots() {
        assert_ne!(
            config_line_systemd(Path::new("/a")),
            config_line_systemd(Path::new("/b"))
        );
        assert_ne!(
            config_line_launchd(Path::new("/a")),
            config_line_launchd(Path::new("/b"))
        );
    }
}
