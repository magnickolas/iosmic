# iosmic

A CLI utility to use iOS device as an external mic on Linux.
This codebase is mostly compiled from [droidcam](https://github.com/dev47apps/droidcam), [droidcam-obs-plugin](https://github.com/dev47apps/droidcam-obs-plugin) and [obs-studio](https://github.com/obsproject/obs-studio).

## Requirements

On the iOS device the application [DroidCam Webcam & OBS Camera](https://apps.apple.com/us/app/droidcam-webcam-obs-camera) has to be installed.

Required packages: alsa, usbmuxd, ffmpeg

## Build

```console
git clone https://github.com/magnickolas/iosmic
cd iosmic
mkdir build
cmake -S. -Bbuild
cmake --build build
cp ./build/iosmic ~/.local/bin/iosmic
```

## Usage

- Connect through USB
  ```console
  iosmic usb 4747
  ```
- Connect through Wi-Fi
  ```console
  iosmic <ip> 4747
  ```
