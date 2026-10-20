//! The binary gates call as `{{gatestrainer}}` is built and current.
//!
//! An integration test makes `cargo test` build the binary target before
//! any test runs; the library's unit tests then run their gates' checks
//! through it (`runner::self_for_gates`) rather than through the test
//! harness, which would read `--check …` as a test filter and exit 0.

#[test]
fn the_binary_answers_a_check() {
    let out = std::process::Command::new(env!("CARGO_BIN_EXE_gatestrainer")).args(["count", "--pattern", "x", "--eq", "0", "Cargo.toml"]).output().expect("runs");
    assert!(out.status.code().is_some(), "{out:?}");
}
