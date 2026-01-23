#!/usr/bin/bash
# Differential testing: compare our VM output against lua5.2 and lua5.4
#
# Files with "-- DIFF: <reason>" comments are expected to have differences
# and won't cause the test to fail.

# Build first
cargo build --release 2>/dev/null

OUR_LUA="./target/release/lua"
LUA52="lua5.2"
LUA54="lua5.4"

# Temp files
OUR_OUT=$(mktemp)
LUA52_OUT=$(mktemp)
LUA54_OUT=$(mktemp)
trap "rm -f $OUR_OUT $LUA52_OUT $LUA54_OUT" EXIT

# Colors
RED='\033[0;31m'
GREEN='\033[0;32m'
YELLOW='\033[0;33m'
BLUE='\033[0;34m'
NC='\033[0m' # No Color

pass=0
fail=0
skip=0
expected_diff=0

echo "=== Differential Testing: our VM vs lua5.2 vs lua5.4 ==="
echo ""

for f in examples/*.lua; do
    name=$(basename "$f")

    # Skip files that use features not in standard Lua or are stress tests
    case "$name" in
        benchmark.lua|stress_*.lua|upvalue_stress.lua)
            echo -e "${YELLOW}SKIP${NC} $name (stress test)"
            skip=$((skip + 1))
            continue
            ;;
    esac

    # Check if file has DIFF annotation (expected difference)
    diff_reason=""
    if grep -q "^-- DIFF:" "$f"; then
        diff_reason=$(grep "^-- DIFF:" "$f" | head -1 | sed 's/^-- DIFF: //')
    fi

    # Run through all three, capturing output and exit status
    # Use timeout to prevent hangs, strip "Cost used:" line from our output
    our_status=0
    timeout 5s $OUR_LUA "$f" 2>&1 | grep -v "^Cost used:" > "$OUR_OUT" || our_status=$?

    lua52_status=0
    timeout 5s $LUA52 "$f" > "$LUA52_OUT" 2>&1 || lua52_status=$?

    lua54_status=0
    timeout 5s $LUA54 "$f" > "$LUA54_OUT" 2>&1 || lua54_status=$?

    # Compare outputs
    diff_52=$(diff -q "$OUR_OUT" "$LUA52_OUT" 2>/dev/null && echo "same" || echo "diff")
    diff_54=$(diff -q "$OUR_OUT" "$LUA54_OUT" 2>/dev/null && echo "same" || echo "diff")

    if [ "$diff_52" = "same" ] && [ "$diff_54" = "same" ]; then
        echo -e "${GREEN}PASS${NC} $name"
        pass=$((pass + 1))
    elif [ "$diff_54" = "same" ]; then
        # Match lua5.4 but not 5.2 - probably fine (we target newer Lua)
        echo -e "${GREEN}PASS${NC} $name (matches lua5.4, differs from lua5.2)"
        pass=$((pass + 1))
    elif [ "$diff_52" = "same" ]; then
        # Match lua5.2 but not 5.4
        if [ -n "$diff_reason" ]; then
            echo -e "${BLUE}XDIF${NC} $name (expected: $diff_reason)"
            expected_diff=$((expected_diff + 1))
        else
            echo -e "${YELLOW}WARN${NC} $name (matches lua5.2, differs from lua5.4)"
            pass=$((pass + 1))
        fi
    else
        # Different from both
        if [ -n "$diff_reason" ]; then
            echo -e "${BLUE}XDIF${NC} $name (expected: $diff_reason)"
            expected_diff=$((expected_diff + 1))
        else
            echo -e "${RED}FAIL${NC} $name"
            fail=$((fail + 1))

            # Show diffs
            echo "  --- Our output vs lua5.4 ---"
            diff -u "$LUA54_OUT" "$OUR_OUT" | head -20 | sed 's/^/  /'
            echo ""
        fi
    fi
done

echo ""
echo "=== Summary ==="
echo -e "Passed: ${GREEN}$pass${NC}"
echo -e "Expected differences: ${BLUE}$expected_diff${NC}"
echo -e "Failed: ${RED}$fail${NC}"
echo -e "Skipped: ${YELLOW}$skip${NC}"

if [ $fail -gt 0 ]; then
    exit 1
fi
