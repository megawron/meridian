#!/bin/sh
# SPDX-License-Identifier: GPL-3.0-or-later
# Build the Rust binary for Meson's custom_target and copy the result to the
# path supplied by Meson.  Keeping the copy outside Meson's command array is
# important: `&&` must be interpreted by a shell, not passed to Cargo.

set -eu

export RUST_MIN_STACK="${RUST_MIN_STACK:-67108864}"

cargo_bin=$1
cargo_profile=$2
source_root=$3
target_dir=$4
output=$5

if [ "$cargo_profile" = "release" ]; then
    "$cargo_bin" build --release \
        --manifest-path "$source_root/Cargo.toml" \
        --target-dir "$target_dir"
else
    "$cargo_bin" build \
        --manifest-path "$source_root/Cargo.toml" \
        --target-dir "$target_dir"
fi

cp "$target_dir/$cargo_profile/meridian" "$output"
