use std::{
    path::{Path, PathBuf},
    process::Command,
};

use clap::{Args, ValueEnum};

use super::output::{Output, Verbosity};
use super::CliError;

/// Output format for `soroban-testkit coverage`.
#[derive(Clone, Copy, ValueEnum)]
pub enum Format {
    /// Per-file coverage stats printed to the terminal.
    Text,
    /// An `lcov.info` file, for IDE integrations (e.g. Coverage Gutters).
    Lcov,
    /// A browsable HTML report.
    Html,
}

/// Arguments for `soroban-testkit coverage`.
#[derive(Args)]
#[command(after_help = "EXAMPLES:\n    \
    soroban-testkit coverage --format html --open\n    \
    soroban-testkit coverage --format lcov\n    \
    soroban-testkit coverage --fail-under 80")]
pub struct CoverageArgs {
    /// Output format.
    #[arg(long, value_enum, default_value_t = Format::Text)]
    format: Format,
    /// Fail (non-zero exit) if line coverage is below this percentage.
    #[arg(long, value_name = "PCT")]
    #[arg(value_parser = validate_percentage)]
    fail_under: Option<f64>,
    /// Cargo.toml for the target workspace (works from any current directory).
    #[arg(long, value_name = "PATH")]
    manifest_path: Option<PathBuf>,
    /// Open the HTML report after generating it (implies --format html).
    #[arg(long)]
    open: bool,
}

/// Wraps `cargo llvm-cov test`, which handles Soroban's coverage needs
/// correctly out of the box — soroban_sdk's own test harness runs contract
/// logic natively (not through the WASM VM), so ordinary LLVM
/// source-based coverage instrumentation applies without any
/// Soroban-specific flags. This command's job is knowing *that*, and
/// knowing the right output-format flags, not reimplementing coverage
/// collection.
///
/// Requires `cargo-llvm-cov` to be installed
/// (`cargo install cargo-llvm-cov`).
fn validate_percentage(raw: &str) -> Result<f64, String> {
    let value: f64 = raw
        .parse()
        .map_err(|_| "expected a percentage from 0 to 100".to_string())?;
    if value.is_finite() && (0.0..=100.0).contains(&value) {
        Ok(value)
    } else {
        Err("expected a finite percentage from 0 to 100".to_string())
    }
}

fn output_path(args: &CoverageArgs) -> Option<PathBuf> {
    match args.format {
        Format::Lcov => Some(PathBuf::from("lcov.info")),
        Format::Html => Some(PathBuf::from("target/llvm-cov/html")),
        Format::Text if args.open => Some(PathBuf::from("target/llvm-cov/html")),
        Format::Text => None,
    }
}

pub fn run(args: CoverageArgs, out: Output) -> Result<(), CliError> {
    let workspace = if let Some(path) = &args.manifest_path {
        let manifest = std::fs::canonicalize(path)
            .map_err(|err| CliError(format!("cannot access manifest {}: {err}", path.display())))?;
        if !manifest.is_file()
            || manifest.file_name().and_then(|s| s.to_str()) != Some("Cargo.toml")
        {
            return Err(CliError(
                "--manifest-path must point to an existing Cargo.toml".into(),
            ));
        }
        Some(manifest)
    } else {
        None
    };
    // Existing report files must never be silently replaced. Use a fresh
    // output location or remove an obsolete report explicitly.
    if let Some(path) = output_path(&args) {
        let root = workspace
            .as_ref()
            .and_then(|p| p.parent())
            .unwrap_or(Path::new("."));
        let report = root.join(path);
        if report.exists() {
            return Err(CliError(format!(
                "coverage report already exists at {}; remove it before generating another report",
                report.display()
            )));
        }
    }
    // cargo's subcommand error for an uninstalled llvm-cov is otherwise
    // indistinguishable from a test failure.
    let installed = Command::new("cargo")
        .arg("llvm-cov")
        .arg("--version")
        .output()
        .map_err(|err| {
            CliError(format!(
                "cannot launch cargo ({err}); install cargo and run `cargo install cargo-llvm-cov`"
            ))
        })?;
    if !installed.status.success() {
        return Err(CliError(
            "cargo-llvm-cov is unavailable; install it with `cargo install cargo-llvm-cov`".into(),
        ));
    }

    let cargo_args = cargo_args(&args, out);
    out.detail(format!("running: cargo {}", cargo_args.join(" ")));

    let mut cmd = Command::new("cargo");
    cmd.args(&cargo_args);
    if let Some(manifest) = workspace {
        cmd.arg("--manifest-path").arg(manifest);
    }
    if !out.color {
        cmd.env("CARGO_TERM_COLOR", "never").env("NO_COLOR", "1");
    }

    let status = cmd.status().map_err(|err| {
        CliError(format!(
            "failed to run `cargo llvm-cov` ({err}); is cargo-llvm-cov installed? \
             try `cargo install cargo-llvm-cov`"
        ))
    })?;

    if !status.success() {
        return Err(CliError(format!(
            "coverage run failed (cargo llvm-cov exited with {status})"
        )));
    }

    Ok(())
}

