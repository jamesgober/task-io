//! Every interleaving of the intake wake protocol (I-43) within loom's
//! preemption bound: no submit is left in a ring while the timer sleeps.

use task_io_models::intake::{IntakeVariant, run_intake};

fn model(bound: usize, f: impl Fn() + Sync + Send + 'static) {
    let mut b = loom::model::Builder::new();
    if b.preemption_bound.is_none() {
        b.preemption_bound = Some(bound);
    }
    b.check(f);
}

#[test]
fn earlier_then_later_deadline() {
    model(3, || run_intake(IntakeVariant::Fixed, &[1, 2]));
}

#[test]
fn equal_deadlines() {
    model(3, || run_intake(IntakeVariant::Fixed, &[5, 5]));
}

#[test]
fn three_submitters() {
    model(2, || run_intake(IntakeVariant::Fixed, &[3, 1, 2]));
}

const RUN_NO_RECHECK: &str = "TASK_IO_MODELS_RUN_NO_RECHECK";

/// Runs the variant without the timer's re-check; only does work in the
/// child process started by `no_recheck_loses_a_wakeup` (a failed loom run
/// aborts while unwinding).
#[test]
fn no_recheck_model() {
    if std::env::var_os(RUN_NO_RECHECK).is_some() {
        model(3, || run_intake(IntakeVariant::NoRecheck, &[1, 2]));
    }
}

/// Without the re-check, a submit that skipped the wake (its deadline was
/// not earlier than the target an earlier submit set) is left in its ring
/// while the timer sleeps with no deadline: loom reports the deadlock.
#[test]
fn no_recheck_loses_a_wakeup() {
    let out = std::process::Command::new(std::env::current_exe().unwrap())
        .args(["--exact", "no_recheck_model", "--test-threads=1"])
        .env(RUN_NO_RECHECK, "1")
        .output()
        .unwrap();
    let text = format!(
        "{}{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(
        !out.status.success(),
        "the variant without the re-check passed"
    );
    assert!(text.contains("deadlock"), "unexpected failure:\n{text}");
}
