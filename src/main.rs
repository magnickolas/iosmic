mod connection;
mod debug;
mod decode;
mod emitter;
mod measure;
mod policy;

use anyhow::Result;
use clap::{Args, Parser, Subcommand};
use connection::Stream;
use policy::PlayPolicy;
use std::time::Duration;

#[derive(Parser)]
#[command(
    about = "Stream iOS microphone audio to an ALSA playback device",
    args_conflicts_with_subcommands = true
)]
struct Cli {
    #[command(subcommand)]
    command: Option<Command>,

    #[command(flatten)]
    run: RunArgs,
}

#[derive(Args, Clone)]
struct ConnArgs {
    /// WiFi IP address (omit for USB)
    #[arg(short = 'H', long)]
    host: Option<String>,
    /// Port number
    #[arg(short = 'P', long, default_value_t = 4747)]
    port: u16,
}

#[derive(Args, Clone)]
struct RunArgs {
    #[command(flatten)]
    conn: ConnArgs,
    /// ALSA playback device to write to
    #[arg(short = 'D', long, default_value = "default")]
    device: String,
    /// Pre-fill buffer size in milliseconds (default: 20 for USB, 100 for WiFi)
    #[arg(short, long)]
    buffer: Option<u32>,
}

#[derive(Subcommand)]
enum Command {
    /// Measure packet jitter
    Measure {
        #[command(flatten)]
        conn: ConnArgs,
        /// Measurement duration in seconds
        #[arg(short, long, default_value_t = 30)]
        seconds: u64,
    },
}

#[tokio::main]
async fn main() -> Result<()> {
    let cli = Cli::parse();

    match cli.command {
        Some(Command::Measure { conn, seconds }) => {
            anyhow::ensure!(seconds > 0, "--seconds must be greater than zero");
            let duration = Duration::from_secs(seconds);
            tokio::select! {
                result = async {
                    let stream = connect(&conn).await?;
                    measure::measure(stream, duration).await
                } => result,
                signal = tokio::signal::ctrl_c() => {
                    signal?;
                    Ok(())
                }
            }
        }
        None => {
            let default_buffer = if cli.run.conn.host.is_some() { 100 } else { 20 };
            let play_policy = PlayPolicy {
                buffer_ms: cli.run.buffer.unwrap_or(default_buffer),
            };

            tokio::select! {
                result = async {
                    let stream = connect(&cli.run.conn).await?;
                    emitter::run(stream, play_policy, &cli.run.device).await
                } => result,
                signal = tokio::signal::ctrl_c() => {
                    signal?;
                    Ok(())
                }
            }
        }
    }
}

async fn connect(conn: &ConnArgs) -> Result<Stream> {
    if let Some(host) = &conn.host {
        connection::connect_wifi(host, conn.port).await
    } else {
        #[cfg(feature = "usb")]
        {
            connection::connect_usb(conn.port).await
        }
        #[cfg(not(feature = "usb"))]
        {
            anyhow::bail!("USB support not compiled in (enable the `usb` feature)")
        }
    }
}
