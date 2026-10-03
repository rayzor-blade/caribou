//! A call of an async Zyntax function, run as a core task. The call
//! returns a `caribou.Future` at once; the function's state machine runs
//! on a task of the caller's world, stepped by Zyntax's `HostTask`. A
//! step that waits on a timer parks the task until the timer is due, so
//! the world runs other tasks meanwhile. The future settles with the
//! function's result, or with the error it raised.
//!
//! The task is a fiber, not a stackless step: the Zyntax code a step runs
//! may call any function of the world, and that function may park.

use caribou::error::Error;
use caribou::{bridge, future, heap, sched};
use caribou_abi::hl::hl_type_kind;
use caribou_abi::{ErrorKind, LangId, Value};
use zyntax_embed::{HostTask, HostTaskStep, ZyntaxPromise};

use crate::object::{self, Class, Origin};

/// What a task needs to make its result a value of the core.
pub struct Result {
    pub kind: hl_type_kind,
    pub class: Option<&'static Class>,
    pub origin: &'static Origin,
    /// Whether the function can leave an error pending.
    pub may_raise: bool,
}

/// Run the promise an async function's entry returned on a task of this
/// world, as the code of `lang`, and return the future it settles.
///
/// # Safety
/// `promise` is what an async entry of `lang`'s runtime returned on this
/// thread, not yet adopted.
pub unsafe fn start(promise: *mut u8, lang: LangId, result: Result) -> Value {
    let promise = unsafe { ZyntaxPromise::adopt(promise) };
    let settled = future::new();
    let value = Value::object(settled.cast());
    let kept = heap::keep(settled.cast());
    sched::spawn_fiber(sched::DEFAULT_STACK_SIZE, move || {
        let _kept = kept;
        let mut task = HostTask::new(promise);
        loop {
            let step = crate::foreign::as_caller(lang, || task.step());
            if let Some(error) = raised(&result) {
                unsafe { future::settle(settled, error, true) };
                return;
            }
            match step {
                HostTaskStep::Ready(word) => {
                    let outcome = unsafe {
                        object::value_out(
                            word as u64,
                            result.kind,
                            result.class,
                            Some(result.origin),
                        )
                    };
                    match outcome {
                        Ok(v) => unsafe { future::settle(settled, v, false) },
                        Err(m) => unsafe {
                            future::settle(
                                settled,
                                Error::value(Error::new(ErrorKind::Type, &m, lang)),
                                true,
                            )
                        },
                    };
                    return;
                }
                HostTaskStep::Timer(deadline) => sched::sleep_until(deadline),
                // Waits to be woken: a bridge that parks a future wakes
                // the task when it resolves it.
                HostTaskStep::Parked => {
                    let _ = sched::park(sched::new_waiter(), None);
                }
                HostTaskStep::Yield => sched::yield_now(),
            }
        }
    });
    value
}

/// The error the step left: a host call's the program could not unwind
/// from, one Zyntax raised, or one a call out of it left pending in the
/// core.
fn raised(result: &Result) -> Option<Value> {
    crate::foreign::host_error()
        .or_else(|| {
            result
                .may_raise
                .then(|| result.origin.take_error())
                .flatten()
        })
        .or_else(bridge::take_pending)
}
