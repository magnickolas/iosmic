#!/bin/bash
function build() {
	# rm -rf ./build/*
	cmake -S. -Bbuild -GNinja -DCMAKE_EXPORT_COMPILE_COMMANDS=1
	cmake --build build && ./build/droidcam ios 4747
}

build
