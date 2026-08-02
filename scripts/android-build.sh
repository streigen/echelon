#!/bin/bash
set -e # Exit on cmd fail

# Set defaults (Debug)
CARGO_FLAGS=""
GRADLE_TASK="assembleDebug"
# Parse arguments
for arg in "$@"; do
  if [ "$arg" == "--release-compact" ]; then
    CARGO_FLAGS="--profile release-compact"
    GRADLE_TASK="assembleRelease"
    BUILD_TYPE="release"
  elif [ "$arg" == "--release" ]; then
    CARGO_FLAGS="--release"
    GRADLE_TASK="assembleRelease"
  fi
done

echo "cargo flags: $CARGO_FLAGS"
echo "gradle task: $GRADLE_TASK"
echo "Cleaning stale native libs..."
rm -r android/app/src/main/jniLibs/*
echo "Running Cargo NDK..."
# $CARGO_FLAGS is intentionally unquoted so it expands to multiple arguments correctly
cargo ndk -t arm64-v8a -t x86_64 -o android/app/src/main/jniLibs build $CARGO_FLAGS --lib

echo "Running Gradle $GRADLE_TASK..."
cd android
gradle $GRADLE_TASK
