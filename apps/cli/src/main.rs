//! `rrs` — developer/test harness for the Nexterm core.
//!
//! Exercises the core (profiles, the mock/real SSH transports, local PTY shell,
//! the HTTP mini-server, highlighting, the danger scanner) without any GUI. Run
//! with `RUST_LOG=debug` for verbose tracing. Secrets are never printed.
//!
//! TODO(rename): the binary is still `rrs` and crates are `rrs-*`; rename to
//! `nexterm` / `nexterm-*` in a dedicated churn-only pass once the brand settles.

use std::sync::Arc;

use anyhow::Context;
use clap::{Parser, Subcommand};
use tracing_subscriber::EnvFilter;

use rrs_core::model::ConnectionProfile;
use rrs_credentials::MemoryCredentialStore;
use rrs_miniservers::{HttpFileServer, MiniServer, MiniServerConfig};
use rrs_protocols::ssh::MockConnector;
use rrs_terminal::{builtin_profiles, LineHighlighter};
use rrs_ui_common::{scan_dangerous, AppCore};

#[derive(Parser)]
#[command(name = "rrs", version, about = "Nexterm dev harness (rrs)")]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    /// Print resolved paths, OS, and enabled features.
    Check,
    /// Profile management (file store).
    Profiles {
        #[command(subcommand)]
        action: ProfileAction,
    },
    /// Run the HTTP file server until Ctrl-C.
    ServeHttp {
        #[arg(long, default_value = ".")]
        root: String,
        #[arg(long, default_value_t = 8080)]
        port: u16,
        #[arg(long, default_value = "127.0.0.1")]
        bind: String,
    },
    /// Open the mock SSH session and run one command (no network).
    SshDemo {
        #[arg(long, default_value = "demo")]
        name: String,
    },
    /// Open a local shell via a PTY and run one command (needs `--features local-pty`).
    LocalShell {
        /// Command to send to the shell. Defaults to a harmless `echo`.
        #[arg(long)]
        command: Option<String>,
        /// Program to launch instead of `$SHELL`.
        #[arg(long)]
        program: Option<String>,
    },
    /// Connect over real SSH and run one command (needs `--features ssh-russh`).
    ///
    /// Dev harness only: the password is read from an environment variable
    /// (default `NEXTERM_SSH_PASSWORD`) and never printed. This is not the final
    /// credential UX — production stores secrets in the OS keyring.
    #[cfg(feature = "ssh-russh")]
    SshConnect {
        #[arg(long)]
        host: String,
        #[arg(long, default_value_t = 22)]
        port: u16,
        #[arg(long, default_value = "root")]
        user: String,
        /// Command to run; defaults to a harmless probe.
        #[arg(long)]
        command: Option<String>,
        /// Path to a private key file (publickey auth).
        #[arg(long)]
        key: Option<String>,
        /// Env var holding the password (dev-only).
        #[arg(long, default_value = "NEXTERM_SSH_PASSWORD")]
        password_env: String,
        /// Disable known_hosts checking (accept unknown host keys).
        #[arg(long)]
        insecure: bool,
    },
    /// Show highlight spans for a line of text.
    Highlight { text: String },
    /// Check a command against the multi-exec danger rules.
    DangerCheck { command: String },
}

#[derive(Subcommand)]
enum ProfileAction {
    /// List stored profiles.
    List,
    /// Add a demo SSH profile.
    AddSsh {
        name: String,
        host: String,
        #[arg(long, default_value = "root")]
        user: String,
    },
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("info")),
        )
        .init();

    let cli = Cli::parse();
    match cli.command {
        Command::Check => cmd_check().await,
        Command::Profiles { action } => cmd_profiles(action).await,
        Command::ServeHttp { root, port, bind } => cmd_serve_http(root, port, bind).await,
        Command::SshDemo { name } => cmd_ssh_demo(name).await,
        Command::LocalShell { command, program } => cmd_local_shell(command, program).await,
        #[cfg(feature = "ssh-russh")]
        Command::SshConnect {
            host,
            port,
            user,
            command,
            key,
            password_env,
            insecure,
        } => cmd_ssh_connect(host, port, user, command, key, password_env, insecure).await,
        Command::Highlight { text } => cmd_highlight(text),
        Command::DangerCheck { command } => cmd_danger_check(command),
    }
}

async fn build_core() -> anyhow::Result<Arc<AppCore>> {
    let credentials = Arc::new(MemoryCredentialStore::new());
    let connector = Arc::new(MockConnector);
    #[allow(unused_mut)]
    let mut core = AppCore::with_defaults(credentials, connector).await?;
    // Wire the PTY-backed local-shell connector when compiled in.
    #[cfg(feature = "local-pty")]
    {
        core = core.with_local_connector(Arc::new(rrs_protocols::LocalShellConnector));
    }
    Ok(Arc::new(core))
}

async fn cmd_check() -> anyhow::Result<()> {
    println!("app name     : {}", rrs_platform::APP_NAME);
    println!("os           : {:?}", rrs_platform::current_os());
    println!("config dir   : {}", rrs_platform::config_dir().display());
    println!("data dir     : {}", rrs_platform::data_dir().display());
    println!(
        "config file  : {}",
        rrs_core::config::AppConfig::default_path().display()
    );
    println!(
        "features     : keyring-os={} ssh-russh={} pty={} local-pty={}",
        cfg!(feature = "keyring-os"),
        cfg!(feature = "ssh-russh"),
        cfg!(feature = "pty"),
        cfg!(feature = "local-pty"),
    );
    Ok(())
}

