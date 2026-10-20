# Gatestrainer

**Deterministic checks for AI output. Only the exit code counts.**

A model's claim that its answer is right counts for nothing. A check that runs and exits decides. Gatestrainer is a single program with three layers:

1. **Checks.** Twenty-three primitive checks, each one deterministic, reading only the files it is given. `gatestrainer numeric --file answer.txt --pattern "answer: (\S+)" --expect 1157.625` exits 0 if the number is there and right, 1 if it isn't, with one problem per line on stderr written to be fed straight back to the model.
2. **Gates.** A gate is files plus a command whose exit code decides, run in a fresh folder with a scrubbed environment, under a watchdog and whatever kernel isolation the OS offers (Landlock and seccomp on Linux, Job Objects on Windows, rlimits on macOS). The command comes from a model, so the gate treats it as hostile.
3. **Blueprints and plants.** A blueprint is a gate described as JSON with named inputs, so a task names it instead of spelling a gate out. After a pass, the gate is also given a **planted false answer**, a wrong answer chosen by the program and never by the model. A gate that passes the plant is weak for that task, whatever it said about the answer.

Built in Rust, one binary, no runtime dependencies. Linux, Windows and macOS.

```sh
cargo install --git https://github.com/cjvigue-cmd/gatestrainer   # crates.io release to follow
gatestrainer demo
```

The demo puts fourteen built-in gates to work on answers written in advance: right ones, an empty one and near misses (a refund counted as income, `\d{2}:\d{2}` letting `24:00` through, a 1.01 kg parcel priced as 1 kg). Every near miss is caught, with the reason a model would get back, in about fifteen seconds (the OpenSCAD part takes about five of them). No model and no network.

## The exit-code contract

| Exit | Meaning | Who's at fault |
|---|---|---|
| 0 | The artifact passes. | |
| 1 | The artifact fails. stderr has the first problem on the first line, then up to 19 more. | The model. |
| 2 | The check itself is wrong: bad arguments, a bad pattern, an unsupported schema keyword, an unknown option. | The gate's author. |

Separating 1 from 2 matters more than it looks. Most tools conflate them, so a typo in a gate reads as "the model failed" and the model is sent to fix an answer that was fine. Every check also refuses options it doesn't know, so a misspelled flag can't silently weaken a gate.

## The checks

Two kinds, and the difference is the point of this project.

**Proves.** A pass means what it says: a script decided it reliably.