/// The arguments passed to `cargo` for a coverage run. `--fail-under PCT`
/// becomes cargo-llvm-cov's `--fail-under-lines PCT`, so the threshold is
/// enforced by cargo-llvm-cov itself and a shortfall surfaces as this
/// command's non-zero exit.
fn cargo_args(args: &CoverageArgs, out: Output) -> Vec<String> {
    let mut cargo_args = vec!["llvm-cov".to_string(), "test".to_string()];
    let mut push = |arg: &str| cargo_args.push(arg.to_string());

    match args.format {
        Format::Text => {}
        Format::Lcov => {
            push("--lcov");
            push("--output-path");
            push("lcov.info");
        }
        Format::Html => push("--html"),
    }

    if args.open {
        if !matches!(args.format, Format::Html) {
            push("--html");
        }
        push("--open");
    }

    if let Some(pct) = args.fail_under {
        push("--fail-under-lines");
        push(&pct.to_string());
    }

    if !out.color {
        push("--color");
        push("never");
    }
    match out.verbosity {
        Verbosity::Quiet => push("--quiet"),
        Verbosity::Normal => {}
        Verbosity::Verbose => push("--verbose"),
    }

    cargo_args
}

#[cfg(test)]
mod tests {
    use super::*;

    fn args(format: Format, fail_under: Option<f64>, open: bool) -> CoverageArgs {
        CoverageArgs {
            format,
            fail_under,
            open,
            manifest_path: None,
        }
    }

    fn out(verbosity: Verbosity, color: bool) -> Output {
        Output { verbosity, color }
    }

    #[test]
    fn percentage_validation_rejects_nonfinite_and_out_of_bounds() {
        for invalid in ["-1", "101", "NaN", "inf", "-inf", "oops"] {
            assert!(validate_percentage(invalid).is_err(), "{invalid}");
        }
        for valid in ["0", "100", "50.5"] {
            assert!(validate_percentage(valid).is_ok(), "{valid}");
        }
    }

    #[test]
    fn stale_report_detection_selects_file_and_directory() {
        assert_eq!(
            output_path(&args(Format::Lcov, None, false)),
            Some(PathBuf::from("lcov.info"))
        );
        assert_eq!(
            output_path(&args(Format::Html, None, false)),
            Some(PathBuf::from("target/llvm-cov/html"))
        );
        assert_eq!(output_path(&args(Format::Text, None, false)), None);
    }

    #[test]
    fn fail_under_is_forwarded_as_fail_under_lines() {
        let cargo = cargo_args(
            &args(Format::Text, Some(80.0), false),
            out(Verbosity::Normal, true),
        );
        assert_eq!(cargo, ["llvm-cov", "test", "--fail-under-lines", "80"]);

        let cargo = cargo_args(
            &args(Format::Lcov, Some(92.5), false),
            out(Verbosity::Normal, true),
        );
        assert!(
            cargo.ends_with(&["--fail-under-lines".to_string(), "92.5".to_string()]),
            "{cargo:?}"
        );
    }

    #[test]
    fn no_threshold_means_no_fail_under_flag() {
        let cargo = cargo_args(
            &args(Format::Html, None, true),
            out(Verbosity::Normal, true),
        );
        assert_eq!(cargo, ["llvm-cov", "test", "--html", "--open"]);
    }

    #[test]
    fn color_and_verbosity_are_forwarded_to_cargo_llvm_cov() {
        let quiet = cargo_args(
            &args(Format::Text, None, false),
            out(Verbosity::Quiet, false),
        );
        assert_eq!(quiet, ["llvm-cov", "test", "--color", "never", "--quiet"]);

        let verbose = cargo_args(
            &args(Format::Text, None, false),
            out(Verbosity::Verbose, true),
        );
        assert_eq!(verbose, ["llvm-cov", "test", "--verbose"]);
    }
}
