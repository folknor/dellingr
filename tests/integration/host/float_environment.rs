//! dellingr refuses to compute in an unsupported floating-point environment
//! (`docs/determinism.md`). These tests set MXCSR directly, which only
//! x86_64 runs (the `mod` line is gated).
//!
//! MXCSR is per thread. Every test runs its body on a scoped thread of its
//! own, which starts from the known default controls, restores the value it
//! inherited before it ends (a drop guard covers a panic), and spawns
//! nothing while the controls are changed, so no other test in this process
//! ever runs under them. While the controls are changed a test only makes the
//! calls under test and keeps their results; it asserts after restoring.

use dellingr::error::{Error, ErrorKind, FloatControl, FloatEnvironment, RoundingDirection};
use dellingr::{ArgCount, Program, RetCount, State};

use crate::support::{self, PushNewRustFn, Recorder};

/// The default controls: every exception masked, round to nearest, no FTZ
/// or DAZ, status flags clear.
const DEFAULT: u32 = 0x1F80;
const DAZ: u32 = 1 << 6;
const FTZ: u32 = 1 << 15;
const ROUNDING_SHIFT: u32 = 13;
/// The control bits dellingr checks; the low six are sticky status flags.
const CONTROLS: u32 = 0xFFC0;

fn read_mxcsr() -> u32 {
    let mut mxcsr: u32 = 0;
    // SAFETY: stmxcsr stores four bytes to the given address, a live local.
    unsafe {
        // process-global: reads this thread's MXCSR, changing nothing.
        std::arch::asm!("stmxcsr dword ptr [{}]", in(reg) &raw mut mxcsr, options(nostack));
    }
    mxcsr
}

fn write_mxcsr(mxcsr: u32) {
    // SAFETY: ldmxcsr loads four bytes from the given address, a live local.
    // Setting reserved bits faults, and every value written here leaves them
    // clear. Rust assumes the default environment, which is why the code run
    // under another value is kept to the calls under test.
    unsafe {
        // process-global: this thread only; callers run on `isolated` threads.
        std::arch::asm!("ldmxcsr dword ptr [{}]", in(reg) &raw const mxcsr, options(nostack));
    }
}

/// Restores the MXCSR value it holds when dropped.
struct Restore(u32);

impl Drop for Restore {
    fn drop(&mut self) {
        write_mxcsr(self.0);
    }
}

/// Runs `body` on a new thread that starts from the default controls and
/// restores the value it inherited, and propagates its panic.
fn isolated<R: Send>(body: impl FnOnce() -> R + Send) -> R {
    std::thread::scope(|scope| {
        scope
            .spawn(move || {
                let _restore = Restore(read_mxcsr());
                write_mxcsr(DEFAULT);
                body()
            })
            .join()
            .unwrap_or_else(|payload| std::panic::resume_unwind(payload))
    })
}

/// Runs `body` with MXCSR set to `mxcsr`, then restores the default. Call
/// only inside `isolated`.
fn under<R>(mxcsr: u32, body: impl FnOnce() -> R) -> R {
    let restore = Restore(DEFAULT);
    write_mxcsr(mxcsr);
    let result = body();
    drop(restore);
    result
}

fn refused_environment(error: &Error) -> FloatEnvironment {
    match error.kind() {
        ErrorKind::UnsupportedFloatEnvironment(environment) => *environment,
        other => panic!("expected an UnsupportedFloatEnvironment refusal, got {other:?}"),
    }
}

fn assert_refused_with(result: dellingr::Result<()>, mxcsr: u32) -> FloatEnvironment {
    let error = result.expect_err("an unsupported environment must be refused");
    let environment = refused_environment(&error);
    match environment.control() {
        FloatControl::Mxcsr(bits) => assert_eq!(bits & CONTROLS, mxcsr & CONTROLS),
        other => panic!("x86_64 reports MXCSR, got {other:?}"),
    }
    environment
}

