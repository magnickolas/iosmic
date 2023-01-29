cmake_minimum_required(VERSION 3.17)
project(iosmic
  VERSION 1.0
)
file(GLOB SOURCES "src/*.cc")
add_executable(iosmic ${SOURCES})
target_compile_features(iosmic PRIVATE cxx_std_20)

target_link_libraries(iosmic PRIVATE
  asound
  usbmuxd-2.0
  avcodec
  avutil
  swresample)
target_include_directories(iosmic PRIVATE include)
target_compile_features(
  iosmic PRIVATE
  cxx_std_17)
target_compile_options(
  iosmic PRIVATE
  -Wall -Wextra -pedantic -O2)
