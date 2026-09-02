mod connection;
mod debug;
mod decode;
mod emitter;
mod policy;
mod resample;
mod sink;
mod virtual_source;

use anyhow::Result;
use clap::{Args, Parser};
use connection::Stream;
use policy::PlayPolicy;
use sink::{AlsaSink, AudioSink, MissingPulsePlugin};
use std::time::Duration;
use virtual_source::VirtualSource;

#[derive(Parser)]
#[command(
    about = "Expose iOS microphone audio to Linux applications",
    after_help = "Repository: https://github.com/magnickolas/iosmic"
)]
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
    /// Write directly to an ALSA playback device instead of creating a microphone source
    #[arg(short = 'D', long)]
    device: Option<String>,
    /// Internal PulseAudio/PipeWire microphone source name (default: iosmic)
    #[arg(long, conflicts_with = "device", value_name = "NAME")]
    source_name: Option<String>,
    /// Human-readable microphone label shown in applications (default: iOS Microphone)
    #[arg(long, conflicts_with = "device", value_name = "TEXT")]
    source_description: Option<String>,
    /// Backing PulseAudio/PipeWire sink name (default: <source-name>_sink)
    #[arg(long, conflicts_with = "device", value_name = "NAME")]
    sink_name: Option<String>,
    /// Pre-fill buffer size in milliseconds (default: 50 for USB, 100 for WiFi)
    #[arg(short, long, value_parser = clap::value_parser!(u32).range(50..=500))]
    buffer: Option<u32>,
    /// Reconnect when latency exceeds this many milliseconds (0 = disabled)
    #[arg(long, default_value_t = 0)]
    latency_reconnect_threshold: u32,
}

#[tokio::main]
async fn main() -> Result<()> {
    let cli = Cli::parse();
    let mut shutdown = ShutdownSignals::new()?;

    let (_virtual_source, device) = match &cli.run.device {
        Some(device) => (None, device.clone()),
        None => (
            Some(VirtualSource::create(
                cli.run.source_name.as_deref().unwrap_or("iosmic"),
                cli.run.sink_name.as_deref(),
                cli.run.source_description.as_deref(),
            )?),
            "pulse".to_owned(),
        ),
    };

    let default_buffer = if cli.run.conn.host.is_some() { 100 } else { 50 };
    let play_policy = PlayPolicy {
        buffer_ms: cli.run.buffer.unwrap_or(default_buffer),
        latency_reconnect_ms: cli.run.latency_reconnect_threshold,
    };

    loop {
        let result = tokio::select! {
            result = async {
                let stream = connect(&cli.run.conn).await?;
                let device = device.clone();
                let buffer_us = play_policy.buffer_us();
                emitter::run(stream, play_policy.clone(), move |sample_rate| {
                    Ok(Box::new(AlsaSink::open(&device, sample_rate, buffer_us)?)
                        as Box<dyn AudioSink>)
                }).await
            } => result,
            _ = shutdown.recv() => {
                return Ok(());
            }
        };
        match result {
            Ok(()) => return Ok(()),
            Err(e) => {
                if e.is::<MissingPulsePlugin>() {
                    return Err(e);
                }
                eprintln!("Disconnected: {e:#}");
                eprintln!("Reconnecting in 1s...");
                tokio::select! {
                    _ = tokio::time::sleep(Duration::from_secs(1)) => {}
                    _ = shutdown.recv() => {
                        return Ok(());
                    }
                }
            }
        }
    }
}

#[cfg(unix)]
struct ShutdownSignals {
    interrupt: tokio::signal::unix::Signal,
    terminate: tokio::signal::unix::Signal,
}

#[cfg(unix)]
impl ShutdownSignals {
    fn new() -> Result<Self> {
        Ok(Self {
            interrupt: tokio::signal::unix::signal(tokio::signal::unix::SignalKind::interrupt())?,
            terminate: tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())?,
        })
    }

    async fn recv(&mut self) {
        tokio::select! {
            _ = self.interrupt.recv() => {}
            _ = self.terminate.recv() => {}
        }
    }
}

#[cfg(not(unix))]
struct ShutdownSignals;

#[cfg(not(unix))]
impl ShutdownSignals {
    fn new() -> Result<Self> {
        Ok(Self)
    }

    async fn recv(&mut self) {
        let _ = tokio::signal::ctrl_c().await;
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

#[cfg(test)]
mod tests {
    use super::{Cli, Parser};

    #[test]
    fn device_conflicts_with_source_configuration() {
        assert!(
            Cli::try_parse_from([
                "iosmic",
                "--device",
                "hw:0,0",
                "--source-description",
                "iOS Microphone",
            ])
            .is_err()
        );
    }

    #[test]
    fn source_configuration_does_not_require_a_custom_source_name() {
        assert!(Cli::try_parse_from(["iosmic", "--source-description", "Office iPhone"]).is_ok());
    }
}
