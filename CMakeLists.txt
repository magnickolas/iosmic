cmake_minimum_required(VERSION 3.17)
project(droidcam
  VERSION 1.0
)
add_executable(droidcam src/connection.cc src/droidcam-cli.cc src/ffmpeg_decode.cc src/net.cc src/plugin.cc src/usb.cc src/loopback.cc)

target_link_libraries(droidcam PRIVATE
  asound
  usbmuxd-2.0
  avcodec
  avutil
  obs)
target_include_directories(droidcam PRIVATE)
target_compile_features(
  droidcam PRIVATE
  cxx_std_17)
target_compile_options(
  droidcam PRIVATE
  -Wall -Wextra -pedantic -O2)
