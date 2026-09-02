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
./target/release/iosmic record --buffer 5
```

Select a different ALSA device with `--device`:

```sh
./target/release/iosmic record --device default --buffer 5
./target/release/iosmic record --device plughw:0,0 --buffer 5
./target/release/iosmic record --device pulse --buffer 5
./target/release/iosmic record --device pipewire:NODE=ios_mic_sink --buffer 5
```

If you want applications to see the iPhone as a microphone, create and route a
virtual source yourself. For example, with PipeWire/PulseAudio tools:

```sh
pactl load-module module-null-sink sink_name=ios_mic_sink sink_properties=device.description=ios_mic_sink
pactl load-module module-remap-source master=ios_mic_sink.monitor source_name=ios_mic source_properties=device.description=ios_mic
```

Then run:

```sh
./target/release/iosmic record --device pipewire:NODE=ios_mic_sink --buffer 5
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
scripts/iosmic-source -- record --buffer 5
scripts/iosmic-source --source-name phone_mic -- record --host 192.168.0.114 --buffer 5
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
scripts/iosmic-source --source-name phone_mic --sink-name phone_sink --device pulse --binary ./target/release/iosmic -- record --buffer 5
```

## Usage

Show top-level help:

```sh
./target/release/iosmic --help
```

### USB

Run in stable recording mode over USB:

```sh
./target/release/iosmic record --buffer 5
```

Run in latency-bounded mode over USB:

```sh
./target/release/iosmic drop --delay 50
```

### Wi-Fi

Run in stable recording mode over Wi-Fi:

```sh
./target/release/iosmic record --host 192.168.0.114 --buffer 5
```

Run in latency-bounded mode over Wi-Fi:

```sh
./target/release/iosmic drop --host 192.168.0.114 --delay 50
```

The default port is `4747`. Override it with:

```sh
./target/release/iosmic record --host 192.168.0.114 --port 4747 --buffer 5
```

## Playback Modes

### `record`

`record` pre-fills a small buffer, then writes every decoded audio frame in
order. If the audio device cannot accept data immediately, writes block until
there is space.

Use this mode when continuity matters:

```sh
./target/release/iosmic record --buffer 5
```

`--buffer` is the initial prebuffer size in milliseconds. Smaller values reduce
latency but leave less room for Wi-Fi, scheduler, and audio-server jitter.

### `drop`

`drop` checks ALSA's queued playback delay before each write. If the delay is
above `--delay`, it skips the whole decoded audio chunk instead of adding more
latency.

Use this mode when staying live matters more than perfect continuity:

```sh
./target/release/iosmic drop --delay 50
```

Very small values such as `--delay 5` are usually too aggressive. AAC frames and
normal desktop audio scheduling can already exceed that budget, so the result
will likely be glitchy.

## Debugging Timing

Print decoded packet timing:

```sh
IOSMIC_PTS_DEBUG=1 ./target/release/iosmic record --host 192.168.0.114 --buffer 5
```

Print arrival timing and ALSA queued delay:

```sh
IOSMIC_TIMING_DEBUG=1 ./target/release/iosmic drop --host 192.168.0.114 --delay 50
```

## Troubleshooting

### Applications Do Not See A Microphone

`iosmic` writes to an ALSA playback device. It does not create an application
input device. Create a virtual source with PipeWire or PulseAudio and route
`iosmic` into its backing sink.

### `drop --delay 5` Is Glitchy

Use a larger delay:

```sh
./target/release/iosmic drop --host 192.168.0.114 --delay 50
```

Then reduce it gradually. The drop policy discards complete decoded chunks, so
very low delay targets can produce audible gaps.

### Wi-Fi Is Unstable

Prefer USB or increase the buffer/delay:

```sh
./target/release/iosmic record --buffer 20
./target/release/iosmic drop --delay 100
```

Wi-Fi jitter directly affects how small the usable latency budget can be.

## CLI Reference

```text
Usage: iosmic <COMMAND>

Commands:
  record  Pre-fill buffer, then play everything in order (for recording)
  drop    Drop frames when latency exceeds target
  help    Print this message or the help of the given subcommand(s)
```

Common options:

```text
-H, --host <HOST>      WiFi IP address (omit for USB)
-P, --port <PORT>      Port number [default: 4747]
-D, --device <DEVICE>  ALSA playback device to write to [default: default]
```

`record` option:

```text
-b, --buffer <BUFFER>  Pre-fill buffer size in milliseconds [default: 2000]
```

`drop` option:

```text
-d, --delay <DELAY>  Max delay in milliseconds before dropping frames [default: 300]
```
