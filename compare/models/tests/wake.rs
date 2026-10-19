//! Every interleaving of the wake protocol, within loom's preemption bound
//! (`LOOM_MAX_PREEMPTIONS`, default 3 here, 2 for three workers). Run on
//! Linux: on Windows, loom's deadlock report overflowed the stack.

use task_io_models::{Variant, run_model};

fn model_bounded(bound: usize, f: impl Fn() + Sync + Send + 'static) {
    let mut b = loom::model::Builder::new();
    if b.preemption_bound.is_none() {
        b.preemption_bound = Some(bound);
    }
    b.check(f);
}

fn model(f: impl Fn() + Sync + Send + 'static) {
    model_bounded(3, f);
}

#[test]
fn fixed_two_workers_two_jobs() {
    model(|| run_model(Variant::Fixed, 2, 2));
}

#[test]
fn fixed_two_workers_three_jobs() {
    model(|| run_model(Variant::Fixed, 2, 3));
}

#[test]
fn fixed_three_workers_two_jobs() {
    // Three workers multiply the state space; a bound of 2 keeps it to
    // minutes.
    model_bounded(2, || run_model(Variant::Fixed, 3, 2));
}

const RUN_LEAK: &str = "TASK_IO_MODELS_RUN_LEAK";

/// Runs the model of the protocol without the fix. Only does work in the
/// child process started by `leak_on_recheck_deadlocks`: loom reports the
/// deadlock with a panic, and a second panic while its threads unwind aborts
/// the process, so the check has to look at the process from outside.
#[test]
fn leak_on_recheck_model() {
    if std::env::var_os(RUN_LEAK).is_some() {
        model(|| run_model(Variant::LeakOnRecheck, 2, 3));
    }
}

/// The model must be able to find the bug it exists for: without the fix,
/// some interleaving leaves a job queued with every thread parked.
#[test]
fn leak_on_recheck_deadlocks() {
    let out = std::process::Command::new(std::env::current_exe().unwrap())
        .args(["--exact", "leak_on_recheck_model", "--test-threads=1"])
        .env(RUN_LEAK, "1")
        .output()
        .unwrap();
    let text = format!(
        "{}{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(!out.status.success(), "the variant without the fix passed");
    assert!(text.contains("deadlock"), "no deadlock reported:\n{text}");
}
