#!/bin/sh
set -eu
test_root=$(CDPATH= cd -- "$(dirname -- "$0")/../.." && pwd)
test_binary=$(mktemp "${TMPDIR:-/tmp}/evolution-ecal-cache.XXXXXX")
trap 'rm -f -- "$test_binary"' EXIT HUP INT TERM
${CC:-cc} -std=c11 -Wall -Wextra -Werror \
    "$test_root/native/tests/ecal_snapshot_cache_test.c" \
    $(pkg-config --cflags --libs gio-2.0) -o "$test_binary"
"$test_binary"
