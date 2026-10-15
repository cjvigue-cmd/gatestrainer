//! `gatestrainer`: the checks, the blueprints and the demo from a command line.

const USAGE: &str = "\
gatestrainer <primitive> [options]        run one check (exit 0 pass, 1 fail, 2 bad check)
gatestrainer --check <primitive> [options]  the same, as a gate's step names it
gatestrainer blueprint list               the gate blueprints, built-in and yours
gatestrainer blueprint test NAME CASES.json  a blueprint against a case file
gatestrainer demo                         every built-in gate on right and wrong answers
gatestrainer --help | --version
";

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let code = match args.first().map(String::as_str) {
        None | Some("--help") | Some("-h") => {
            print!("{USAGE}\n{}", gatestrainer::check::USAGE);
            0
        }
        Some("--version") | Some("-V") => {
            println!("gatestrainer {}", env!("CARGO_PKG_VERSION"));
            0
        }
        Some("--check") | Some("check") => gatestrainer::check_cli(&args[1..]),
        Some("blueprint") | Some("--blueprint") => runtime().block_on(gatestrainer::blueprint_cli::cli(&args[1..])),
        Some("demo") | Some("--demo") => runtime().block_on(gatestrainer::blueprint_cli::demo()),
        Some(_) => gatestrainer::check_cli(&args),
    };
    std::process::exit(code);
}

fn runtime() -> tokio::runtime::Runtime {
    match tokio::runtime::Runtime::new() {
        Ok(rt) => rt,
        Err(e) => {
            eprintln!("gatestrainer: no async runtime: {e}");
            std::process::exit(2);
        }
    }
}