async fn cmd_profiles(action: ProfileAction) -> anyhow::Result<()> {
    let core = build_core().await?;
    let store = core.profiles();
    match action {
        ProfileAction::List => {
            let profiles = store.list_profiles().await.context("listing profiles")?;
            if profiles.is_empty() {
                println!("(no profiles yet — try `rrs profiles add-ssh myhost 10.0.0.1`)");
            }
            for p in profiles {
                println!("{}  {}  [{:?}]", p.id, p.name, p.kind());
            }
            Ok(())
        }
        ProfileAction::AddSsh { name, host, user } => {
            let profile = ConnectionProfile::new_ssh(name, host, user);
            let id = profile.id;
            store
                .upsert_profile(profile)
                .await
                .context("saving profile")?;
            println!("added profile {id}");
            Ok(())
        }
    }
}

async fn cmd_serve_http(root: String, port: u16, bind: String) -> anyhow::Result<()> {
    let mut config = MiniServerConfig::http("cli-http", port, root);
    config.bind_address = bind;
    if let Some(warning) = config.security_warning() {
        tracing::warn!("{warning}");
    }
    let mut server = HttpFileServer::new(config);
    server.start().await.context("starting http server")?;
    println!("HTTP file server running. Press Ctrl-C to stop.");
    tokio::signal::ctrl_c()
        .await
        .context("waiting for ctrl-c")?;
    server.stop().await.context("stopping http server")?;
    println!("stopped.");
    Ok(())
}

async fn cmd_ssh_demo(name: String) -> anyhow::Result<()> {
    let core = build_core().await?;
    let profile = ConnectionProfile::new_ssh(&name, "mock.invalid", "root");
    let (id, mut session) = core.connect(&profile).await?;
    println!("session {id} connected (mock)");

    // Read the banner.
    let banner = session.read().await?;
    print!("{}", String::from_utf8_lossy(&banner));
    // Send a command and read the echo.
    session.write(b"uname -a").await?;
    let out = session.read().await?;
    print!("{}", String::from_utf8_lossy(&out));
    println!("\n(demo complete)");
    session.close().await?;
    Ok(())
}

async fn cmd_local_shell(command: Option<String>, program: Option<String>) -> anyhow::Result<()> {
    let core = build_core().await?;
    let profile = ConnectionProfile::new_local_shell("local", program);
    let (id, mut session) = core.connect(&profile).await?;
    println!("session {id} connected (local pty)");

    // Run the command, then `exit` so the shell closes and we observe clean EOF
    // instead of blocking on a prompt.
    let cmd = command.unwrap_or_else(|| "echo hello from rrs local shell".to_string());
    session.write(format!("{cmd}\nexit\n").as_bytes()).await?;
    loop {
        let chunk = session.read().await?;
        if chunk.is_empty() {
            break;
        }
        print!("{}", String::from_utf8_lossy(&chunk));
    }
    session.close().await?;
    println!("\n(local-shell complete)");
    Ok(())
}

#[cfg(feature = "ssh-russh")]
#[allow(clippy::too_many_arguments)]
async fn cmd_ssh_connect(
    host: String,
    port: u16,
    user: String,
    command: Option<String>,
    key: Option<String>,
    password_env: String,
    insecure: bool,
) -> anyhow::Result<()> {
    use rrs_core::model::{CredentialRef, ProtocolSettings};
    use rrs_credentials::Secret;
    use rrs_protocols::RusshConnector;

    let credentials = Arc::new(MemoryCredentialStore::new());
    let connector = Arc::new(RusshConnector);
    let core = Arc::new(AppCore::with_defaults(credentials, connector).await?);

    let mut profile = ConnectionProfile::new_ssh("ssh-connect", &host, &user);
    if let ProtocolSettings::Ssh(s) = &mut profile.settings {
        s.port = port;
        s.private_key_path = key;
        s.strict_host_key_checking = !insecure;
    }

    // Dev-only: pull the password from the environment and stash it in the
    // (in-memory) credential store, so it flows through the normal transient
    // resolve path. The value is never printed.
    if let Ok(pw) = std::env::var(&password_env) {
        if !pw.is_empty() {
            profile.credential = Some(CredentialRef::new("ssh-connect-cli"));
            core.set_profile_secret(&profile, Secret::new(pw)).await?;
        }
    }

    let (id, mut session) = core.connect(&profile).await.context("ssh connect")?;
    println!("session {id} connected (ssh)");

    let cmd = command.unwrap_or_else(|| "echo SSH_OK; uname -a".to_string());
    session.write(format!("{cmd}\nexit\n").as_bytes()).await?;
    loop {
        let chunk = session.read().await?;
        if chunk.is_empty() {
            break;
        }
        print!("{}", String::from_utf8_lossy(&chunk));
    }
    session.close().await?;
    println!("\n(ssh-connect complete)");
    Ok(())
}

fn cmd_highlight(text: String) -> anyhow::Result<()> {
    let profile = &builtin_profiles()[0];
    let hl = LineHighlighter::from_profile(profile).context("compiling highlight rules")?;
    let spans = hl.spans(&text);
    println!("line: {text}");
    if spans.is_empty() {
        println!("(no matches)");
    }
    for s in spans {
        println!(
            "  [{:>3}..{:<3}] {:?}  {:?}",
            s.start,
            s.end,
            s.style,
            &text[s.start..s.end]
        );
    }
    Ok(())
}

fn cmd_danger_check(command: String) -> anyhow::Result<()> {
    let warnings = scan_dangerous(&command);
    if warnings.is_empty() {
        println!("OK - no dangerous patterns detected.");
    } else {
        println!("flagged:");
        for w in warnings {
            println!("  - {} ({})", w.matched, w.reason);
        }
    }
    Ok(())
}
