// The library must expose a callable entry point so an external crate
// (polaris-pro) can dispatch the CLI. This test only needs it to compile
// and be referenceable.
#[test]
fn run_is_public_and_callable() {
    // Reference the symbol without invoking it (invoking would run the CLI).
    let _f: fn() -> std::process::ExitCode = polaris_cli::run;
}
