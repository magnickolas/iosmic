# iosmic

Stream microphone audio from an iOS device to a local ALSA playback device.

`iosmic` connects to the iOS app over USB or Wi-Fi, decodes the AAC audio stream,
and writes PCM audio to the ALSA device selected with `--device`. Audio routing is
managed by the user through ALSA, PulseAudio, PipeWire, or desktop audio tools.

## Requirements

- Linux with ALSA.
- Rust 2024 toolchain.
- For USB mode: `usbmuxd` access to the iOS device.
- For Wi-Fi mode: the iOS device must be reachable on the network.
- If routing into desktop applications as a microphone, a user-created virtual
  sink/source in PipeWire or PulseAudio.

## Build

```sh
cargo build --release
```

The optimized binary is written to:

```sh
./target/release/iosmic
```

For development:

```sh
cargo check
cargo test
cargo clippy --all-targets --all-features
```

## Audio Routing

By default, `iosmic` writes to ALSA's `default` playback device:

```sh
./target/release/iosmic --buffer 5
```

Select a different ALSA device with `--device`:

```sh
./target/release/iosmic --device default --buffer 5
./target/release/iosmic --device plughw:0,0 --buffer 5
./target/release/iosmic --device pulse --buffer 5
./target/release/iosmic --device pipewire:NODE=ios_mic_sink --buffer 5
```

If you want applications to see the iPhone as a microphone, create and route a
virtual source yourself. For example, with PipeWire/PulseAudio tools:

```sh
pactl load-module module-null-sink sink_name=ios_mic_sink sink_properties=device.description=ios_mic_sink
pactl load-module module-remap-source master=ios_mic_sink.monitor source_name=ios_mic source_properties=device.description=ios_mic
```

Then run:

```sh
./target/release/iosmic --device pipewire:NODE=ios_mic_sink --buffer 5
```

In applications, select:

```text
ios_mic
```

The exact device string depends on your audio stack. `iosmic` does not create,
move, or remove audio server devices.

### Temporary Source Script

For convenience, `scripts/iosmic-source` creates a temporary source, runs
`iosmic`, then unloads only the modules it created when `iosmic` exits or the
script receives Ctrl-C/TERM.

```sh
scripts/iosmic-source -- --buffer 5
scripts/iosmic-source --source-name phone_mic -- --host 192.168.0.114 --buffer 5
```

By default the script creates:

```text
ios_mic_sink
ios_mic_sink.monitor
ios_mic
```

and runs `iosmic` with:

```text
--device pulse
```

with `PULSE_SINK=ios_mic_sink`. Override the app-facing source name, backing
sink name, ALSA device, or binary path with:

```sh
scripts/iosmic-source --source-name phone_mic --sink-name phone_sink --device pulse --binary ./target/release/iosmic -- --buffer 5
```

## Usage

Show help:

```sh
./target/release/iosmic --help
./target/release/iosmic measure --help
```

### USB

```sh
./target/release/iosmic --buffer 5
```

### Wi-Fi

```sh
./target/release/iosmic --host 192.168.0.114 --buffer 5
```

The default port is `4747`. Override it with:

```sh
./target/release/iosmic --host 192.168.0.114 --port 4747 --buffer 5
```

## Measuring Jitter

Use `measure` to inspect packet-arrival jitter without opening an ALSA device:

```sh
./target/release/iosmic measure --seconds 30
./target/release/iosmic measure --host 192.168.0.114 --seconds 30
```

It prints positive drift percentiles:

```text
total_seconds=30.0
warmup_seconds=2.0
measurement_seconds=28.0
positive_drift_p95_ms=2.817
positive_drift_p99_ms=4.103
positive_drift_max_ms=6.442
```

The first 10% of the run is treated as warmup, capped at 2 seconds, so startup
burstiness does not inflate the steady-state measurement.

## Playback

`iosmic` pre-fills a small buffer, then writes every decoded audio frame in
order. If the audio device cannot accept data immediately, writes block until
there is space.

`--buffer` is the initial prebuffer size in milliseconds. Smaller values reduce
latency but leave less room for Wi-Fi, scheduler, and audio-server jitter.

## Debugging Timing

Print decoded packet timing:

```sh
IOSMIC_PTS_DEBUG=1 ./target/release/iosmic --host 192.168.0.114 --buffer 5
```

Print arrival timing and ALSA queued delay:

```sh
IOSMIC_TIMING_DEBUG=1 ./target/release/iosmic --host 192.168.0.114 --buffer 5
```

## Troubleshooting

### Applications Do Not See A Microphone

`iosmic` writes to an ALSA playback device. It does not create an application
input device. Create a virtual source with PipeWire or PulseAudio and route
`iosmic` into its backing sink.

### Wi-Fi Is Unstable

Prefer USB or increase the buffer:

```sh
./target/release/iosmic --buffer 20
```

Wi-Fi jitter directly affects how small the usable latency budget can be.

## CLI Reference

```text
Usage: iosmic [OPTIONS]
       iosmic <COMMAND>

Usage: iosmic measure [OPTIONS]

Options:
  -H, --host <HOST>      WiFi IP address (omit for USB)
  -P, --port <PORT>      Port number [default: 4747]
  -D, --device <DEVICE>  ALSA playback device to write to [default: default]
  -b, --buffer <BUFFER>  Pre-fill buffer size in milliseconds [default: 2000]
  -h, --help             Print help

Measure options:
  -H, --host <HOST>        WiFi IP address (omit for USB)
  -P, --port <PORT>        Port number [default: 4747]
  -s, --seconds <SECONDS>  Measurement duration in seconds [default: 30]
```
