//! OTDEL API binary.
//!
//! Subcommands (no external CLI dependency — there are five of them and they take no
//! options beyond the environment):
//!
//! * `serve` (default) — run the HTTP API;
//! * `migrate` — apply migrations with the **migration** role
//!   (`OTDEL_DATABASE_ADMIN_URL`), never with the runtime role;
//! * `bootstrap` — provision the configured bureau (also with the migration role);
//! * `hash-password` — read a password from stdin and print its Argon2 hash, so
//!   `scripts/dev-init.sh` can write the hash into the local env file without the
//!   password ever appearing in a command line or in the output;
//! * `check-config` — load and validate the configuration, print the redacted result.

use std::io::{IsTerminal, Read};
use std::process::ExitCode;

use anyhow::{bail, Context, Result};
use otdel_core::config::Config;
use otdel_core::secret;
use tracing_subscriber::EnvFilter;

#[tokio::main]
async fn main() -> ExitCode {
    let command = std::env::args()
        .nth(1)
        .unwrap_or_else(|| "serve".to_owned());

    let result = match command.as_str() {
        "serve" => serve().await,
        "migrate" => migrate().await,
        "bootstrap" => bootstrap().await,
        "hash-password" => hash_password(),
        "check-config" => check_config(),
        "-h" | "--help" | "help" => {
            print_usage();
            Ok(())
        }
        other => {
            eprintln!("unknown command `{other}`");
            print_usage();
            return ExitCode::from(2);
        }
    };

    match result {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            // `{error:#}` prints the whole context chain; configuration errors name the
            // variable at fault, never its value.
            eprintln!("otdel-api: {error:#}");
            ExitCode::FAILURE
        }
    }
}

fn print_usage() {
    eprintln!(
        "usage: otdel-api [serve|migrate|bootstrap|hash-password|check-config]\n\
         \n\
         Configuration comes from the environment (see .env.example and docs/backend-1a.md).\n\
         `migrate` and `bootstrap` require OTDEL_DATABASE_ADMIN_URL (migration role);\n\
         `serve` uses OTDEL_DATABASE_URL (restricted runtime role)."
    );
}

fn init_tracing(filter: &str) {
    let env_filter = EnvFilter::try_from_default_env()
        .or_else(|_| EnvFilter::try_new(filter))
        .unwrap_or_else(|_| EnvFilter::new("info"));

    // No secrets are ever put into spans/fields; see otdel-core::config for redaction.
    let _ = tracing_subscriber::fmt()
        .with_env_filter(env_filter)
        .with_target(true)
        .try_init();
}

fn load_config() -> Result<Config> {
    Config::from_env().map_err(|error| anyhow::anyhow!("{}", error.message))
}

async fn serve() -> Result<()> {
    let config = load_config()?;
    init_tracing(&config.log_filter);
    tracing::debug!(config = ?config, "configuration loaded");

    let state = otdel_api::build_state(config)
        .await
        .map_err(|error| anyhow::anyhow!("{}", error.message))?;

    otdel_api::serve(state)
        .await
        .map_err(|error| anyhow::anyhow!("{}", error.message))
}

async fn migrate() -> Result<()> {
    let config = load_config()?;
    init_tracing(&config.log_filter);

    let admin_url = config
        .require_admin_database_url()
        .map_err(|error| anyhow::anyhow!("{}", error.message))?;

    otdel_db::run_migrations(admin_url)
        .await
        .context("applying migrations")?;
    tracing::info!("migrations applied");
    Ok(())
}

async fn bootstrap() -> Result<()> {
    let config = load_config()?;
    init_tracing(&config.log_filter);

    let admin_url = config
        .require_admin_database_url()
        .map_err(|error| anyhow::anyhow!("{}", error.message))?;

    let bureau_id = otdel_db::bootstrap_bureau(admin_url, &config.bureau_slug)
        .await
        .context("provisioning the bureau")?;
    tracing::info!(bureau = %config.bureau_slug, bureau_id = %bureau_id, "bureau is provisioned");
    Ok(())
}

/// Reads the password from stdin (never from argv, which is visible in `ps`) and prints
/// only the PHC hash.
fn hash_password() -> Result<()> {
    if std::io::stdin().is_terminal() {
        bail!("pipe the password into stdin, e.g. `printf '%s' \"$PASSWORD\" | otdel-api hash-password`");
    }
    let mut password = String::new();
    std::io::stdin()
        .read_to_string(&mut password)
        .context("reading the password from stdin")?;
    let password = password.trim_end_matches(['\n', '\r']);

    let hash =
        secret::hash_password(password).map_err(|error| anyhow::anyhow!("{}", error.message))?;
    println!("{hash}");
    Ok(())
}

fn check_config() -> Result<()> {
    let config = load_config()?;
    // Debug for Config redacts every secret.
    println!("{config:?}");
    Ok(())
}
