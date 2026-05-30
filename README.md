# iosmic

Turn your iOS device into a microphone for Linux.

## Requirements

- Install and run this app on the iOS device: [DroidCam Webcam & OBS Camera](https://apps.apple.com/us/app/droidcam-webcam-obs-camera/id1510258102)
- For USB mode, install [usbmuxd](https://github.com/libimobiledevice/usbmuxd) on your Linux device.
- For virtual microphone mode, install PulseAudio or PipeWire with its PulseAudio compatibility server and ALSA's [`pulse` plugin](https://github.com/alsa-project/alsa-plugins/).
    ```
    apk add alsa-plugins-pulse
    apt install libasound2-plugins
    dnf install alsa-plugins-pulseaudio
    pacman -S alsa-plugins
    xbps-install -S alsa-plugins-pulseaudio
    zypper install alsa-plugins-pulse
    ```

## Install

```sh
git clone https://github.com/magnickolas/iosmic
cargo install --path iosmic
```

For Wi-Fi only, omit USB support:

```sh
cargo install --path iosmic --no-default-features
```

## Use

Receive audio from the iPhone over USB and play it through the default ALSA output:

```sh
iosmic
```

Connect over Wi-Fi:

```sh
iosmic --host 192.168.1.42
```

Choose an ALSA playback device:

```sh
iosmic --device hw:0,0
```

Expose the iPhone as a microphone named `ios_mic`:

```sh
iosmic --source-name ios_mic
```

Applications show this source as `iOS Mic`. To use a different display label:

```sh
iosmic --source-name ios_mic --source-description "Nicolas's iPhone"
```

For Wi-Fi microphone input:

```sh
iosmic --source-name iphone_mic --host 192.168.1.42
```

Select the named source in the application that should receive microphone audio. `iosmic` removes the temporary source and its backing sink when it exits, including on Ctrl-C and SIGTERM.

## Options

```text
-H, --host <HOST>                         iOS device IP address; omit for USB
-P, --port <PORT>                         DroidCam port (default: 4747)
-D, --device <DEVICE>                     ALSA playback device (default: default)
    --source-name <NAME>                  Temporary PipeWire/PulseAudio microphone name
    --source-description <TEXT>           Microphone label shown in applications (default: iOS Mic)
    --sink-name <NAME>                    Backing sink name (default: <source-name>_sink)
-b, --buffer <MILLISECONDS>               Pre-fill buffer: 50-500 ms
    --latency-reconnect-threshold <MS>    Reconnect above this latency; 0 disables it
```

`--source-name` uses ALSA's `pulse` device automatically and cannot be combined with `--device`. Source and sink names are internal IDs and may contain ASCII letters, numbers, `.`, `_`, and `-`. The source description is the human-readable label and supports normal printable text.
