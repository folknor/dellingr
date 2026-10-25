//! Literal pins for replay-visible standard-library behavior.

use crate::support::{self, Recorder};

#[test]
fn math_random_lua_stream_is_pinned() {
    let (mut state, recorder) = Recorder::state();
    state.set_rng_seed(0);
    support::try_run(
        &mut state,
        r#"
        print(string.format("%.0f", math.random() * 9007199254740992))
        print(math.random(8))
        print(math.random(-3, 4))
        print(math.random(1))
        print(math.random(12, 12))
        print(string.format("%.0f", math.random() * 9007199254740992))
        "#,
    )
    .expect("the pinned math.random sequence runs");

    // The no-argument calls expose the high 53 bits of draws one and four:
    // multiplying by 2^53 makes them exact integers, and `%.0f` prints all
    // their digits (plain `print` renders with `%.14g`, which would round).
    // Draws two and three use the low three bits for ranges of width eight.
    // Both singleton calls must leave draw four next in the stream.
    assert_eq!(
        recorder.prints(),
        vec![
            b"7956156453446585".to_vec(),
            b"5".to_vec(),
            b"4".to_vec(),
            b"1".to_vec(),
            b"12".to_vec(),
            b"8744927430068624".to_vec(),
        ]
    );
}

#[test]
fn stock_library_pairs_order_is_pinned() {
    let actual = String::from_utf8(support::eval_bytes(
        r#"
        local function order(t)
            local result = ''
            for key in pairs(t) do result = result .. key .. ',' end
            return result
        end
        return order(_G) .. '|' .. order(math) .. '|' .. order(string) .. '|' .. order(table)
        "#,
    ))
    .expect("the stock library keys are ASCII");

    // Snapshot mode registers named Rust functions but writes the same table
    // keys through the same ordered registration lists.
    assert_eq!(
        actual,
        concat!(
            "ipairs,next,pairs,print,error,type,tonumber,tostring,unpack,",
            "getmetatable,setmetatable,rawget,rawset,rawequal,rawlen,select,",
            "_G,math,string,table,|",
            "sin,cos,atan2,sqrt,abs,min,max,floor,ceil,random,tan,acos,asin,",
            "atan,deg,rad,exp,log,fmod,modf,pi,huge,|",
            "sub,find,format,len,upper,lower,reverse,match,gmatch,gsub,|",
            "insert,remove,sort,unpack,pack,concat,move,"
        )
    );
}

/// A NaN result is pinned only as "some NaN": its sign and payload are not
/// part of the determinism guarantee (reference/deterministic-math.md).
const ANY_NAN: u64 = 0x7FF8_0000_0000_0000;
const INF: u64 = 0x7FF0_0000_0000_0000;
const NEG_INF: u64 = 0xFFF0_0000_0000_0000;
const NEG_ZERO: u64 = 0x8000_0000_0000_0000;
/// Smallest positive subnormal, 2^-1074, and its negation.
const SUB: u64 = 1;
const NEG_SUB: u64 = 0x8000_0000_0000_0001;
const MAX: u64 = 0x7FEF_FFFF_FFFF_FFFF;
const NEG_MAX: u64 = 0xFFEF_FFFF_FFFF_FFFF;
/// Smallest positive normal, 2^-1022.
const MIN_NORMAL: u64 = 0x0010_0000_0000_0000;

fn bits(x: f64) -> u64 {
    x.to_bits()
}

fn assert_bits(label: &str, got: f64, expected: u64) {
    if expected == ANY_NAN {
        assert!(
            got.is_nan(),
            "{label}: expected NaN, got {:#018x}",
            got.to_bits()
        );
    } else {
        assert_eq!(
            got.to_bits(),
            expected,
            "{label}: got {:#018x}, expected {expected:#018x}",
            got.to_bits()
        );
    }
}

