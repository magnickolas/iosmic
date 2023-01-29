#!/bin/bash
function build() {
	# rm -rf ./build/*
	cmake -S. -Bbuild -GNinja -DCMAKE_EXPORT_COMPILE_COMMANDS=1
	cmake --build build && ./build/iosmic usb 4747
}

build