| Check | Decides |
|---|---|
| `count` | a pattern occurs exactly, at least or at most N times across files |
| `numeric` | the value a pattern captures is within tolerance of the expected one |
| `schema` | a file is JSON that fits a JSON Schema (a subset; unsupported keywords are refused, never ignored) |
| `csv` | the header is exactly the named columns, the types hold, the row count is in range |
| `bounds` | words, lines, characters, or the length ratio against a source, within limits |
| `scope` | a change touched only the allowed paths, against a baseline copy |
| `differential` | two programs agree on every case from a list (a refactor against its original) |
| `mutation` | the tests pass on the code and fail on at least a share of its mutants |
| `regex` | a pattern matches every line in one file and no line in another, in the named engine (Rust, Python or JavaScript) |
| `quote` | every quotation of four or more words is in the source word for word |
| `date` | every date is real (no 30 February), its weekday right, and the expected one when given |
| `resolve` | relative links and anchors exist, code names exist in the named files, required mentions are present |
| `preserve` | every number, URL, code span, placeholder or heading level in the source appears in the result at least as often |
| `records` | two data files (CSV, TSV, a JSON array of objects, JSON Lines) hold the same records: the same field names, values equal as text, numbers (`3.50` = `3.5`, but `02134` stays `02134`), true/false or both empty, in order unless `--order any` |
| `grounded` | every value in a JSON answer is in the source: text word for word, numbers equal to ones written there (`1,249.50` grounds `1249.5`), ISO dates written there in any form; true/false/null and `--except` fields skipped |
| `ran` | a test command exited 0 *and* its runner reported finishing (unittest's `Ran N tests` and `OK`, no file reported as its own test by `node --test`, a `test result:` for every test binary cargo starts): code that exits its own process mid-test can't pass. The code blueprints and presets run their tests through it |
| `mesh` | an STL (ASCII or binary) is a closed solid, every edge on exactly two faces, with the expected bounding box and, if given, volume |

**Lints.** A pass means the text shows none of the listed symptoms. These catch phrases, not substance, and a rephrased answer slips past. They're labelled so you never mistake one for a proof.

| Check | Looks for |
|---|---|
| `placeholder` | unfinished answers: "rest of the code unchanged", `todo!()`, `pass  # TODO`, lines that are only `...` |
| `claims` | work claimed that didn't happen: "I ran the tests", "verified to work", "production-ready" |
| `secrets` | credentials left in: known key formats and random-looking values assigned to a password, token or key |
| `overclaim` | sweeping words the source never used: all, never, completely, eliminates |
| `flattery` | praise of the asker, eager openers, grovelling |
| `refusal` | deflections: "As an AI…", "I'm unable to…", "I don't have access to…" |

Honest limits, so they're written down: `quote` ignores quotations under four words; `preserve` checks that a summary's numbers exist in the source, not that each is attached to the right claim; `mutation`'s operators are small (flipped comparisons, swapped and/or, booleans, integers off by one), so it catches tests that assert nothing, not tests that miss a branch; `resolve` counts web links but doesn't fetch them; `grounded` proves a value is in the source, not that it sits in the right field, and a small integer is in almost any text; `mesh` measures the envelope, closedness and volume, so a hole in the wrong place passes.

`gatestrainer --help` prints every option.

## Gates

A gate runs a command in a fresh temp folder holding the model's files and any fixed scaffolding, tokenised without a shell, with the environment scrubbed (API keys never reach it). Several `steps` run in order and the first to fail decides. `{{gatestrainer}}` in a step stands for this program, so a gate can be `{{gatestrainer}} --check numeric …` and run inside the same sandbox as everything else.

Protection is layered, and every result says which layer applied:

| Layer | Linux | macOS | Windows | Stops |
|---|---|---|---|---|
| Watchdog: wall-clock timeout, process-tree memory limit, process-count cap, whole-tree kill | ✔ | ✔ | ✔ | hangs, memory bombs, runaway trees |
| rlimits: `NPROC`, `FSIZE`, `CPU`, `CORE` | ✔ | ✔ | | fork bombs at the kernel, disk filling |
| Job Object: per-tree process limit, memory limit, kill-on-close | | | ✔ | fork bombs and memory bombs at the kernel |
| Landlock: reads only in the sandbox, system folders and the toolchain; writes only in the sandbox; TCP denied (kernel ≥ 5.13; network needs ≥ 6.7) | ✔ | | | reading secrets on disk, `rm -rf ~`, writes elsewhere |
| seccomp: no IPv4, IPv6, packet or Unix sockets, no io_uring | ✔ | | | exfiltration over UDP or TCP; reaching dockerd or the session bus through a Unix socket |

The watchdog is the portable contract; the kernel layers are extra. `GATESTRAINER_REQUIRE_FS_ISOLATION=1` refuses to run when Landlock is unavailable instead of degrading.

Before the first real attempt, a gate is given an **empty answer**: one that accepts it can't tell right from wrong, and is refused.

## Blueprints and plants

```sh
gatestrainer blueprint list
gatestrainer blueprint test math-answer blueprints/cases/math-answer.json
```

Twenty-three blueprints are built in: `rust-tests`, `python-tests`, `javascript-tests`, `typescript-tests` (your hidden tests against the model's code; TypeScript runs through Node's own type stripping, 22.18 or later, so types are erased and the tests decide), `python-refactor`, `javascript-refactor` (old and new agree on your cases), `*-tests-for` (the model's tests must kill mutants of your code), `math-answer`, `date-answer`, `sql-query`, `regex`, `json-schema`, `csv-table`, `data-convert` (CSV, TSV, JSON or JSON Lines into another of those with every record kept), `markdown-docs`, `rewrite-text`, `purge-term` (a rename or reference purge: your pattern matches nothing afterwards and nothing else is lost), `summarize`, `extract-json` (fields from your document into your schema, every value found in the document), `cited-answer`, `openscad-part` (an OpenSCAD model rendered to STL, then measured: a closed solid of your size and volume; `{{openscad}}` in a step runs OpenSCAD as a separate program). Each says what it `proves`: `full`, or `partial` when its checks are necessary conditions only. Your own go in the folder `GATESTRAINER_BLUEPRINTS` names.

A blueprint's `plants` list says which false answer to try after a pass: `mutate` (the answer with one small change: a flipped comparison, a constant off by one), `number` (the last number changed), `quote` (a word inserted into the first quotation), `overclaim`, `flattery` or `date`. A caller can also supply its own `must_fail` answers. The result reports what was planted and whether the gate caught it. A missed plant doesn't fail the answer; it marks the pass as unverified, because the answer wasn't shown wrong, the gate was shown weak.

`blueprint test` runs a blueprint against a case file of right and wrong answers, and demands at least one of each: a gate is only as good as what it rejects.

## From Rust

```rust
let gate = gatestrainer::GateSpec::preset_with_tests(gatestrainer::TaskType::PythonCodegen, Some(my_tests))?.unwrap();
let result = gatestrainer::run_gate(&gate, model_code, 1, 1, "my-task", None).await?;
if result.passed { /* the tests ran and passed, in the sandbox */ }
```

`gatestrainer::check_cli(&args)` runs a check in-process with the same exit code.

## Environment

| Variable | Effect |
|---|---|
| `GATESTRAINER_PYTHON_BIN`, `GATESTRAINER_NODE_BIN`, `GATESTRAINER_CARGO_BIN`, `GATESTRAINER_OPENSCAD_BIN` | the program a preset or the `regex` check runs (default: the PATH's) |
| `GATESTRAINER_GATE_READ_PATHS` | extra folders a gate may read on Linux (a toolchain outside the system folders) |
| `GATESTRAINER_REQUIRE_FS_ISOLATION` | `1`: refuse to run without Landlock |
| `GATESTRAINER_OUT` | where validated artifacts are kept |
| `GATESTRAINER_RECORDS` | where the plant record is kept |
| `GATESTRAINER_BLUEPRINTS` | a folder of your own blueprints |

## Where it comes from

Gatestrainer is the gate library of [WildSon Coureur](https://github.com/cjvigue-cmd/Coureur), a local-first engine that hands work to a local model and lets gates like these decide what counts as done. The checks were split out so that anyone using any AI tool can run them, with no GPU, no router and no model.

## Support

Gatestrainer is free, and stays free. If it saved you a bad merge, the Sponsor button at the top of the repo is there.

## License

MIT or Apache-2.0, at your option.
