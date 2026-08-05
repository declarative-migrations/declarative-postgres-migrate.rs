use anyhow::{bail, Result};
use dpm::server::ServerConfig;

fn print_help() {
    println!(
        concat!(
            "dpm-server {}\n\n",
            "USAGE:\n    dpm-server\n\n",
            "CONFIGURATION:\n",
            "    DPM_SERVER_BIND\n",
            "    DPM_SERVER_TOKEN\n",
            "    DPM_SERVER_DATABASES_JSON\n",
            "    DPM_SERVER_ALLOW_APPLY\n",
            "    DPM_SERVER_MAX_BODY_BYTES\n",
            "    DPM_SERVER_MAX_IN_FLIGHT\n"
        ),
        env!("CARGO_PKG_VERSION")
    );
}

#[tokio::main]
async fn main() -> Result<()> {
    let mut args = std::env::args().skip(1);
    if let Some(argument) = args.next() {
        match argument.as_str() {
            "-h" | "--help" => {
                print_help();
                return Ok(());
            }
            "-V" | "--version" => {
                println!("dpm-server {}", env!("CARGO_PKG_VERSION"));
                return Ok(());
            }
            _ => bail!("unknown argument {argument:?}; use --help"),
        }
    }
    if let Some(argument) = args.next() {
        bail!("unexpected argument {argument:?}; use --help");
    }
    dpm::server::run(ServerConfig::from_env()?).await
}
