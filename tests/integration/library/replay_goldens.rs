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
