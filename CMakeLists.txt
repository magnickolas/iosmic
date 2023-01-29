cmake_minimum_required(VERSION 3.17)
project(droidcam
  VERSION 1.0
)
file(GLOB SOURCES "src/*.cc")
add_executable(droidcam ${SOURCES})
target_compile_features(droidcam PRIVATE cxx_std_20)

target_link_libraries(droidcam PRIVATE
  asound
  usbmuxd-2.0
  avcodec
  avutil
  swresample
  obs)
target_include_directories(droidcam PRIVATE include)
target_compile_features(
  droidcam PRIVATE
  cxx_std_17)
target_compile_options(
  droidcam PRIVATE
  -Wall -Wextra -pedantic -O2)
