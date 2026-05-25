mod connection;
mod debug;
mod decode;
mod emitter;
mod policy;
mod resample;

use anyhow::Result;
use clap::{Args, Parser};
use connection::Stream;
use policy::PlayPolicy;
use std::time::Duration;

#[derive(Parser)]
#[command(about = "Stream iOS microphone audio to an ALSA playback device")]
struct Cli {
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
    /// Pre-fill buffer size in milliseconds (default: 50 for USB, 100 for WiFi)
    #[arg(short, long, value_parser = clap::value_parser!(u32).range(50..=500))]
    buffer: Option<u32>,
}

#[tokio::main]
async fn main() -> Result<()> {
    let cli = Cli::parse();

    let default_buffer = if cli.run.conn.host.is_some() { 100 } else { 50 };
    let play_policy = PlayPolicy {
        buffer_ms: cli.run.buffer.unwrap_or(default_buffer),
    };

    loop {
        let result = tokio::select! {
            result = async {
                let stream = connect(&cli.run.conn).await?;
                emitter::run(stream, play_policy.clone(), &cli.run.device).await
            } => result,
            signal = tokio::signal::ctrl_c() => {
                signal?;
                return Ok(());
            }
        };
        match result {
            Ok(()) => return Ok(()),
            Err(e) => {
                eprintln!("Disconnected: {e:#}");
                eprintln!("Reconnecting in 1s...");
                tokio::select! {
                    _ = tokio::time::sleep(Duration::from_secs(1)) => {}
                    signal = tokio::signal::ctrl_c() => {
                        signal?;
                        return Ok(());
                    }
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
