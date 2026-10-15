//! The ways a gate can fail to run. A gate that ran and said no is not an
//! error: that is a verdict (`runner::GateResult`).

/// Why a gate could not be run, or a check could not be set up.
#[derive(Debug, thiserror::Error, Clone, PartialEq)]
pub enum Error {
    /// The gate's command failed in a way that is the gate's problem, not
    /// the answer's (it could not be tokenised, or ran but could not be read).
    #[error("Gate failed: {0}")]
    GateFailure(String),
    /// The sandbox could not be made (temp dir, a hostile filename, a limit
    /// that could not be set).
    #[error("Sandbox failure: {0}")]
    SandboxError(String),
    /// The command's program is not on the PATH. Nothing checks for
    /// programs ahead of time; the spawn's `NotFound` says so.
    #[error("Required dependency '{0}' not found in PATH. Install it and make sure it is reachable from this terminal.")]
    DependencyMissing(String),
}

pub type Result<T> = std::result::Result<T, Error>;
