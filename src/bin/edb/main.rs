//! Command entry point: dispatch, process signals, and redacted output.
mod admin;
mod arguments;
mod data;
mod dispatch;
mod health;
mod runtime;
use arguments::{Arguments, transaction_defaults, usage};
use dispatch::run;
use edb_core::{ErrorCategory, SemanticError};
use std::process::ExitCode;
use std::sync::{
    Arc, OnceLock,
    atomic::{AtomicBool, Ordering},
};

static STOP: OnceLock<Arc<AtomicBool>> = OnceLock::new();

fn cancellation_token() -> &'static Arc<AtomicBool> {
    STOP.get_or_init(|| Arc::new(AtomicBool::new(false)))
}

fn stop_requested() -> bool {
    cancellation_token().load(Ordering::Relaxed)
}
extern "C" fn stop_signal(_: libc::c_int) {
    // A lock-free store is the only work performed in a signal handler.
    if let Some(stop) = STOP.get() {
        stop.store(true, Ordering::Relaxed);
    }
}

fn io_error() -> SemanticError {
    SemanticError::new(ErrorCategory::Unavailable, "cli/io", "process I/O failed")
}

fn install_signals() -> Result<(), SemanticError> {
    // Initialize before a handler can read the shared cancellation token.
    cancellation_token();
    // POSIX handlers only set a lock-free flag. The main thread owns all
    // cleanup, and handlers remain installed for the process lifetime.
    unsafe {
        let mut action: libc::sigaction = std::mem::zeroed();
        action.sa_sigaction = stop_signal as *const () as libc::sighandler_t;
        libc::sigemptyset(&mut action.sa_mask);
        action.sa_flags = libc::SA_RESTART;
        for signal in [libc::SIGINT, libc::SIGTERM] {
            if libc::sigaction(signal, &action, std::ptr::null_mut()) != 0 {
                return Err(io_error());
            }
        }
    }
    Ok(())
}

fn main() -> ExitCode {
    let raw: Vec<String> = std::env::args().skip(1).collect();
    let result = data::dispatch(&raw)
        .or_else(|| admin::dispatch(&raw))
        .unwrap_or_else(|| Arguments::parse(raw).and_then(run));
    match result {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            // Never print arbitrary server errors, anomalies, transaction data,
            // connection strings or paths originating in failed input.
            eprintln!("ERROR category={:?} code={}", error.category, error.code);
            for field in ["line", "column", "byte_offset"] {
                if let Some(value) = error.details.get(field)
                    && !value.is_empty()
                    && value.bytes().all(|byte| byte.is_ascii_digit())
                {
                    eprintln!("{field}={value}");
                }
            }
            if error.code.starts_with("cli/") || error.code.starts_with("config/") {
                eprintln!("{}", error.message);
            }
            if error.category == ErrorCategory::Fault && error.code == "storage/missing-object" {
                eprintln!(
                    "Inspect the database before retrying. If only the derived current index is damaged, rebuild it with edb consolidate --database ID using authorized credentials. Missing canonical or retained-receipt data requires separate recovery."
                );
            }
            eprintln!(
                "See edb --help and docs/01_tutorials/01_application_workflow.md; no rollback is implied by a failed observation."
            );
            ExitCode::from(if error.category == ErrorCategory::Incorrect {
                2
            } else {
                1
            })
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn process_signal_cancels_the_shared_maintenance_token() {
        let token = cancellation_token().clone();
        token.store(false, Ordering::Relaxed);
        let control =
            edb_core::MaintenanceControl::new(std::time::Duration::ZERO, token.clone()).unwrap();
        install_signals().unwrap();
        // Exercise the installed POSIX handler in this dedicated test process.
        unsafe {
            libc::raise(libc::SIGINT);
        }
        assert_eq!(control.check().unwrap_err().code, "maintenance/canceled");
        assert!(stop_requested());
        token.store(false, Ordering::Relaxed);
        control.check().unwrap();
    }
}
