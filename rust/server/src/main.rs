//! MinDash server entry point.

use std::io::Read;
use std::path::PathBuf;
use std::sync::Arc;

use mindash_server::{router, AppState};

const USAGE: &str = "\
MinDash - your homelab, under control.

USAGE:
    mindash [OPTIONS]

OPTIONS:
    --port <PORT>          Port to listen on                    [default: 5050]
    --data-dir <PATH>      Where config.json and secrets live   [default: /opt/mindash]
    --static-dir <PATH>    Where the web bundle is served from  [default: static]

    --set-password         Read a password from stdin and store a hash, then exit.
                           Use this before exposing MinDash to a network.
    --print-config-dir     Print the resolved data directory and exit.
    -h, --help             Print this help and exit.
    -V, --version          Print the version and exit.

ENVIRONMENT:
    RUST_LOG               Log filter, e.g. RUST_LOG=mindash=debug
    MINDASH_SSH_KEY        Identity file to use for every SSH connection. Useful
                           for a throwaway key in testing; normally you would rely
                           on your own ~/.ssh/config instead.
";

struct Args {
    port: u16,
    data_dir: PathBuf,
    static_dir: PathBuf,
    action: Action,
}

enum Action {
    Serve,
    Help,
    Version,
    SetPassword,
    PrintConfigDir,
}

/// Parse the command line.
///
/// A flag that needs a value and does not get one is an error rather than a
/// silent fall back to the default: being told `--port` with no number and
/// quietly listening on 5050 anyway is how you end up wondering why nothing
/// connects.
fn parse_args() -> Result<Args, String> {
    let mut args = std::env::args().skip(1);
    let mut parsed = Args {
        port: 5050,
        data_dir: PathBuf::from("/opt/mindash"),
        static_dir: PathBuf::from("static"),
        action: Action::Serve,
    };

    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--port" => {
                let value = args.next().ok_or("--port needs a number")?;
                parsed.port = value
                    .parse()
                    .map_err(|_| format!("--port: not a port number: {value}"))?;
            }
            "--data-dir" => {
                parsed.data_dir = PathBuf::from(args.next().ok_or("--data-dir needs a path")?);
            }
            "--static-dir" => {
                parsed.static_dir = PathBuf::from(args.next().ok_or("--static-dir needs a path")?);
            }
            "--set-password" => parsed.action = Action::SetPassword,
            "--print-config-dir" => parsed.action = Action::PrintConfigDir,
            "--help" | "-h" => parsed.action = Action::Help,
            "--version" | "-V" => parsed.action = Action::Version,
            other => return Err(format!("unknown argument: {other}")),
        }
    }
    Ok(parsed)
}

#[tokio::main]
async fn main() -> std::io::Result<()> {
    let args = match parse_args() {
        Ok(a) => a,
        Err(e) => {
            eprintln!("mindash: {e}\n\n{USAGE}");
            std::process::exit(2);
        }
    };

    match args.action {
        Action::Help => {
            println!("{USAGE}");
            return Ok(());
        }
        Action::Version => {
            println!("mindash {}", env!("CARGO_PKG_VERSION"));
            return Ok(());
        }
        Action::PrintConfigDir => {
            println!("{}", args.data_dir.display());
            return Ok(());
        }
        Action::SetPassword => return set_password(&args.data_dir).await,
        Action::Serve => {}
    }

    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| "mindash=info,tower_http=warn".into()),
        )
        .init();

    let state = Arc::new(AppState::new(args.data_dir.clone(), args.static_dir.clone()).await?);
    let app = router(state.clone());

    let listener = tokio::net::TcpListener::bind(("0.0.0.0", args.port)).await?;
    tracing::info!(
        "MinDash {} listening on http://0.0.0.0:{}  (data {}, static {})",
        env!("CARGO_PKG_VERSION"),
        args.port,
        args.data_dir.display(),
        args.static_dir.display()
    );
    if state.auth_enabled() {
        tracing::info!("admin auth is enabled");
    } else {
        tracing::warn!(
            "admin auth is DISABLED - anyone who can reach this port has full \
             control. Set a password with `mindash --set-password`, and run \
             behind a VPN."
        );
    }

    axum::serve(listener, app).await?;
    Ok(())
}

/// Read a password and store its hash.
///
/// Taken from stdin rather than an argument so it does not land in the shell
/// history or in `ps` output. The hash is written with restrictive permissions
/// where the platform supports them.
async fn set_password(data_dir: &PathBuf) -> std::io::Result<()> {
    let mut password = String::new();
    std::io::stdin().read_to_string(&mut password)?;
    // A trailing newline from `echo` is not part of the password.
    let password = password.trim_end_matches(['\n', '\r']);

    if password.is_empty() {
        eprintln!("mindash: no password on stdin. Try: printf 'secret' | mindash --set-password");
        std::process::exit(2);
    }
    if password.chars().count() < 8 {
        eprintln!("mindash: password must be at least 8 characters");
        std::process::exit(2);
    }

    tokio::fs::create_dir_all(data_dir).await?;
    let state = AppState::new(data_dir.clone(), PathBuf::from("static")).await?;
    state.set_password(password).await?;

    println!("password set for {}", data_dir.display());
    Ok(())
}
