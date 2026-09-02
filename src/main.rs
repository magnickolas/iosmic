mod connection;
mod debug;
mod decode;
mod emitter;
mod error;
mod jitter;
mod policy;
mod renderer;
mod resample;
mod sink;
mod virtual_source;

use anyhow::Result;
use clap::{Args, Parser};
use connection::Stream;
use error::ConfigurationError;
use policy::PlayPolicy;
use sink::{AlsaSinkFactory, MissingPulsePlugin, SinkFactory};
use std::sync::Arc;
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
    /// Maximum decoded jitter-buffer size in milliseconds (default: derived)
    #[arg(long, value_parser = clap::value_parser!(u32).range(100..=2000))]
    max_buffer: Option<u32>,
    /// Requested ALSA client-buffer size in milliseconds
    #[arg(long, default_value_t = 40, value_parser = clap::value_parser!(u32).range(20..=200))]
    alsa_buffer: u32,
    /// Reconnect after this much continuous post-start starvation, in milliseconds
    #[arg(long, default_value_t = 1500, value_parser = clap::value_parser!(u32).range(250..=5000))]
    starvation_reconnect: u32,
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
        target_buffer_ms: cli.run.buffer,
        default_target_buffer_ms: default_buffer,
        maximum_buffer_ms: cli.run.max_buffer,
        alsa_buffer_ms: cli.run.alsa_buffer,
        starvation_reconnect_ms: cli.run.starvation_reconnect,
    };
    let sink_factory: Arc<dyn SinkFactory> = Arc::new(AlsaSinkFactory::new(device));
    let mut reconnect_count = 0u64;
    let process_timing = debug::TimingDebug::from_env();

    loop {
        let result = tokio::select! {
            result = async {
                let stream = connect(&cli.run.conn).await?;
                emitter::run(stream, play_policy.clone(), Arc::clone(&sink_factory)).await
            } => result,
            _ = shutdown.recv() => {
                return Ok(());
            }
        };
        match result {
            Ok(()) => return Ok(()),
            Err(e) => {
                if e.is::<MissingPulsePlugin>() || e.is::<ConfigurationError>() {
                    process_timing.log_render("non_retryable_error_count=1");
                    return Err(e);
                }
                reconnect_count += 1;
                process_timing
                    .log_render(format_args!("network_reconnect_count={reconnect_count}"));
                eprintln!("Disconnected (reconnect #{reconnect_count}): {e:#}");
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

    #[test]
    fn latency_options_parse_at_their_documented_limits() {
        let cli = Cli::try_parse_from([
            "iosmic",
            "--buffer",
            "50",
            "--max-buffer",
            "100",
            "--alsa-buffer",
            "20",
            "--starvation-reconnect",
            "5000",
        ])
        .unwrap();
        assert_eq!(cli.run.buffer, Some(50));
        assert_eq!(cli.run.max_buffer, Some(100));
        assert_eq!(cli.run.alsa_buffer, 20);
        assert_eq!(cli.run.starvation_reconnect, 5000);
    }

    #[test]
    fn latency_option_ranges_are_enforced() {
        assert!(Cli::try_parse_from(["iosmic", "--buffer", "49"]).is_err());
        assert!(Cli::try_parse_from(["iosmic", "--max-buffer", "2001"]).is_err());
        assert!(Cli::try_parse_from(["iosmic", "--alsa-buffer", "19"]).is_err());
        assert!(Cli::try_parse_from(["iosmic", "--starvation-reconnect", "249"]).is_err());
    }

    #[test]
    fn obsolete_alsa_delay_threshold_is_rejected() {
        assert!(Cli::try_parse_from(["iosmic", "--latency-reconnect-threshold", "100"]).is_err());
    }
}
