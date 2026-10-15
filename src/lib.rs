//! Gatestrainer: deterministic checks for AI output.
//!
//! A model's claim that its answer is right counts for nothing; a check
//! that runs and exits decides. Three layers:
//!
//! - [`check`]: the primitive checks (`gatestrainer <primitive> …`). Each
//!   reads only the files it is given, does one fixed thing, and exits 0
//!   when the artifact passes, 1 when it fails (one problem per line on
//!   stderr, written to be fed back to the model) and 2 when the check
//!   itself is misconfigured, so a gate author's mistake is never mistaken
//!   for the model's.
//! - [`runner`] and [`sandbox`]: a gate is files plus a command whose exit
//!   code decides, run in a fresh folder with a scrubbed environment under
//!   a watchdog and whatever kernel isolation the OS offers.
//! - [`blueprint`] and [`plant`]: a gate described as JSON with named
//!   inputs, and the false answer a gate is also given after a pass, to
//!   prove it would have said no.

pub mod blueprint;
pub mod blueprint_cli;
pub mod check;
pub mod config;
pub mod error;
pub mod events;
pub mod plant;
pub mod runner;
pub mod sandbox;
pub mod spec;
pub mod util;
pub mod verdict;

pub use check::cli as check_cli;
pub use error::{Error, Result};
pub use runner::{run_gate, GateRequest, GateResult};
pub use spec::{GateKind, GateSpec, TaskType};
