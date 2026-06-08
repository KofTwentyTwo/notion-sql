#!/usr/bin/env sh
# Verify every tracked Rust source file begins with the SPDX + copyright banner.
# Driven off `git ls-files` so target/ and any generated .rs are never scanned.
set -eu

EXPECTED_SPDX="// SPDX-License-Identifier: MIT"
EXPECTED_COPYRIGHT="// Copyright (c) 2026 James Maes"
fail=0
count=0

for f in $(git ls-files '*.rs'); do
    count=$((count + 1))
    first=$(head -n 1 "$f")
    second=$(head -n 2 "$f" | tail -n 1)
    if [ "$first" != "$EXPECTED_SPDX" ] || [ "$second" != "$EXPECTED_COPYRIGHT" ]; then
        echo "MISSING/incorrect license header: $f"
        fail=1
    fi
done

if [ "$fail" -ne 0 ]; then
    echo "License header check FAILED. Every .rs file must start with:"
    echo "  $EXPECTED_SPDX"
    echo "  $EXPECTED_COPYRIGHT"
    exit 1
fi
echo "License header check passed ($count files)."
