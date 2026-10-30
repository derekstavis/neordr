use std::{
    path::PathBuf,
    process::{Command, ExitCode},
};

use anyhow::{Context, Result};
use clap::{Parser, Subcommand};
use neordr::{attach, nvim, protocol, router, selection, server, service, session};
use serde_json::{Value, json};

#[derive(Parser)]
#[command(
    name = "neordr",
    version,
    about = "Herdr-compatible Neovim session bridge"
)]
struct Cli {
    #[arg(long, global = true, env = "HERDR_SESSION")]
    session: Option<String>,
    #[command(subcommand)]
    command: Option<Commands>,
}

#[derive(Subcommand)]
enum Commands {
    /// Diagnose bridges, attached UIs and Moshi conversation discovery (read-only).
    Doctor {
        #[arg(long)]
        json: bool,
        /// Local Moshi HTTP gateway port.
        #[arg(long, default_value_t = 24543)]
        gateway_port: u16,
    },
    /// Herdr-compatible terminal control streaming (full Neovim editor).
    Terminal {
        #[command(subcommand)]
        command: TerminalCommands,
    },
    /// Validate Moshi's Herdr compatibility configuration.
    Config {
        #[command(subcommand)]
        command: ConfigCommands,
    },
    /// Herdr server compatibility commands.
    Server {
        #[command(subcommand)]
        command: ServerCommands,
    },
    /// Route session-less Moshi input requests to the owning Neovim instance.
    Router {
        /// Ensure an independent router is running, then return.
        #[arg(long)]
        background: bool,
    },
    /// Run the router as a user service (launchd or systemd).
    Service {
        #[command(subcommand)]
        command: ServiceCommands,
    },
    /// Find live Neovim RPC sockets (including instances without a bridge).
    Discover {
        #[arg(long)]
        socket: Vec<PathBuf>,
        #[arg(long)]
        json: bool,
    },
    /// Serve one Neovim instance in the foreground until SIGINT/SIGTERM.
    Serve {
        #[arg(long, env = "NVIM")]
        nvim: PathBuf,
    },
    Session {
        #[command(subcommand)]
        command: Sessions,
    },
    /// Send one JSON API request to a registered session.
    Rpc {
        method: String,
        #[arg(long, default_value = "{}")]
        params: String,
    },
}

#[derive(Subcommand)]
enum TerminalCommands {
    Session {
        #[command(subcommand)]
        command: TerminalSessions,
    },
}
#[derive(Subcommand)]
enum TerminalSessions {
    Control {
        target: String,
        /// Accepted for compatibility; attaches another UI without evicting clients.
        #[arg(long)]
        takeover: bool,
        #[arg(long, default_value_t = 80)]
        cols: u16,
        #[arg(long, default_value_t = 24)]
        rows: u16,
    },
}

#[derive(Subcommand)]
enum ServiceCommands {
    /// Install and start the service. A router that is already running keeps serving.
    Install,
    /// Stop the service and remove its definition. Workspaces keep running.
    Uninstall,
    /// Restart the service's router; its workspaces keep running.
    Restart,
    /// Show whether the service is installed and which router answers.
    Status,
}

#[derive(Subcommand)]
enum ConfigCommands {
    Check,
}

#[derive(Subcommand)]
enum ServerCommands {
    ReloadConfig,
}

fn check_config(root: &std::path::Path) -> Result<()> {
    let path = std::env::var_os("HERDR_CONFIG_PATH")
        .filter(|p| !p.is_empty())
        .map(PathBuf::from)
        .unwrap_or_else(|| root.join("herdr/config.toml"));
    let contents = match std::fs::read_to_string(&path) {
        Ok(contents) => contents,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(()),
        Err(error) => return Err(error).with_context(|| format!("read {}", path.display())),
    };
    let config: toml::Table =
        toml::from_str(&contents).with_context(|| format!("invalid config {}", path.display()))?;
    if let Some(mode) = config
        .get("ui")
        .and_then(|ui| ui.get("sidebar_collapsed_mode"))
    {
        anyhow::ensure!(
            matches!(mode.as_str(), Some("hidden" | "compact")),
            "ui.sidebar_collapsed_mode must be hidden or compact"
        );
    }
    Ok(())
}

#[derive(Subcommand)]
enum Sessions {
    List {
        #[arg(long)]
        json: bool,
    },
    Attach {
        name: String,
    },
}

