mod connection;
mod decode;
mod emitter;
mod policy;

use anyhow::Result;
use clap::{Args, Parser, Subcommand};
use policy::PlayPolicy;

#[derive(Parser)]
#[command(about = "Stream iOS microphone audio to an ALSA playback device")]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Args, Clone)]
struct ConnArgs {
    /// WiFi IP address (omit for USB)
    #[arg(short = 'H', long)]
    host: Option<String>,
    /// Port number
    #[arg(short = 'P', long, default_value_t = 4747)]
    port: u16,
    /// ALSA playback device to write to
    #[arg(short = 'D', long, default_value = "default")]
    device: String,
}

#[derive(Subcommand)]
enum Command {
    /// Pre-fill buffer, then play everything in order (for recording)
    Record {
        #[command(flatten)]
        conn: ConnArgs,
        /// Pre-fill buffer size in milliseconds
        #[arg(short, long, default_value_t = 2000)]
        buffer: u32,
    },
    /// Drop frames when latency exceeds target
    Drop {
        #[command(flatten)]
        conn: ConnArgs,
        /// Max delay in milliseconds before dropping frames
        #[arg(short, long, default_value_t = 300)]
        delay: u32,
    },
}

#[tokio::main]
async fn main() -> Result<()> {
    let cli = Cli::parse();

    let (conn, play_policy) = match cli.command {
        Command::Record { conn, buffer } => (conn, PlayPolicy::Record { buffer_ms: buffer }),
        Command::Drop { conn, delay } => (
            conn,
            PlayPolicy::Drop {
                max_delay_ms: delay,
            },
        ),
    };

    let device = conn.device.clone();
    let host = conn.host.clone();
    let port = conn.port;

    tokio::select! {
        result = async {
            let stream = if let Some(host) = &host {
                connection::connect_wifi(host, port).await?
            } else {
                connection::connect_usb(port).await?
            };

            emitter::run(stream, play_policy, &device).await
        } => result,
        signal = tokio::signal::ctrl_c() => {
            signal?;
            Ok(())
        }
    }
}