#[test]
fn every_unsupported_control_is_refused_and_diagnosed() {
    let mut cases = vec![
        (DEFAULT | FTZ, "flush-to-zero"),
        (DEFAULT | DAZ, "denormals-are-zero"),
        (DEFAULT | (0b01 << ROUNDING_SHIFT), "downward"),
        (DEFAULT | (0b10 << ROUNDING_SHIFT), "upward"),
        (DEFAULT | (0b11 << ROUNDING_SHIFT), "toward zero"),
    ];
    let unmasked: Vec<u32> = (7..13).map(|bit| DEFAULT & !(1 << bit)).collect();
    for &mxcsr in &unmasked {
        cases.push((mxcsr, "unmasked exception"));
    }

    isolated(|| {
        let program = Program::compile("x = 1", None).expect("the default environment compiles");
        for (mxcsr, label) in cases {
            let mut state = State::new();
            state.load(&program).expect("room for the chunk");
            // An unmasked exception traps on the first inexact or invalid
            // operation, so those cases make only the calls that are refused
            // before any work. The others also create a State, which must
            // succeed: creation is not checked.
            let traps = unmasked.contains(&mxcsr);
            let (created, compiled, pushed, called) = under(mxcsr, || {
                let created = (!traps).then(State::new);
                let compiled = Program::compile("return 1.5 + 2", None).map(drop);
                let pushed = state.push_number(1.0);
                let called = state.call(ArgCount::Fixed(0), RetCount::Fixed(0));
                (created, compiled, pushed, called)
            });

            if !traps {
                let mut created = created.expect("created above");
                support::run(&mut created, "y = 2");
            }
            let environments =
                [compiled, pushed, called].map(|result| assert_refused_with(result, mxcsr));
            let environment = environments[0];
            assert_eq!(
                environment.flushes_subnormal_results(),
                mxcsr & FTZ != 0,
                "{label}"
            );
            assert_eq!(
                environment.flushes_subnormal_inputs(),
                mxcsr & DAZ != 0,
                "{label}"
            );
            assert_eq!(environment.traps_enabled(), traps, "{label}");
            let expected_rounding = match (mxcsr >> ROUNDING_SHIFT) & 0b11 {
                0b00 => RoundingDirection::Nearest,
                0b01 => RoundingDirection::Downward,
                0b10 => RoundingDirection::Upward,
                _ => RoundingDirection::TowardZero,
            };
            assert_eq!(environment.rounding(), expected_rounding, "{label}");
            assert!(!environment.alternate_handling());

            // The refused call changed nothing; the State works once the
            // environment is back.
            assert_eq!(state.get_top(), 1, "{label}: the chunk is still waiting");
            state
                .call(ArgCount::Fixed(0), RetCount::Fixed(0))
                .expect("the restored environment runs");
            assert_eq!(support::global_number(&mut state, "x"), 1.0);
        }
    });
}

#[test]
fn status_flags_are_accepted_and_left_alone() {
    isolated(|| {
        let mut state = State::new();
        let (result, after) = under(DEFAULT | 0x3F, || {
            let result = support::try_run(&mut state, "z = 3 / 7");
            (result, read_mxcsr())
        });
        result.expect("sticky status flags are not a control");
        assert_eq!(after & 0x3F, 0x3F, "the check clears no status flag");
        assert_eq!(after & CONTROLS, DEFAULT);
    });
}

/// Turns on FTZ and returns `Ok`, as engine code that leaves the
/// environment changed would.
fn flip(_state: &mut State) -> dellingr::Result<u8> {
    write_mxcsr(DEFAULT | FTZ);
    Ok(0)
}

/// Turns on FTZ and fails.
fn flip_and_fail(_state: &mut State) -> dellingr::Result<u8> {
    write_mxcsr(DEFAULT | FTZ);
    Err(Error::without_location(ErrorKind::HostError(
        "callback failure".into(),
    )))
}

/// Turns on FTZ, records whether a push is refused, restores the default
/// and returns a value.
fn flip_push_and_restore(state: &mut State) -> dellingr::Result<u8> {
    write_mxcsr(DEFAULT | FTZ);
    let refused = state.push_number(1.0);
    write_mxcsr(DEFAULT);
    let refused = matches!(
        refused.as_ref().map_err(Error::kind),
        Err(ErrorKind::UnsupportedFloatEnvironment(_))
    );
    state.set_user_data(refused);
    state.push_number(7.0)?;
    Ok(1)
}