/// `%` (floored, Lua 5.4) and `math.fmod` (truncated) on signed zeros,
/// subnormal divisors, huge quotients, infinities and NaN operands, as bits.
///
/// Each expected value is derived by hand from IEEE `fmod`, which is exact, so
/// every correct libm agrees: `fmod` keeps the dividend's sign, and `%` adds
/// the divisor when a nonzero remainder has the opposite sign. The operands
/// reach the script as arguments so the parser cannot fold them; the folded
/// path (`lua_modulo`, shared with the VM) is pinned separately below.
#[test]
fn remainder_edge_cases_are_pinned_bit_exact() {
    // (dividend, divisor, `%` result, `math.fmod` result), all as bits.
    #[rustfmt::skip]
    let cases: &[(u64, u64, u64, u64)] = &[
        // Signs of the operands.
        (bits(5.5), bits(2.0), bits(1.5), bits(1.5)),
        (bits(-5.5), bits(2.0), bits(0.5), bits(-1.5)),
        (bits(5.5), bits(-2.0), bits(-0.5), bits(1.5)),
        (bits(-5.5), bits(-2.0), bits(-1.5), bits(-1.5)),
        // Zero results keep the dividend's sign and are never corrected.
        (0, bits(1.0), 0, 0),
        (NEG_ZERO, bits(1.0), NEG_ZERO, NEG_ZERO),
        (bits(4.0), bits(2.0), 0, 0),
        (bits(-4.0), bits(2.0), NEG_ZERO, NEG_ZERO),
        (bits(4.0), bits(-2.0), 0, 0),
        (bits(-4.0), bits(-2.0), NEG_ZERO, NEG_ZERO),
        (0, NEG_INF, 0, 0),
        // Infinite divisors: the floor correction adds the infinity.
        (bits(5.0), INF, bits(5.0), bits(5.0)),
        (bits(5.0), NEG_INF, NEG_INF, bits(5.0)),
        (bits(-5.0), INF, INF, bits(-5.0)),
        (bits(-5.0), NEG_INF, bits(-5.0), bits(-5.0)),
        // Invalid operations and NaN operands.
        (INF, bits(3.0), ANY_NAN, ANY_NAN),
        (NEG_INF, bits(3.0), ANY_NAN, ANY_NAN),
        (INF, INF, ANY_NAN, ANY_NAN),
        (bits(3.0), 0, ANY_NAN, ANY_NAN),
        (bits(3.0), NEG_ZERO, ANY_NAN, ANY_NAN),
        (0, 0, ANY_NAN, ANY_NAN),
        (ANY_NAN, bits(3.0), ANY_NAN, ANY_NAN),
        (bits(3.0), ANY_NAN, ANY_NAN, ANY_NAN),
        // Subnormal divisors and dividends (exact, so no rounding anywhere).
        (bits(1.0), SUB, 0, 0),
        (3, 2, 1, 1),
        (3, 0x8000_0000_0000_0002, NEG_SUB, 1),
        (bits(1.0), 3, 1, 1),
        (bits(-1.0), 3, 2, NEG_SUB),
        (bits(1.0), 0x8000_0000_0000_0003, 0x8000_0000_0000_0002, 1),
        (bits(1.5), 3, 0, 0),
        (MIN_NORMAL, SUB, 0, 0),
        (SUB, NEG_MAX, NEG_MAX, SUB),
        // Huge quotients: the remainder of (2^53 - 1) * 2^971 by 3 is 2, by 7 is 5.
        (MAX, bits(3.0), bits(2.0), bits(2.0)),
        (MAX, bits(-3.0), bits(-1.0), bits(2.0)),
        (NEG_MAX, bits(3.0), bits(1.0), bits(-2.0)),
        (NEG_MAX, bits(-3.0), bits(-2.0), bits(-2.0)),
        (MAX, bits(7.0), bits(5.0), bits(5.0)),
        (MAX, MIN_NORMAL, 0, 0),
        (NEG_MAX, MIN_NORMAL, NEG_ZERO, NEG_ZERO),
        (MAX, MAX, 0, 0),
        (MAX, NEG_MAX, 0, 0),
        (bits(1.0), MAX, bits(1.0), bits(1.0)),
        (bits(-1.0), MAX, MAX, bits(-1.0)),
        // Inexact divisor: 1.0 = 9 * 0.1 + r, with r four ulps below 0.1.
        (bits(1.0), bits(0.1), 0x3FB9_9999_9999_9996, 0x3FB9_9999_9999_9996),
        (bits(-1.0), bits(0.1), 0x3C90_0000_0000_0000, 0xBFB9_9999_9999_9996),
    ];

    let (mut state, _recorder) = Recorder::state();
    let program =
        dellingr::Program::compile("local a, b = ...\nreturn a % b, math.fmod(a, b)", None)
            .expect("the remainder probe compiles");
    for &(a, b, modulo, fmod) in cases {
        let (x, y) = (f64::from_bits(a), f64::from_bits(b));
        state.load(&program).expect("the remainder probe loads");
        state.push_number(x).expect("dividend pushes");
        state.push_number(y).expect("divisor pushes");
        state
            .call(dellingr::ArgCount::Fixed(2), dellingr::RetCount::Fixed(2))
            .expect("the remainder probe runs");
        let got_modulo = state.to_number(-2).expect("`%` returns a number");
        let got_fmod = state.to_number(-1).expect("math.fmod returns a number");
        state.set_top(0).expect("the stack clears");
        assert_bits(&format!("{a:#018x} % {b:#018x}"), got_modulo, modulo);
        assert_bits(&format!("fmod({a:#018x}, {b:#018x})"), got_fmod, fmod);
    }
}

/// The constant folder shares `lua_modulo` with the VM, so folded `%` must
/// agree bit for bit with the runtime cases above.
#[test]
fn folded_remainder_matches_the_runtime_edge_cases() {
    let (mut state, _recorder) = Recorder::state();
    support::eval(
        &mut state,
        "return -5.5 % 2, 5.5 % -2, -4 % 2.0, 5 % -(1/0), 1 % 0.1, -1 % 0.1",
        6,
    );
    let expected = [
        bits(0.5),
        bits(-0.5),
        NEG_ZERO,
        NEG_INF,
        0x3FB9_9999_9999_9996,
        0x3C90_0000_0000_0000,
    ];
    for (offset, &want) in expected.iter().enumerate() {
        let index = isize::try_from(offset).expect("small index") - 6;
        let got = state.to_number(index).expect("a folded `%` is a number");
        assert_bits(&format!("folded case {offset}"), got, want);
    }
}