fn default_workspace(config: &std::path::Path) -> Result<String> {
    if let Some(name) = selection::current(config)? {
        return Ok(name);
    }
    if let Some(first) = session::bridges(config)?.first() {
        return Ok(first.name.clone());
    }
    // Nothing is running: start a workspace in the home directory.
    let home = std::env::var_os("HOME")
        .map(PathBuf::from)
        .filter(|p| p.is_absolute() && p.is_dir())
        .context("no running Neovim workspaces and $HOME is not usable for a new one")?;
    let params = neordr::contracts::api::WorkspaceCreateParams {
        source_workspace_id: None,
        cwd: Some(home.to_string_lossy().into()),
        focus: true,
        label: None,
        env: Default::default(),
    };
    neordr::editors::create(config, &params, None).context("create default workspace in ~")?;
    selection::current(config)?
        .or_else(|| {
            session::bridges(config)
                .ok()?
                .first()
                .map(|s| s.name.clone())
        })
        .context("created a default workspace but could not find it")
}

fn run() -> Result<i32> {
    let cli = Cli::parse();
    if let Some(Commands::Discover { socket, json }) = &cli.command {
        let instances = nvim::discover(socket);
        if *json {
            println!("{}", serde_json::to_string_pretty(&instances)?);
        } else {
            for instance in instances {
                println!(
                    "{}\t{}\t{}",
                    instance.pid,
                    instance.socket.display(),
                    instance.cwd
                );
            }
        }
        return Ok(0);
    }
    let config = session::config_root()?;
    match cli.command {
        Some(Commands::Doctor { json, gateway_port }) => {
            let report = neordr::doctor::inspect(&config, gateway_port, cli.session.as_deref())?;
            if json {
                println!("{}", serde_json::to_string_pretty(&report)?);
            } else {
                print!("{}", report.text());
            }
        }
        Some(Commands::Terminal {
            command:
                TerminalCommands::Session {
                    command:
                        TerminalSessions::Control {
                            target,
                            takeover: _,
                            cols,
                            rows,
                        },
                },
        }) => return neordr::control::run(&config, cli.session.as_deref(), &target, cols, rows),
        Some(Commands::Config {
            command: ConfigCommands::Check,
        }) => {
            check_config(&config)?;
            println!("config: ok");
        }
        Some(Commands::Server {
            command: ServerCommands::ReloadConfig,
        }) => {
            check_config(&config)?;
            // Moshi updates Herdr's sidebar before attachment. Neovim owns our
            // UI, so this setting has no renderer to reload and is not applied.
            println!(
                "{}",
                json!({"ok": true, "ui_owner": "neovim", "herdr_ui_settings_applied": false})
            );
        }
        Some(Commands::Router { background }) => {
            if background {
                router::start_background(&config)?;
            } else {
                router::serve(&config)?;
            }
        }
        Some(Commands::Service { command }) => match command {
            ServiceCommands::Install => service::install(&config)?,
            ServiceCommands::Uninstall => service::uninstall()?,
            ServiceCommands::Restart => service::restart(&config)?,
            ServiceCommands::Status => service::status(&config)?,
        },
        Some(Commands::Discover { .. }) => unreachable!(),
        Some(Commands::Serve { nvim }) => {
            let name = cli.session.context("serve requires --session <name>")?;
            session::validate_name(&name)?;
            server::serve(&session::socket_path(&config, &name)?, &nvim)?;
        }
        Some(Commands::Session {
            command: Sessions::List { json },
        }) => {
            router::start_background(&config)?;
            let sessions = session::list(&config)?;
            if json {
                println!(
                    "{}",
                    serde_json::to_string_pretty(&json!({"sessions": sessions}))?
                );
            } else {
                for item in sessions {
                    println!("{}\t{}", item.name, item.socket_path.display());
                }
            }
        }
        Some(Commands::Session {
            command: Sessions::Attach { name },
        }) => {
            // Canonicalize argv so the long-lived process visibly carries
            // --session, matching moshi-hook's command-line fallback.
            let exe = std::env::args_os()
                .next()
                .context("missing executable name")?;
            let status = Command::new(exe).arg("--session").arg(name).status()?;
            return Ok(status.code().unwrap_or(1));
        }
        Some(Commands::Rpc { method, params }) => {
            let name = cli.session.unwrap_or_else(|| "default".into());
            if name == "default" {
                router::start_background(&config)?;
            }
            let params: Value = serde_json::from_str(&params)?;
            let result = protocol::call(
                &session::socket_path(&config, &name)?,
                &protocol::Request {
                    id: "neordr:cli".into(),
                    method,
                    params,
                },
            )?;
            println!("{}", serde_json::to_string_pretty(&result)?);
        }
        None => {
            return match cli.session {
                Some(name) if name != "default" => attach::run(&config, name, false),
                _ => {
                    router::start_background(&config)?;
                    attach::run(&config, default_workspace(&config)?, true)
                }
            };
        }
    }
    Ok(0)
}

fn main() -> ExitCode {
    match run() {
        Ok(code) => ExitCode::from(u8::try_from(code).unwrap_or(1)),
        Err(e) => {
            eprintln!("neordr: {e:#}");
            ExitCode::FAILURE
        }
    }
}