const COUNTER: &str = "
    local count = 0
    function bump() count = count + 1 return count end
    function run()
        local captured = count
        local read = function() return captured end
        bump()
        host()
        bump()
        return read()
    end
";

fn state_with_host(function: dellingr::RustFunc) -> (State, Recorder) {
    let (mut state, recorder) = Recorder::state();
    state
        .push_new_rust_fn(function)
        .expect("room for a function");
    state.set_global("host").expect("global install");
    support::run(&mut state, COUNTER);
    (state, recorder)
}

fn call_global(state: &mut State, name: &str) -> dellingr::Result<()> {
    state.get_global(name)?;
    state.call(ArgCount::Fixed(0), RetCount::Fixed(0))
}

#[test]
fn a_callback_that_returns_ok_in_a_changed_environment_fails_there() {
    isolated(|| {
        let (mut state, recorder) = state_with_host(flip);
        // `flip` changes the environment itself; `under` restores it.
        let result = under(DEFAULT, || call_global(&mut state, "run"));
        assert_refused_with(result, DEFAULT | FTZ);
        assert_eq!(
            recorder.error_count(),
            1,
            "reported once, as the callback's error"
        );

        // Lua did not resume after the callback: the second bump never ran,
        // and the frames holding the captured local were unwound.
        assert_eq!(state.get_top(), 0);
        support::eval(&mut state, "return bump()", 1);
        assert_eq!(state.to_number(-1).expect("a number"), 2.0);
    });
}

#[test]
fn a_callback_error_in_a_changed_environment_is_kept() {
    isolated(|| {
        let (mut state, recorder) = state_with_host(flip_and_fail);
        let result = under(DEFAULT, || call_global(&mut state, "run"));
        let error = result.expect_err("the callback failed");
        assert!(
            matches!(error.kind(), ErrorKind::HostError(_)),
            "the callback's own error is kept: {:?}",
            error.kind()
        );
        assert_eq!(recorder.error_count(), 1);
        support::eval(&mut state, "return bump()", 1);
        assert_eq!(state.to_number(-1).expect("a number"), 2.0);
    });
}

#[test]
fn a_nested_refusal_leaves_the_call_usable_once_restored() {
    isolated(|| {
        let (mut state, recorder) = state_with_host(flip_push_and_restore);
        let result = under(DEFAULT, || call_global(&mut state, "run"));
        result.expect("the callback restored the environment before returning");
        assert_eq!(
            state.user_data::<bool>(),
            Some(&true),
            "the push was refused"
        );
        assert_eq!(
            recorder.error_count(),
            0,
            "an entry refusal is not reported"
        );
        support::eval(&mut state, "return bump()", 1);
        assert_eq!(state.to_number(-1).expect("a number"), 3.0);
    });
}

#[cfg(feature = "snapshot")]
#[test]
fn save_and_load_refuse_and_load_rechecks_after_setup() {
    use dellingr::{LoadError, SaveError};

    isolated(|| {
        let mut state = State::new();
        support::run(&mut state, "saved = 4");
        let save = state.save_state().expect("the default environment saves");

        let (saved, loaded) = under(DEFAULT | DAZ, || {
            let saved = state.save_state().map(drop);
            let loaded = State::load_state(
                &save.bytes,
                Box::new(Recorder::default()),
                support::no_setup,
            )
            .map(drop);
            (saved, loaded)
        });
        assert!(
            matches!(saved, Err(SaveError::UnsupportedFloatEnvironment(e)) if e.flushes_subnormal_inputs())
        );
        assert!(matches!(
            loaded,
            Err(LoadError::UnsupportedFloatEnvironment(_))
        ));

        // A setup closure is host code: one that leaves the environment
        // changed stops the load before anything saved is materialized.
        let dirty = under(DEFAULT, || {
            State::load_state(&save.bytes, Box::new(Recorder::default()), |_| {
                write_mxcsr(DEFAULT | FTZ);
                Ok::<(), std::convert::Infallible>(())
            })
            .map(drop)
        });
        assert!(
            matches!(dirty, Err(LoadError::UnsupportedFloatEnvironment(e)) if e.flushes_subnormal_results())
        );

        let mut reloaded = support::reload(&state);
        assert_eq!(support::global_number(&mut reloaded, "saved"), 4.0);
    });
}
