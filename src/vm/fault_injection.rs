//! On-demand refusal testing. No hook calls back into the VM.
use std::cell::{Cell, RefCell};

use super::State;

/// A structural preflight reached during refusal testing.
#[doc(hidden)]
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum FaultSite {
    /// Entering a native invocation.
    NativeDepth,
    /// Following a metamethod handler.
    Metamethod,
    /// Following a callable table.
    CallChain,
    /// Admitting a Lua call.
    CallDepth,
    /// Reserving one stack slot.
    StackSlot,
    /// Reserving a batch of stack slots.
    StackSpace,
}

/// A recorded structural preflight, including its refusal eligibility.
#[doc(hidden)]
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct FaultEvent {
    /// The preflight that was reached.
    pub site: FaultSite,
    /// The depth, stack length or requested space checked at this site.
    pub argument: usize,
    /// Whether this event advances the refusal countdown.
    pub eligible: bool,
    /// Whether this event refused the operation.
    pub fired: bool,
}

#[derive(Default)]
pub(super) struct FaultInjection {
    remaining: Cell<Option<u32>>,
    recording: Cell<bool>,
    events: RefCell<Vec<FaultEvent>>,
}

impl State {
    #[cfg(feature = "snapshot")]
    pub(super) fn debug_assert_fault_injection_disarmed(&self) {
        debug_assert!(!self.fault_injection.recording.get());
        debug_assert!(self.fault_injection.remaining.get().is_none());
    }

    /// Arm the one-based Nth eligible structural refusal, or record an
    /// uninjected execution with `None`.
    ///
    /// # Panics
    ///
    /// Panics if the target is zero, before changing the current recording.
    #[doc(hidden)]
    pub fn arm_fault_injection(&self, target: Option<u32>) {
        assert!(target != Some(0), "fault targets are one-based");
        self.fault_injection.events.borrow_mut().clear();
        self.fault_injection.remaining.set(target);
        self.fault_injection.recording.set(true);
    }

    /// Stop recording and retrieve the structural checks reached in order.
    #[doc(hidden)]
    pub fn finish_fault_injection(&self) -> Vec<FaultEvent> {
        self.fault_injection.remaining.set(None);
        self.fault_injection.recording.set(false);
        std::mem::take(&mut *self.fault_injection.events.borrow_mut())
    }

    pub(super) fn inject_fault(&self, site: FaultSite, argument: usize, eligible: bool) -> bool {
        let fault = &self.fault_injection;
        if !fault.recording.get() {
            return false;
        }
        let fire = if eligible {
            match fault.remaining.get() {
                Some(1) => {
                    fault.remaining.set(None);
                    true
                }
                Some(n) => {
                    fault.remaining.set(Some(n - 1));
                    false
                }
                None => false,
            }
        } else {
            false
        };
        fault.events.borrow_mut().push(FaultEvent {
            site,
            argument,
            eligible,
            fired: fire,
        });
        fire
    }
}

#[cfg(test)]
mod tests {
    use super::{FaultEvent, FaultSite, State};
    use crate::error::ErrorKind;
    use crate::{ArgCount, RetCount};

    #[test]
    fn countdown_skips_ineligible_checks_and_fires_only_once() {
        fn callback(state: &mut State) -> crate::Result<u8> {
            state.check_stack_space(0)?;
            state.push_nil()?;
            Ok(0)
        }

        let mut state = State::new();
        state.push_rust_fn(callback).unwrap();
        state.arm_fault_injection(Some(2));
        let error = state
            .call(ArgCount::Fixed(0), RetCount::Fixed(0))
            .unwrap_err();
        assert!(matches!(error.kind(), ErrorKind::StackOverflow { .. }));
        assert_eq!(state.get_top(), 0);
        state.push_nil().unwrap();
        assert_eq!(
            state.finish_fault_injection(),
            vec![
                FaultEvent {
                    site: FaultSite::NativeDepth,
                    argument: 0,
                    eligible: false,
                    fired: false,
                },
                FaultEvent {
                    site: FaultSite::StackSpace,
                    argument: 0,
                    eligible: true,
                    fired: false,
                },
                FaultEvent {
                    site: FaultSite::StackSlot,
                    argument: 0,
                    eligible: true,
                    fired: true,
                },
                FaultEvent {
                    site: FaultSite::StackSlot,
                    argument: 0,
                    eligible: true,
                    fired: false,
                },
            ]
        );
        state.push_nil().unwrap();
        assert!(state.finish_fault_injection().is_empty());
    }

    #[test]
    fn zero_target_is_rejected_before_changing_the_recording() {
        let mut state = State::new();
        state.arm_fault_injection(Some(1));
        let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            state.arm_fault_injection(Some(0));
        }));
        assert!(result.is_err());
        assert!(matches!(
            state.push_nil().unwrap_err().kind(),
            ErrorKind::StackOverflow { .. }
        ));
        assert_eq!(
            state.finish_fault_injection(),
            vec![FaultEvent {
                site: FaultSite::StackSlot,
                argument: 0,
                eligible: true,
                fired: true,
            }]
        );
    }
}
