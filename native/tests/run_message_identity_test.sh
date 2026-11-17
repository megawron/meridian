#!/bin/sh
set -eu
test_root=$(CDPATH= cd -- "$(dirname -- "$0")/../.." && pwd)
test_binary=$(mktemp "${TMPDIR:-/tmp}/meridian-message-identity.XXXXXX")
trap 'rm -f -- "$test_binary"' EXIT HUP INT TERM
# pkg-config intentionally supplies compiler arguments as separate words.
${CC:-cc} -std=c11 -Wall -Wextra -Werror \
    "$test_root/native/tests/message_identity_test.c" \
    $(pkg-config --cflags --libs camel-1.2 libedataserver-1.2 gio-2.0) -o "$test_binary"
"$test_binary"
