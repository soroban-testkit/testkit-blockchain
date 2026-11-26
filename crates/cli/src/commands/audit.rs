use std::collections::{HashMap, HashSet};
use std::io::Read;
use std::path::{Path, PathBuf};

use clap::{Args, ValueEnum};
use proc_macro2::Span;
use syn::visit::{self, Visit};
use syn::{
    Attribute, BinOp, Expr, FnArg, ImplItemFn, Item, ItemFn, ItemImpl, Local, Pat, Signature, Type,
    Visibility,
};
use walkdir::{self, WalkDir};

use super::output::Output;
use super::CliError;

/// Arguments for `soroban-testkit audit`.
#[derive(Args)]
#[command(after_help = "EXAMPLES:\n    \
    soroban-testkit audit ./src\n    \
    soroban-testkit audit ./src --strict\n    \
    soroban-testkit audit ./src --rule missing-require-auth --rule missing-ttl-bump\n    \
    soroban-testkit audit ./src --exclude-rule unchecked-i128-arithmetic\n    \
    soroban-testkit audit . --exclude target --exclude '**/generated/**'\n    \
    soroban-testkit audit . --include 'contracts/**' --exclude '*_test.rs'\n    \
    soroban-testkit audit ./src --json | jq '.findings'")]
pub struct AuditArgs {
    /// Directory or .rs file to audit; directories are scanned recursively
    /// for .rs files. Pass `-` to read a single source file from stdin
    /// (for editor integrations that audit unsaved buffers)
    #[arg(value_name = "PATH", default_value = ".", verbatim_doc_comment)]
    path: PathBuf,
    /// Exit 1 if any findings are reported, or 2 if a file failed to parse.
    /// Without it, findings are printed and the command still exits 0
    #[arg(long, verbatim_doc_comment)]
    strict: bool,
    /// Print findings as a JSON document (`schema_version` 1) on stdout
    /// instead of text
    #[arg(long, verbatim_doc_comment)]
    json: bool,
    /// Run only this rule (repeatable, or comma-separated). Every rule
    /// named is run; rules not named are skipped. Without --rule, all
    /// rules run
    #[arg(
        long = "rule",
        value_name = "RULE",
        value_enum,
        value_delimiter = ',',
        verbatim_doc_comment
    )]
    rules: Vec<Rule>,
    /// Skip this rule (repeatable, or comma-separated). Applied after
    /// --rule; naming a rule in both is an error
    #[arg(
        long = "exclude-rule",
        value_name = "RULE",
        value_enum,
        value_delimiter = ',',
        verbatim_doc_comment
    )]
    exclude_rules: Vec<Rule>,
    /// Audit only .rs files matching this glob (repeatable, or
    /// comma-separated). Globs match paths relative to PATH with `/`
    /// separators: `*` and `?` stay within one path segment, `**` spans
    /// any number of segments, and a glob without `/` matches a file or
    /// directory name at any depth. A file also matches when one of its
    /// parent directories does. Without --include, every .rs file is
    /// audited
    #[arg(
        long = "include",
        value_name = "GLOB",
        value_delimiter = ',',
        verbatim_doc_comment
    )]
    include: Vec<String>,
    /// Skip .rs files and directories matching this glob (repeatable, or
    /// comma-separated), e.g. generated or vendored code. Same syntax as
    /// --include and applied after it; skipped directories are not read
    #[arg(
        long = "exclude",
        value_name = "GLOB",
        value_delimiter = ',',
        verbatim_doc_comment
    )]
    exclude: Vec<String>,
}

/// The audit rules, by the ID each finding reports.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, ValueEnum)]
pub enum Rule {
    /// An entry point's Address parameter that is never checked with require_auth
    MissingRequireAuth,
    /// Raw `+ - * /` on an i128 value
    UncheckedI128Arithmetic,
    /// A persistent/temporary storage read with no extend_ttl
    MissingTtlBump,
}

impl Rule {
    const ALL: [Rule; 3] = [
        Rule::MissingRequireAuth,
        Rule::UncheckedI128Arithmetic,
        Rule::MissingTtlBump,
    ];

    /// The rule ID used in findings and accepted by --rule/--exclude-rule.
    pub fn id(self) -> &'static str {
        match self {
            Rule::MissingRequireAuth => "missing-require-auth",
            Rule::UncheckedI128Arithmetic => "unchecked-i128-arithmetic",
            Rule::MissingTtlBump => "missing-ttl-bump",
        }
    }
}

/// Resolves `--rule` and `--exclude-rule` into the rules to run, in the
/// fixed order of [`Rule::ALL`]. Selecting rules never hides findings of
/// another selected rule: every rule in the result is fully evaluated, and
/// the filter only decides which rules run, not which findings are kept.
///
/// Contradictory flags are usage errors, so the message is returned for
/// [`usage_error`] to report like any other clap argument error (exit 2).
fn selected_rules(include: &[Rule], exclude: &[Rule]) -> Result<Vec<Rule>, String> {
    if let Some(rule) = include.iter().find(|rule| exclude.contains(rule)) {
        return Err(format!(
            "rule `{}` is both selected with --rule and skipped with --exclude-rule; \
             pass it to only one of them",
            rule.id()
        ));
    }
    let rules: Vec<Rule> = Rule::ALL
        .into_iter()
        .filter(|rule| include.is_empty() || include.contains(rule))
        .filter(|rule| !exclude.contains(rule))
        .collect();
    if rules.is_empty() {
        return Err(
            "--exclude-rule skips every rule, so there is nothing to audit; \
                    leave at least one rule enabled"
                .to_string(),
        );
    }
    Ok(rules)
}

/// `--include` / `--exclude` path globs (#142), matched against the same
/// root-relative, `/`-separated paths findings are reported under.
///
/// Syntax, kept deliberately small and dependency-free:
/// - `*` matches any run of characters within one path segment, `?` exactly
///   one character, and `**` as a whole segment any number of segments
///   (including none);
/// - a glob without `/` matches a file or directory name at any depth, like
///   a `.gitignore` entry (`generated`, `*_pb.rs`);
/// - a glob with `/` is anchored at the audited root (`src/gen/**`); a
///   leading `./` or `/` and a trailing `/` are ignored, and `\` is read as
///   `/` so Windows-style globs work;
/// - a path matches when the glob matches it or any of its parent
///   directories, so `--exclude vendor` skips everything under `vendor/`.
///
/// A file is audited when it matches some `--include` glob (or none were
/// given) and no `--exclude` glob.
#[derive(Debug, Default)]
struct PathFilter {
    include: Vec<Vec<String>>,
    exclude: Vec<Vec<String>>,
}

impl PathFilter {
    fn new(include: &[String], exclude: &[String]) -> Result<Self, String> {
        let parse = |flag: &str, globs: &[String]| -> Result<Vec<Vec<String>>, String> {
            globs
                .iter()
                .map(|glob| {
                    let normalized = glob.replace('\\', "/");
                    let trimmed = normalized
                        .trim_start_matches("./")
                        .trim_start_matches('/')
                        .trim_end_matches('/');
                    if trimmed.is_empty() {
                        return Err(format!(
                            "{flag} {} is empty; pass a glob such as `target` or `src/generated/**`",
                            super::quote(glob)
                        ));
                    }
                    let mut segments: Vec<String> = trimmed
                        .split('/')
                        .filter(|segment| !segment.is_empty() && *segment != ".")
                        .map(str::to_string)
                        .collect();
                    // No `/`: match the name at any depth.
                    if segments.len() == 1 && segments[0] != "**" {
                        segments.insert(0, "**".to_string());
                    }
                    Ok(segments)
                })
                .collect()
        };
        Ok(PathFilter {
            include: parse("--include", include)?,
            exclude: parse("--exclude", exclude)?,
        })
    }

    fn is_active(&self) -> bool {
        !self.include.is_empty() || !self.exclude.is_empty()
    }

    /// Whether `path` (or one of its parent directories) matches an
    /// `--exclude` glob.
    fn excludes(&self, path: &str) -> bool {
        self.exclude
            .iter()
            .any(|glob| matches_path_or_parent(glob, path))
    }

    /// Whether the file at `path` should be audited.
    fn selects(&self, path: &str) -> bool {
        (self.include.is_empty()
            || self
                .include
                .iter()
                .any(|glob| matches_path_or_parent(glob, path)))
            && !self.excludes(path)
    }
}

fn matches_path_or_parent(glob: &[String], path: &str) -> bool {
    let segments: Vec<&str> = path.split('/').filter(|s| !s.is_empty()).collect();
    (1..=segments.len()).any(|len| match_segments(glob, &segments[..len]))
}

fn match_segments(glob: &[String], path: &[&str]) -> bool {
    match glob.split_first() {
        None => path.is_empty(),
        Some((first, rest)) if first == "**" => {
            (0..=path.len()).any(|skip| match_segments(rest, &path[skip..]))
        }
        Some((first, rest)) => path.split_first().is_some_and(|(head, tail)| {
            match_segment(first.as_bytes(), head.as_bytes()) && match_segments(rest, tail)
        }),
    }
}

/// `*` and `?` within a single segment. Bytes are compared, so `?` matches
/// one byte; globs are expected to be ASCII.
fn match_segment(glob: &[u8], name: &[u8]) -> bool {
    match glob.split_first() {
        None => name.is_empty(),
        Some((b'*', rest)) => (0..=name.len()).any(|skip| match_segment(rest, &name[skip..])),
        Some((b'?', rest)) => !name.is_empty() && match_segment(rest, &name[1..]),
        Some((c, rest)) => name.first() == Some(c) && match_segment(rest, &name[1..]),
    }
}

/// Exits with clap's usage-error format and status 2, as for an unknown
/// flag, honouring --no-color / NO_COLOR like the rest of the CLI.
fn usage_error(message: String) -> ! {
    use clap::CommandFactory;
    let mut command = super::Cli::command();
    if super::output::no_color_requested(std::env::args_os()) {
        command = command.color(clap::ColorChoice::Never);
    }
    command.build();
    command
        .find_subcommand_mut("audit")
        .expect("audit is a subcommand")
        .error(clap::error::ErrorKind::ArgumentConflict, message)
        .exit()
}

/// Version of the `audit --json` document shape. Bump it on any breaking
/// change: a removed or renamed field, or a changed type or meaning.
/// Adding a new field is not breaking and keeps the same version.
const AUDIT_JSON_SCHEMA_VERSION: u32 = 1;

/// The bounded claim every non-quiet `audit` run ends with, including a
/// clean one: an empty result is not a security guarantee.
const DISCLAIMER: &str = "soroban-testkit audit is a linter with three heuristics, not a security \
                          product — a missing finding is not a security guarantee.";

/// A `.rs` file that was not audited because it is not valid Rust syntax
/// (#144). Reported with the parser's position and message instead of being
/// dropped silently, so a clean result never hides an unaudited file.
#[derive(Debug, PartialEq)]
struct Skipped {
    /// Same form as [`Finding::file`].
    file: String,
    /// 1-based line of the parse error.
    line: usize,
    /// 1-based column of the parse error.
    column: usize,
    message: String,
}

/// Exit status for `audit` when its input could not be audited: a PATH that is
/// missing or unreadable, or, with `--strict`, a file that fails to parse. See
/// `EXIT STATUS` in `--help`.
pub const INVALID_INPUT_EXIT_CODE: i32 = 2;

/// How an `audit` run that read its input ended. Invalid input is an error,
/// not a status (see [`run`]).
#[derive(Debug, PartialEq, Eq)]
pub enum AuditStatus {
    /// The scan completed. Findings, if any, do not fail the run unless
    /// `--strict` is passed.
    Pass,
    /// `--strict` was passed and this many findings were reported.
    StrictFindings(usize),
}

#[derive(Debug)]
struct Finding {
    /// See [`finding_path`].
    file: String,
    line: usize,
    rule: &'static str,
    severity: &'static str,
    message: String,
}

/// Static checks over a contract crate. **Not a security product** — a
/// linter with three heuristics, each independently useful and each
/// capable of missing real bugs or flagging non-bugs. The absence of a
/// finding here is never a security guarantee.
///
 fix/audit-136-138-139
/// 1. Contract entry points taking an `Address` parameter that never call
///    `require_auth` anywhere in the function body. An entry point is a
///    `pub fn` in an `impl` block marked `#[contractimpl]`, or any fn of a
///    `#[contractimpl]` trait impl; free functions and helper `impl`s are
///    not entry points (see [`EntryPoints`]).
/// 2. Arithmetic (`+ - * / %`, unary `-`, compound assignment, and raw
///    arithmetic method calls) on an `i128`-typed binding, done outside a
///    `checked_`/`saturating_`/`wrapping_` call. The finding points at the
///    operator's line, so multiline expressions identify the exact
///    arithmetic source location.

/// 1. Contract entry points taking an `Address` parameter (including references,
///    fully qualified paths like `soroban_sdk::Address`, and type or import
///    aliases) that never call `require_auth` anywhere in the function body or
///    in delegated helper functions. An entry point is a `pub fn` in an `impl`
///    block marked `#[contractimpl]`, or any fn of a `#[contractimpl]` trait
///    impl; free functions and helper `impl`s are not entry points (see
///    [`EntryPoints`]). Intentional open entry points can suppress this rule
///    with `#[allow(soroban_testkit::missing_require_auth)]` (or
///    `#[allow(missing_require_auth)]`).
/// 2. Arithmetic (`+ - * /`) on an `i128`-typed binding (following references
///    and simple aliases), done outside a `checked_`/`saturating_`/`wrapping_`
///    call. The finding points at the operator's line, so multiline expressions
///    identify the exact arithmetic source location.
 main
/// 3. A `.persistent()`/`.temporary()` storage read with no
///    `.extend_ttl(` call anywhere in the same function.
///
/// Finding paths are relative to the audited `PATH` and `/`-separated (see
/// [`finding_path`]), so the same crate reports the same paths whichever
/// directory the command runs from. `--include`/`--exclude` globs match
/// those same paths (see [`PathFilter`]). Symbolic links below `PATH` are
/// not followed (see [`collect_sources`]). Files that do not parse are
/// listed as skipped, with the parser's position and message.
///
/// A finding's line is the line of its function's `fn` keyword, or of the
/// operator for arithmetic, so a doc comment or attribute above a function
/// never moves the reported line.
pub fn run(args: AuditArgs, out: Output) -> Result<AuditStatus, CliError> {
    let rules =
        selected_rules(&args.rules, &args.exclude_rules).unwrap_or_else(|msg| usage_error(msg));
    let all_rules = rules.len() == Rule::ALL.len();
    let filter =
        PathFilter::new(&args.include, &args.exclude).unwrap_or_else(|msg| usage_error(msg));
    let mut findings = Vec::new();
    let mut skipped = Vec::new();

    if args.path == Path::new("-") {
        if filter.is_active() {
            usage_error(
                "--include and --exclude filter the files found under a directory PATH; \
                 they cannot be used when reading from stdin (`-`)"
                    .to_string(),
            );
        }
        let mut src = String::new();
        std::io::stdin()
            .read_to_string(&mut src)
            .map_err(|err| CliError(format!("failed to read stdin: {err}")))?;
        audit_source(&src, "<stdin>", &rules, &mut findings, &mut skipped);
    } else {
        // WalkDir yields a missing root as an Err entry, which the
        // filter_map below would drop, so a typo'd PATH would otherwise
        // "pass" with no findings.
        std::fs::metadata(&args.path).map_err(|err| {
            CliError(format!(
                "cannot audit {}: {err}; pass a contract crate's source directory, \
                 a .rs file, or `-` for stdin",
                super::quote(args.path.display())
            ))
        })?;
        let collection = collect_sources(&args.path, &filter)?;
        for (path, reason) in &collection.skipped {
            out.detail(format!("skipping {} ({reason})", path.display()));
        }
        for (path, display) in &collection.files {
            out.detail(format!("scanning {}", path.display()));
            audit_file(path, display, &rules, &mut findings, &mut skipped)?;
        }
        // A pattern typo would otherwise "pass" with no findings, like a
        // typo'd PATH, so filtering out every .rs file is an error.
        if collection.files.is_empty() && collection.filtered_out > 0 {
            return Err(CliError(format!(
                "no .rs file under {} matches --include/--exclude ({} filtered out); \
                 check the glob patterns",
                super::quote(args.path.display()),
                collection.filtered_out
            )));
        }
    }

    // stdout stays a single JSON document with --json, which lists skipped
    // files in its `skipped` field instead.
    if !args.json {
        for skip in &skipped {
            eprintln!(
                "warning: skipped {}:{}:{}: not valid Rust, so it was not audited: {}",
                skip.file, skip.line, skip.column, skip.message
            );
        }
    }
    // A file that does not parse is not audited. Without --strict it is
    // reported above and the run goes on. With --strict the run is invalid
    // input, because a clean result would hide a file nobody checked.
    if args.strict && !skipped.is_empty() {
        let files: Vec<&str> = skipped.iter().map(|skip| skip.file.as_str()).collect();
        return Err(CliError(format!(
            "{} file(s) failed to parse and were not audited (--strict): {}",
            skipped.len(),
            files.join(", ")
        )));
    }
    let skipped_note = match skipped.len() {
        0 => String::new(),
        1 => " 1 file could not be parsed and was not audited (see the warning above).".to_string(),
        n => {
            format!(" {n} files could not be parsed and were not audited (see the warnings above).")
        }
    };

    // Text mode only: stdout must stay a single JSON document with --json,
    // which reports the rules that ran in its `rules` field instead.
    if !all_rules && !args.json {
        let ids: Vec<&str> = rules.iter().map(|rule| rule.id()).collect();
        out.note(format!("audit: running only {}", ids.join(", ")));
    }

    if args.json {
        println!("{}", render_json(&findings, &skipped, &rules));
    } else if findings.is_empty() {
        out.note(format!("audit: no findings.{skipped_note} {DISCLAIMER}"));
    } else {
        for finding in &findings {
            println!(
                "{}:{}: [{}] {}: {}",
                finding.file, finding.line, finding.severity, finding.rule, finding.message
            );
        }
        out.note(format!(
            "\n{} finding(s).{skipped_note} {DISCLAIMER}",
            findings.len()
        ));
    }

    if args.strict && !findings.is_empty() {
        return Ok(AuditStatus::StrictFindings(findings.len()));
    }
    Ok(AuditStatus::Pass)
}

/// What a directory walk decided: the `.rs` files to audit, and the paths it
/// passed over with the reason, reported at detail verbosity.
#[derive(Debug, Default)]
struct Collection {
    /// Files to read, with the path their findings are reported under.
    files: Vec<(PathBuf, String)>,
    /// Paths that were not read, and why.
    skipped: Vec<(PathBuf, &'static str)>,
    /// `.rs` files left out by `--include` or `--exclude`.
    filtered_out: usize,
}

/// Walks `root` for the `.rs` files a directory audit reads (#151).
///
/// Symbolic links below `root` are never followed. A linked file or directory
/// inside the tree is listed in [`Collection::skipped`] and not read, so a
/// link cannot loop back into the tree or audit the same source under two
/// paths. `root` itself is followed when it is a link: naming a path is a
/// request to audit what it points at, and skipping it would turn a linked
/// crate into a silent clean audit of zero files.
///
/// Excluded directories are pruned before they are read, and a traversal
/// error stops the walk instead of letting it report a partial audit.
fn collect_sources(root: &Path, filter: &PathFilter) -> Result<Collection, CliError> {
    let mut collection = Collection::default();
    let mut pruned = Vec::new();
    let walker = WalkDir::new(root)
        .follow_root_links(true)
        .into_iter()
        .filter_entry(|entry| {
            // The root itself is always walked, whatever its type.
            if entry.depth() == 0 || !entry.file_type().is_dir() {
                return true;
            }
            let keep = !filter.excludes(&finding_path(root, entry.path()));
            if !keep {
                pruned.push((entry.path().to_path_buf(), "--exclude"));
            }
            keep
        });
    for entry in walker {
        // A traversal error means at least one .rs file may never be
        // examined, so stop instead of reporting a misleading clean audit.
        let entry = entry.map_err(|err| traversal_error(root, &err))?;
        // `path_is_symlink` is also true for a followed root, which is why
        // the root is exempt from this check.
        if entry.depth() > 0 && entry.path_is_symlink() {
            collection
                .skipped
                .push((entry.path().to_path_buf(), "symlink, not followed"));
            continue;
        }
        if !entry.file_type().is_file() {
            continue;
        }
        if entry.path().extension().is_none_or(|ext| ext != "rs") {
            continue;
        }
        let display = finding_path(root, entry.path());
        if !filter.selects(&display) {
            collection.filtered_out += 1;
            collection
                .skipped
                .push((entry.path().to_path_buf(), "--include/--exclude"));
            continue;
        }
        collection.files.push((entry.path().to_path_buf(), display));
    }
    collection.skipped.append(&mut pruned);
    Ok(collection)
}

/// Renders findings as the `audit --json` document, schema version
/// [`AUDIT_JSON_SCHEMA_VERSION`]:
///
/// ```json
/// {"schema_version":1,"disclaimer":"...","rules":["missing-require-auth","unchecked-i128-arithmetic","missing-ttl-bump"],"skipped":[{"file":"src/gen.rs","line":3,"column":5,"message":"..."}],"finding_count":1,"findings":[{"file":"src/lib.rs","line":7,"severity":"warning","rule":"missing-require-auth","message":"..."}]}
/// ```
///
/// `rules` lists the rules that ran (see `--rule`/`--exclude-rule`) and
/// `skipped` the files that could not be parsed (#144); both were added
/// without a version bump because new fields are not breaking.
fn render_json(findings: &[Finding], skipped: &[Skipped], rules: &[Rule]) -> String {
    let items: Vec<String> = findings
        .iter()
        .map(|finding| {
            format!(
                "{{\"file\":{},\"line\":{},\"severity\":{},\"rule\":{},\"message\":{}}}",
                json_string(&finding.file),
                finding.line,
                json_string(finding.severity),
                json_string(finding.rule),
                json_string(&finding.message),
            )
        })
        .collect();
    let rule_ids: Vec<String> = rules.iter().map(|rule| json_string(rule.id())).collect();
    let skipped_items: Vec<String> = skipped
        .iter()
        .map(|skip| {
            format!(
                "{{\"file\":{},\"line\":{},\"column\":{},\"message\":{}}}",
                json_string(&skip.file),
                skip.line,
                skip.column,
                json_string(&skip.message),
            )
        })
        .collect();
    format!(
        "{{\"schema_version\":{AUDIT_JSON_SCHEMA_VERSION},\"disclaimer\":{},\"rules\":[{}],\"skipped\":[{}],\"finding_count\":{},\"findings\":[{}]}}",
        json_string(DISCLAIMER),
        rule_ids.join(","),
        skipped_items.join(","),
        findings.len(),
        items.join(",")
    )
}

/// Quotes and escapes `value` as a JSON string. Hand-rolled so the audit
/// command takes no serialization dependency for one small document.
fn json_string(value: &str) -> String {
    let mut out = String::with_capacity(value.len() + 2);
    out.push('"');
    for ch in value.chars() {
        match ch {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            c if (c as u32) < 0x20 => out.push_str(&format!("\\u{:04x}", c as u32)),
            c => out.push(c),
        }
    }
    out.push('"');
    out
}

/// The path a finding in `file` is reported under: relative to the audited
/// `root` with `/` separators, e.g. `src/lib.rs` for `root = "."` or
/// `lib.rs` for `root = "examples/vault/src"`. When `root` is the file
/// itself, its file name is used. Falls back to `file` as given if it is
/// not under `root`.
fn finding_path(root: &Path, file: &Path) -> String {
    let relative = match file.strip_prefix(root) {
        Ok(rel) if rel.as_os_str().is_empty() => file.file_name().map(Path::new).unwrap_or(file),
        Ok(rel) => rel,
        Err(_) => file,
    };
    relative
        .components()
        .map(|c| c.as_os_str().to_string_lossy())
        .collect::<Vec<_>>()
        .join("/")
}

fn audit_file(
    path: &Path,
    display: &str,
    rules: &[Rule],
    findings: &mut Vec<Finding>,
    skipped: &mut Vec<Skipped>,
) -> Result<(), CliError> {
    let src = std::fs::read_to_string(path).map_err(|err| {
        CliError(format!(
            "failed to read {}: {err}",
            super::quote(path.display())
        ))
    })?;
    audit_source(&src, display, rules, findings, skipped);
    Ok(())
}

/// Turns a `WalkDir` failure into the shared CLI error shape. The audited
/// root is named so the message stays actionable when `walkdir::Error` has
/// no path of its own (for example, some loop and metadata failures).
fn traversal_error(root: &Path, err: &walkdir::Error) -> CliError {
    let path = err.path().unwrap_or(root);
    let cause = match err.io_error() {
        Some(io) => io.to_string(),
        None => err.to_string(),
    };
    format_traversal_error(path, &cause)
}

fn format_traversal_error(path: &Path, cause: &str) -> CliError {
    CliError(format!(
        "cannot audit {}: {cause}; make the directory readable or pass a different PATH",
        super::quote(path.display())
    ))
}

/// Parses `src` (the contents of a single `.rs` file, attributed to
/// `display` in findings) and walks it for audit findings. Only `rules` are
/// evaluated. An empty source has no findings. An unparseable source is not
/// an error — not every `.rs` file under a crate root need parse standalone
/// (generated templates, say), and a stdin editor buffer may be mid-edit —
/// but it is recorded in `skipped` with the parser's position and message,
/// so the caller can report it rather than imply it was audited (#144).
///
/// Each function is audited once, against its own body: nested items (a
/// `fn` or `impl` declared inside a function body) are skipped by every
/// rule's walk and audited separately as functions of their own, so one
/// source expression is never reported twice.
fn audit_source(
    src: &str,
    display: &str,
    rules: &[Rule],
    findings: &mut Vec<Finding>,
    skipped: &mut Vec<Skipped>,
) {
    let file = match syn::parse_file(src) {
        Ok(file) => file,
        Err(err) => {
            let start = err.span().start();
            skipped.push(Skipped {
                file: display.to_string(),
                line: start.line.max(1),
                // proc-macro2 columns are 0-based; editors count from 1.
                column: start.column + 1,
                message: parse_error_message(&err),
            });
            return;
        }
    };

    let mut helpers = HashMap::new();
    let mut address_aliases = HashSet::new();
    let mut i128_aliases = HashSet::new();
    collect_file_items(&file, &mut helpers, &mut address_aliases, &mut i128_aliases);

    let mut visitor = FunctionVisitor {
        path: display.to_string(),
        rules,
        findings,
        contract_impl: None,
        contract_impl_suppressed: false,
        helpers: &helpers,
        address_aliases: &address_aliases,
        i128_aliases: &i128_aliases,
    };
    visitor.visit_file(&file);
}

fn collect_file_items<'a>(
    file: &'a syn::File,
    helpers: &mut HashMap<String, Vec<&'a syn::Block>>,
    address_aliases: &mut HashSet<String>,
    i128_aliases: &mut HashSet<String>,
) {
    for item in &file.items {
        match item {
            Item::Fn(f) => {
                helpers
                    .entry(f.sig.ident.to_string())
                    .or_default()
                    .push(&f.block);
            }
            Item::Impl(i) => {
                for impl_item in &i.items {
                    if let syn::ImplItem::Fn(f) = impl_item {
                        helpers
                            .entry(f.sig.ident.to_string())
                            .or_default()
                            .push(&f.block);
                    }
                }
            }
            Item::Use(u) => {
                collect_use_tree(&u.tree, false, address_aliases, i128_aliases);
            }
            _ => {}
        }
    }

    let mut changed = true;
    let mut passes = 0;
    while changed && passes < 10 {
        changed = false;
        passes += 1;
        for item in &file.items {
            if let Item::Type(t) = item {
                let name = t.ident.to_string();
                if type_is_address(&t.ty, address_aliases) {
                    if address_aliases.insert(name.clone()) {
                        changed = true;
                    }
                }
                if type_is_i128(&t.ty, i128_aliases) {
                    if i128_aliases.insert(name) {
                        changed = true;
                    }
                }
            }
        }
    }
}

fn collect_use_tree(
    tree: &syn::UseTree,
    prefix_ends_in_address: bool,
    address_aliases: &mut HashSet<String>,
    i128_aliases: &mut HashSet<String>,
) {
    match tree {
        syn::UseTree::Path(p) => {
            let is_address = p.ident == "Address";
            collect_use_tree(&p.tree, is_address, address_aliases, i128_aliases);
        }
        syn::UseTree::Name(_) => {}
        syn::UseTree::Rename(r) => {
            if r.ident == "Address" || prefix_ends_in_address {
                address_aliases.insert(r.rename.to_string());
            }
            if r.ident == "i128" {
                i128_aliases.insert(r.rename.to_string());
            }
        }
        syn::UseTree::Group(g) => {
            for item in &g.items {
                collect_use_tree(item, prefix_ends_in_address, address_aliases, i128_aliases);
            }
        }
        syn::UseTree::Glob(_) => {}
    }
}

/// The parser's message, made actionable for tokenizer failures: syn reports
/// an unbalanced delimiter or unterminated literal only as "cannot parse
/// string into token stream", pointing at the offending token.
fn parse_error_message(err: &syn::Error) -> String {
    let message = err.to_string();
    if message == "cannot parse string into token stream" {
        format!(
            "unbalanced delimiter, or an unterminated string, character literal or comment \
             ({message})"
        )
    } else {
        message
    }
}

/// Which functions of an `impl` block are contract entry points (#134).
///
/// Soroban exports the `pub` functions of an `impl` marked `#[contractimpl]`
/// (in any spelling, e.g. `#[soroban_sdk::contractimpl]` or
/// `#[contractimpl(contracttrait)]`), and every function of a
/// `#[contractimpl]` trait impl, whose methods take the trait's visibility.
/// Free functions and methods of other `impl` blocks are ordinary Rust
/// helpers: callers reach them only through an entry point, which is where
/// `require_auth` belongs, so `missing-require-auth` does not apply to them.
/// Macros are not expanded, so entry points generated by a macro are not seen.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum EntryPoints {
    /// `#[contractimpl] impl Contract { .. }`: only `pub fn`s.
    PublicFns,
    /// `#[contractimpl] impl Trait for Contract { .. }`: every fn.
    AllFns,
}

fn is_contractimpl(attrs: &[Attribute]) -> bool {
    attrs.iter().any(|attr| {
        attr.path()
            .segments
            .last()
            .is_some_and(|seg| seg.ident == "contractimpl")
    })
}

fn has_auth_suppression(attrs: &[Attribute]) -> bool {
    attrs.iter().any(is_auth_suppression_attr)
}

fn is_auth_suppression_attr(attr: &Attribute) -> bool {
    if !attr.path().is_ident("allow") {
        return false;
    }
    let syn::Meta::List(meta_list) = &attr.meta else {
        return false;
    };
    let Ok(nested) = meta_list.parse_args_with(
        syn::punctuated::Punctuated::<syn::Meta, syn::Token![,]>::parse_terminated,
    ) else {
        return false;
    };
    for meta in nested {
        if let syn::Meta::Path(p) = meta {
            let idents: Vec<_> = p.segments.iter().map(|s| s.ident.to_string()).collect();
            if idents.last().map(|s| s.as_str()) == Some("missing_require_auth") {
                if idents.len() == 1
                    || (idents.len() == 2
                        && matches!(idents[0].as_str(), "soroban_testkit" | "testkit" | "audit"))
                {
                    return true;
                }
            }
        }
    }
    false
}

struct FunctionVisitor<'a> {
    path: String,
    rules: &'a [Rule],
    findings: &'a mut Vec<Finding>,
    /// Set while visiting the items of a `#[contractimpl]` block.
    contract_impl: Option<EntryPoints>,
    contract_impl_suppressed: bool,
    helpers: &'a HashMap<String, Vec<&'a syn::Block>>,
    address_aliases: &'a HashSet<String>,
    i128_aliases: &'a HashSet<String>,
}

impl<'ast> Visit<'ast> for FunctionVisitor<'_> {
    fn visit_item_fn(&mut self, node: &'ast ItemFn) {
        let line = fn_line(&node.sig);
        // A free function is never an entry point, even when nested in one.
        let suppressed = has_auth_suppression(&node.attrs);
        self.audit_function(&node.sig, &node.block, line, false, suppressed);
        self.audit_i128_arithmetic(&node.sig, &node.block);
        // Continues into nested items, which are audited as their own functions
        visit::visit_item_fn(self, node);
    }

    fn visit_item_impl(&mut self, node: &'ast ItemImpl) {
        let outer = self.contract_impl;
        let outer_suppressed = self.contract_impl_suppressed;
        self.contract_impl = is_contractimpl(&node.attrs).then_some(if node.trait_.is_some() {
            EntryPoints::AllFns
        } else {
            EntryPoints::PublicFns
        });
        self.contract_impl_suppressed = has_auth_suppression(&node.attrs);
        visit::visit_item_impl(self, node);
        self.contract_impl = outer;
        self.contract_impl_suppressed = outer_suppressed;
    }

    fn visit_impl_item_fn(&mut self, node: &'ast ImplItemFn) {
        let line = fn_line(&node.sig);
        let entry_point = match self.contract_impl {
            Some(EntryPoints::AllFns) => true,
            Some(EntryPoints::PublicFns) => matches!(node.vis, Visibility::Public(_)),
            None => false,
        };
        let suppressed = self.contract_impl_suppressed || has_auth_suppression(&node.attrs);
        self.audit_function(&node.sig, &node.block, line, entry_point, suppressed);
        self.audit_i128_arithmetic(&node.sig, &node.block);
        visit::visit_impl_item_fn(self, node);
    }
}

impl FunctionVisitor<'_> {
    fn runs(&self, rule: Rule) -> bool {
        self.rules.contains(&rule)
    }

    fn audit_function(
        &mut self,
        sig: &Signature,
        body: &syn::Block,
        line: usize,
        entry_point: bool,
        suppressed: bool,
    ) {
        let name = sig.ident.to_string();

        // Rule 1: an entry point's Address parameter that's never checked
        // with require_auth. Helpers are reached through an entry point.
 fix/audit-136-138-139
        let address_params = address_typed_params(sig);
        if entry_point && self.runs(Rule::MissingRequireAuth) && !address_params.is_empty() {
            let mut tracked_addrs = HashSet::new();
            for arg in &sig.inputs {
                if let FnArg::Typed(pat_type) = arg {
                    if type_is_address(&pat_type.ty) {
                        extract_pat_idents(&pat_type.pat, &mut tracked_addrs);
                    }
                }
            }
            let mut auth_visitor = RequireAuthVisitor {
                tracked_addrs,

        let address_params = address_typed_params(sig, self.address_aliases);
        if entry_point
            && !suppressed
            && self.runs(Rule::MissingRequireAuth)
            && !address_params.is_empty()
        {
            let mut auth_visitor = RequireAuthVisitor {
                helpers: self.helpers,
                visiting: HashSet::new(),
 main
                has_require_auth: false,
            };
            auth_visitor.visit_block(body);
            if !auth_visitor.has_require_auth {
                self.push(
                    line,
                    Rule::MissingRequireAuth.id(),
                    "warning",
                    format!(
                        "fn {name} takes Address parameter(s) {address_params:?} but never calls \
                         require_auth on any of them in its body"
                    ),
                );
            }
        }

        // Rule 3: a storage read with no extend_ttl anywhere in the function.
        if !self.runs(Rule::MissingTtlBump) {
            return;
        }
        let mut storage_visitor = StorageAccessVisitor {
            storage_type: None,
            has_extend_ttl: false,
        };
        storage_visitor.visit_block(body);

        if let Some(storage_type) = storage_visitor.storage_type {
            if !storage_visitor.has_extend_ttl {
                self.push(
                    line,
                    Rule::MissingTtlBump.id(),
                    "warning",
                    format!(
                        "fn {name} reads from {} storage but never calls extend_ttl in its body",
                        storage_type
                    ),
                );
            }
        }
    }

    fn push(&mut self, line: usize, rule: &'static str, severity: &'static str, message: String) {
        self.findings.push(Finding {
            file: self.path.clone(),
            line,
            rule,
            severity,
            message,
        });
    }

    fn audit_i128_arithmetic(&mut self, sig: &Signature, body: &syn::Block) {
        if !self.runs(Rule::UncheckedI128Arithmetic) {
            return;
        }
        let mut i128_bindings: HashSet<String> = HashSet::new();
        for arg in &sig.inputs {
            if let FnArg::Typed(pat_type) = arg {
                if type_is_i128(&pat_type.ty, self.i128_aliases) {
                    extract_pat_idents(&pat_type.pat, &mut i128_bindings);
                }
            }
        }

        let mut visitor = I128ArithmeticVisitor {
            path: self.path.clone(),
            fn_name: sig.ident.to_string(),
            i128_bindings,
            i128_aliases: self.i128_aliases,
            findings: self.findings,
        };
        visitor.visit_block(body);
    }
}

fn address_typed_params(sig: &Signature, aliases: &HashSet<String>) -> Vec<String> {
    sig.inputs
        .iter()
        .filter_map(|arg| match arg {
            FnArg::Typed(pat_type) if type_is_address(&pat_type.ty, aliases) => {
                match &*pat_type.pat {
                    Pat::Ident(ident) => Some(ident.ident.to_string()),
                    _ => Some("<pattern>".to_string()),
                }
            }
            _ => None,
        })
        .collect()
}

fn type_is_address(ty: &Type, aliases: &HashSet<String>) -> bool {
    match ty {
        Type::Reference(r) => type_is_address(&r.elem, aliases),
        Type::Group(g) => type_is_address(&g.elem, aliases),
        Type::Paren(p) => type_is_address(&p.elem, aliases),
        Type::Path(p) => {
            p.path.segments.last().is_some_and(|seg| {
                seg.ident == "Address" || aliases.contains(&seg.ident.to_string())
            })
        }
        _ => false,
    }
}

/// The 1-based line of a function's `fn` keyword (#152). Read from the AST
/// rather than from the item's tokens: those start with any outer attribute
/// or doc comment (`#[doc = ...]`), so a documented function would otherwise
/// be reported at its doc comment instead of its definition.
fn fn_line(sig: &Signature) -> usize {
    sig.fn_token.span.start().line
}

/// The 1-based line of an arithmetic operator, which is where arithmetic
/// findings point. Read from the operator's own span (#153), not from a
/// token stream.
fn op_line(op: &BinOp) -> usize {
    let span = match op {
        BinOp::Add(token) => token.spans[0],
        BinOp::Sub(token) => token.spans[0],
        BinOp::Mul(token) => token.spans[0],
        BinOp::Div(token) => token.spans[0],
        // `is_arithmetic` admits only the four operators above.
        _ => Span::call_site(),
    };
    span.start().line
}

/// Rule 2 uses a dedicated AST walk (rather than a text search) because
/// "arithmetic on an i128" needs to know which bindings are actually
/// typed `i128`, and a plain string search over the token stream can't
/// tell an `i128` value from the substring appearing anywhere else.
///
/// A finding is reported once per arithmetic *expression*, at that
/// expression's line: `a + b + c` parses as `(a + b) + c`, but it is one
/// source expression and produces one finding. Arithmetic in a separate
/// expression nested inside it (e.g. a call argument) is reported on its own.
struct I128ArithmeticVisitor<'a> {
    path: String,
    fn_name: String,
    i128_bindings: HashSet<String>,
    i128_aliases: &'a HashSet<String>,
    findings: &'a mut Vec<Finding>,
}

impl<'ast> Visit<'ast> for I128ArithmeticVisitor<'_> {
    // Nested functions are audited separately with their own bindings
    fn visit_item(&mut self, _node: &'ast Item) {}

    fn visit_local(&mut self, node: &'ast Local) {
        let mut is_i128 = false;
        if let Pat::Type(pat_type) = &node.pat {
 fix/audit-136-138-139
            if type_is_i128(&pat_type.ty) {

            if type_is_i128(&pat_type.ty, self.i128_aliases) {
 main
                is_i128 = true;
            }
        }
        if !is_i128 {
            if let Some(init) = &node.init {
 fix/audit-136-138-139
                if self.touches_i128(&init.expr) {
                    is_i128 = true;
                }
            }
        }
        if is_i128 {
            if let Pat::Ident(ident) = &node.pat {
                self.i128_bindings.insert(ident.ident.to_string());
            } else if let Pat::Type(pat_type) = &node.pat {
                if let Pat::Ident(ident) = &*pat_type.pat {
                    self.i128_bindings.insert(ident.ident.to_string());

                if self.is_i128_expr(&init.expr) {
                    is_i128 = true;
 main
                }
            }
        }
        if is_i128 {
            extract_pat_idents(&node.pat, &mut self.i128_bindings);
        }
        visit::visit_local(self, node);
    }

    fn visit_expr_binary(&mut self, node: &'ast syn::ExprBinary) {
        if !is_arithmetic(&node.op) {
            return visit::visit_expr_binary(self, node);
        }
        let whole = Expr::Binary(node.clone());
        if self.arithmetic_touches_i128(&whole) {
            self.push_arithmetic_finding(line_of(node.op));
        }
        // The arithmetic operators of this expression are covered by the
        // check above; only its non-arithmetic operands may hold separate
        // expressions (calls, blocks, closures) that need their own check.
        self.visit_arithmetic_operands(&whole);
    }

    fn visit_expr_unary(&mut self, node: &'ast syn::ExprUnary) {
        if !matches!(node.op, syn::UnOp::Neg(_)) || is_int_literal(&node.expr) {
            return visit::visit_expr_unary(self, node);
        }
        let whole = Expr::Unary(node.clone());
        if self.arithmetic_touches_i128(&whole) {
            self.push_arithmetic_finding(line_of(node.op));
        }
        self.visit_arithmetic_operands(&whole);
    }

    fn visit_expr_method_call(&mut self, node: &'ast syn::ExprMethodCall) {
        let method = node.method.to_string();
        if !is_unchecked_arithmetic_method(&method) {
            return visit::visit_expr_method_call(self, node);
        }
        let whole = Expr::MethodCall(node.clone());
        if self.arithmetic_touches_i128(&whole) {
            self.push_arithmetic_finding(line_of(&node.method));
        }
        self.visit_arithmetic_operands(&whole);
    }

    fn visit_expr_call(&mut self, node: &'ast syn::ExprCall) {
        if let Expr::Path(p) = &*node.func {
            if is_unchecked_arithmetic_fn_path(&p.path)
                && node
                    .args
                    .iter()
                    .any(|arg| self.arithmetic_touches_i128(arg))
            {
                self.push_arithmetic_finding(line_of(p));
                for arg in &node.args {
                    self.visit_arithmetic_operands(arg);
                }
                return;
            }
        }
        visit::visit_expr_call(self, node);
    }
}

fn is_arithmetic(op: &BinOp) -> bool {
    matches!(
        op,
        BinOp::Add(_)
            | BinOp::Sub(_)
            | BinOp::Mul(_)
            | BinOp::Div(_)
            | BinOp::Rem(_)
            | BinOp::AddAssign(_)
            | BinOp::SubAssign(_)
            | BinOp::MulAssign(_)
            | BinOp::DivAssign(_)
            | BinOp::RemAssign(_)
    )
}

fn is_compound_assign(op: &BinOp) -> bool {
    matches!(
        op,
        BinOp::AddAssign(_)
            | BinOp::SubAssign(_)
            | BinOp::MulAssign(_)
            | BinOp::DivAssign(_)
            | BinOp::RemAssign(_)
    )
}

fn is_unchecked_arithmetic_method(name: &str) -> bool {
    matches!(
        name,
        "add"
            | "sub"
            | "mul"
            | "div"
            | "rem"
            | "neg"
            | "add_assign"
            | "sub_assign"
            | "mul_assign"
            | "div_assign"
            | "rem_assign"
            | "unchecked_add"
            | "unchecked_sub"
            | "unchecked_mul"
            | "unchecked_div"
            | "unchecked_rem"
            | "unchecked_neg"
            | "strict_add"
            | "strict_sub"
            | "strict_mul"
            | "strict_div"
            | "strict_rem"
            | "strict_neg"
    )
}

fn is_safe_arithmetic_method(name: &str) -> bool {
    name.starts_with("checked_")
        || name.starts_with("saturating_")
        || name.starts_with("wrapping_")
        || name.starts_with("overflowing_")
}

fn is_option_unwrap_method(name: &str) -> bool {
    matches!(
        name,
        "unwrap"
            | "expect"
            | "unwrap_or"
            | "unwrap_or_default"
            | "unwrap_or_else"
            | "ok_or"
            | "ok_or_else"
    )
}

fn is_arithmetic_type_or_trait(name: &str) -> bool {
    matches!(
        name,
        "i128"
            | "Add"
            | "Sub"
            | "Mul"
            | "Div"
            | "Rem"
            | "Neg"
            | "AddAssign"
            | "SubAssign"
            | "MulAssign"
            | "DivAssign"
            | "RemAssign"
    )
}

fn is_unchecked_arithmetic_fn_path(path: &syn::Path) -> bool {
    let len = path.segments.len();
    if len < 2 {
        return false;
    }
    let last = path.segments[len - 1].ident.to_string();
    let prev = path.segments[len - 2].ident.to_string();
    is_unchecked_arithmetic_method(&last) && is_arithmetic_type_or_trait(&prev)
}

fn is_safe_arithmetic_fn_path(path: &syn::Path) -> bool {
    let len = path.segments.len();
    if len < 2 {
        return false;
    }
    let last = path.segments[len - 1].ident.to_string();
    let prev = path.segments[len - 2].ident.to_string();
    is_safe_arithmetic_method(&last) && prev == "i128"
}

fn is_int_literal(expr: &Expr) -> bool {
    match expr {
        Expr::Lit(lit) => matches!(lit.lit, syn::Lit::Int(_)),
        Expr::Paren(p) => is_int_literal(&p.expr),
        Expr::Group(g) => is_int_literal(&g.expr),
        _ => false,
    }
}

impl I128ArithmeticVisitor<'_> {
 fix/audit-136-138-139
    fn push_arithmetic_finding(&mut self, line: usize) {
        self.findings.push(Finding {
            file: self.path.clone(),
            line,
            rule: Rule::UncheckedI128Arithmetic.id(),
            severity: "warning",
            message: format!(
                "fn {} does raw arithmetic on an i128 value; prefer checked_/saturating_/ \
                 wrapping_ variants to avoid silent overflow",
                self.fn_name
            ),
        });

    fn is_i128_expr(&self, expr: &Expr) -> bool {
        match expr {
            Expr::Path(p) => p
                .path
                .get_ident()
                .is_some_and(|ident| self.i128_bindings.contains(&ident.to_string())),
            Expr::Lit(lit) => matches!(&lit.lit, syn::Lit::Int(i) if i.suffix() == "i128"),
            Expr::Reference(r) => self.is_i128_expr(&r.expr),
            Expr::Unary(u) if matches!(u.op, syn::UnOp::Deref(_)) => self.is_i128_expr(&u.expr),
            Expr::Paren(p) => self.is_i128_expr(&p.expr),
            Expr::Group(g) => self.is_i128_expr(&g.expr),
            _ => false,
        }
 main
    }

    /// Whether any operand anywhere in this arithmetic expression (looking
    /// through nested arithmetic operators, method forms, and parentheses) is an i128 value.
    fn arithmetic_touches_i128(&self, expr: &Expr) -> bool {
        match expr {
            Expr::Binary(binary) if is_arithmetic(&binary.op) => {
                self.arithmetic_touches_i128(&binary.left)
                    || self.arithmetic_touches_i128(&binary.right)
            }
            Expr::Unary(unary)
                if matches!(unary.op, syn::UnOp::Neg(_)) && !is_int_literal(&unary.expr) =>
            {
                self.arithmetic_touches_i128(&unary.expr)
            }
            Expr::MethodCall(mc)
                if is_unchecked_arithmetic_method(&mc.method.to_string()) =>
            {
                self.arithmetic_touches_i128(&mc.receiver)
                    || mc.args.iter().any(|arg| self.arithmetic_touches_i128(arg))
            }
            Expr::Paren(p) => self.arithmetic_touches_i128(&p.expr),
            Expr::Group(g) => self.arithmetic_touches_i128(&g.expr),
            other => self.touches_i128(other),
        }
    }

    /// Visits the leaf operands of an arithmetic expression, skipping the
    /// arithmetic nodes that belong to the same source expression.
    fn visit_arithmetic_operands(&mut self, expr: &Expr) {
        match expr {
            Expr::Binary(binary) if is_arithmetic(&binary.op) => {
                self.visit_arithmetic_operands(&binary.left);
                self.visit_arithmetic_operands(&binary.right);
            }
            Expr::Unary(unary)
                if matches!(unary.op, syn::UnOp::Neg(_)) && !is_int_literal(&unary.expr) =>
            {
                self.visit_arithmetic_operands(&unary.expr);
            }
            Expr::MethodCall(mc)
                if is_unchecked_arithmetic_method(&mc.method.to_string()) =>
            {
                self.visit_arithmetic_operands(&mc.receiver);
                for arg in &mc.args {
                    self.visit_arithmetic_operands(arg);
                }
            }
            Expr::Paren(p) => self.visit_arithmetic_operands(&p.expr),
            Expr::Group(g) => self.visit_arithmetic_operands(&g.expr),
            other => self.visit_expr(other),
        }
    }

    fn touches_i128(&self, expr: &Expr) -> bool {
        match expr {
            Expr::Path(p) => p
                .path
                .get_ident()
                .is_some_and(|ident| self.i128_bindings.contains(&ident.to_string())),
            Expr::Lit(lit) => matches!(&lit.lit, syn::Lit::Int(i) if i.suffix() == "i128"),
            Expr::Reference(r) => self.touches_i128(&r.expr),
 fix/audit-136-138-139
            Expr::Unary(u) if matches!(u.op, syn::UnOp::Deref(_) | syn::UnOp::Neg(_)) => {
                self.touches_i128(&u.expr)
            }
            Expr::Binary(b) if is_arithmetic(&b.op) && !is_compound_assign(&b.op) => {
                self.touches_i128(&b.left) || self.touches_i128(&b.right)
            }
            Expr::Try(t) => self.touches_i128(&t.expr),
            Expr::Cast(c) => type_is_i128(&c.ty),
            Expr::MethodCall(mc) => {
                let method = mc.method.to_string();
                (is_safe_arithmetic_method(&method)
                    || is_unchecked_arithmetic_method(&method)
                    || is_option_unwrap_method(&method))
                    && self.touches_i128(&mc.receiver)
            }
            Expr::Call(call) => {
                if let Expr::Path(p) = &*call.func {
                    (is_safe_arithmetic_fn_path(&p.path)
                        || is_unchecked_arithmetic_fn_path(&p.path))
                        && call.args.iter().any(|arg| self.touches_i128(arg))
                } else {
                    false
                }
            }

            Expr::Unary(u) if matches!(u.op, syn::UnOp::Deref(_)) => self.touches_i128(&u.expr),
 main
            Expr::Paren(p) => self.touches_i128(&p.expr),
            Expr::Group(g) => self.touches_i128(&g.expr),
            _ => false,
        }
    }
}

 fix/audit-136-138-139
fn extract_pat_idents(pat: &Pat, bindings: &mut HashSet<String>) {
    match pat {
        Pat::Ident(ident) => {
            bindings.insert(ident.ident.to_string());
        }
        Pat::Reference(r) => extract_pat_idents(&r.pat, bindings),
        Pat::Type(t) => extract_pat_idents(&t.pat, bindings),
        Pat::Paren(p) => extract_pat_idents(&p.pat, bindings),
        Pat::Tuple(t) => {
            for elem in &t.elems {
                extract_pat_idents(elem, bindings);
            }
        }
        _ => {}
    }
}

fn remove_pat_idents(pat: &Pat, bindings: &mut HashSet<String>) {
    let mut idents = HashSet::new();
    extract_pat_idents(pat, &mut idents);
    for id in idents {
        bindings.remove(&id);
    }
}

fn type_is_i128(ty: &Type) -> bool {
    matches!(ty, Type::Path(p) if p.path.is_ident("i128"))
}

/// Visitor that checks if require_auth is called on a tracked Address parameter
/// (or a local alias of one) in the function body.
struct RequireAuthVisitor {
    tracked_addrs: HashSet<String>,

fn type_is_i128(ty: &Type, aliases: &HashSet<String>) -> bool {
    match ty {
        Type::Reference(r) => type_is_i128(&r.elem, aliases),
        Type::Group(g) => type_is_i128(&g.elem, aliases),
        Type::Paren(p) => type_is_i128(&p.elem, aliases),
        Type::Path(p) => {
            p.path.is_ident("i128")
                || p.path.segments.last().is_some_and(|seg| {
                    seg.ident == "i128" || aliases.contains(&seg.ident.to_string())
                })
        }
        _ => false,
    }
}

fn extract_pat_idents(pat: &Pat, bindings: &mut HashSet<String>) {
    match pat {
        Pat::Ident(ident) => {
            bindings.insert(ident.ident.to_string());
        }
        Pat::Reference(r) => extract_pat_idents(&r.pat, bindings),
        Pat::Type(t) => extract_pat_idents(&t.pat, bindings),
        Pat::Paren(p) => extract_pat_idents(&p.pat, bindings),
        Pat::Tuple(t) => {
            for elem in &t.elems {
                extract_pat_idents(elem, bindings);
            }
        }
        _ => {}
    }
}

/// Visitor that checks if require_auth is called anywhere in the function body
/// or in delegated helper functions.
struct RequireAuthVisitor<'a> {
    helpers: &'a HashMap<String, Vec<&'a syn::Block>>,
    visiting: HashSet<String>,
 main
    has_require_auth: bool,
}

impl<'ast> Visit<'ast> for RequireAuthVisitor<'_> {
    // A nested function's require_auth does not authorize the outer function
    fn visit_item(&mut self, _node: &'ast Item) {}

    fn visit_local(&mut self, node: &'ast Local) {
        if let Some(init) = &node.init {
            if self.is_tracked_addr(&init.expr) {
                extract_pat_idents(&node.pat, &mut self.tracked_addrs);
            } else {
                remove_pat_idents(&node.pat, &mut self.tracked_addrs);
            }
        } else {
            remove_pat_idents(&node.pat, &mut self.tracked_addrs);
        }
        visit::visit_local(self, node);
    }

    fn visit_expr(&mut self, node: &'ast Expr) {
        if self.has_require_auth {
            return;
        }
        if let Expr::MethodCall(mc) = node {
            if (mc.method == "require_auth" || mc.method == "require_auth_for_args")
                && self.is_tracked_addr(&mc.receiver)
            {
                self.has_require_auth = true;
                return;
            }
            _ => {}
        }
        visit::visit_expr(self, node);
    }
}

 fix/audit-136-138-139
impl RequireAuthVisitor {
    fn is_tracked_addr(&self, expr: &Expr) -> bool {
        match expr {
            Expr::Path(p) => p
                .path
                .get_ident()
                .is_some_and(|ident| self.tracked_addrs.contains(&ident.to_string())),
            Expr::Reference(r) => self.is_tracked_addr(&r.expr),
            Expr::Unary(u) if matches!(u.op, syn::UnOp::Deref(_)) => self.is_tracked_addr(&u.expr),
            Expr::Paren(p) => self.is_tracked_addr(&p.expr),
            Expr::Group(g) => self.is_tracked_addr(&g.expr),
            Expr::MethodCall(mc)
                if matches!(mc.method.to_string().as_str(), "clone" | "to_owned") =>
            {
                self.is_tracked_addr(&mc.receiver)
            }
            _ => false,
        }

impl RequireAuthVisitor<'_> {
    fn check_helper_auth(&mut self, name: &str) -> bool {
        if let Some(blocks) = self.helpers.get(name) {
            if self.visiting.insert(name.to_string()) {
                for block in blocks {
                    let mut inner_visitor = RequireAuthVisitor {
                        helpers: self.helpers,
                        visiting: self.visiting.clone(),
                        has_require_auth: false,
                    };
                    inner_visitor.visit_block(block);
                    if inner_visitor.has_require_auth {
                        self.visiting.remove(name);
                        return true;
                    }
                }
                self.visiting.remove(name);
            }
        }
        false
 main
    }
}

/// Visitor that checks for storage access patterns (.persistent()/.temporary() with .get())
/// and extend_ttl calls. Replaces text heuristics with AST inspection that handles
/// formatting variations transparently.
struct StorageAccessVisitor {
    storage_type: Option<&'static str>,
    has_extend_ttl: bool,
}

impl<'ast> Visit<'ast> for StorageAccessVisitor {
    // A nested function's storage access is reported for that function only
    fn visit_item(&mut self, _node: &'ast Item) {}

    fn visit_expr(&mut self, node: &'ast Expr) {
        if let Expr::MethodCall(mc) = node {
            if mc.method == "extend_ttl" {
                self.has_extend_ttl = true;
            }
            // Check for .persistent().get() or .temporary().get() chain
            if mc.method == "get" {
                if let Expr::MethodCall(storage_call) = &*mc.receiver {
                    if storage_call.method == "persistent" && self.storage_type.is_none() {
                        self.storage_type = Some("persistent");
                    } else if storage_call.method == "temporary" && self.storage_type.is_none() {
                        self.storage_type = Some("temporary");
                    }
                }
            }
        }
        visit::visit_expr(self, node);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn json_help_states_the_current_schema_version() {
        // `audit --help` documents `--json` as "(`schema_version` 1)"; bump
        // that text (and the help snapshot) together with the constant.
        assert_eq!(AUDIT_JSON_SCHEMA_VERSION, 1);
    }

    #[test]
    fn json_document_declares_schema_version_and_findings() {
        let findings = vec![Finding {
            file: "src/lib.rs".to_string(),
            line: 7,
            rule: "missing-require-auth",
            severity: "warning",
            message: "fn pay takes Address parameter(s) [\"from\"]".to_string(),
        }];
        let json = render_json(&findings, &[], &Rule::ALL);
        assert!(json.starts_with("{\"schema_version\":1,"), "{json}");
        assert!(json.contains("\"finding_count\":1"), "{json}");
        assert!(json.contains("\"line\":7"), "{json}");
        assert!(json.contains("\"rule\":\"missing-require-auth\""), "{json}");
        assert!(json.contains(r#"[\"from\"]"#), "{json}");
    }

    #[test]
    fn json_document_for_no_findings_is_an_empty_list() {
        let json = render_json(&[], &[], &Rule::ALL);
        assert!(
            json.ends_with(",\"finding_count\":0,\"findings\":[]}"),
            "{json}"
        );
    }

    #[test]
    fn finding_paths_are_relative_to_the_audited_root() {
        let p = |s: &str| PathBuf::from(s);
        assert_eq!(finding_path(&p("."), &p("./src/lib.rs")), "src/lib.rs");
        assert_eq!(
            finding_path(&p("examples/vault/src"), &p("examples/vault/src/lib.rs")),
            "lib.rs"
        );
        assert_eq!(
            finding_path(&p("/abs/crate"), &p("/abs/crate/src/nested/mod.rs")),
            "src/nested/mod.rs"
        );
    }

    #[test]
    fn a_single_file_root_reports_its_file_name() {
        let file = PathBuf::from("crates/cli/src/commands/mod.rs");
        assert_eq!(finding_path(&file, &file), "mod.rs");
    }

    #[test]
    fn a_file_outside_the_root_keeps_its_given_path() {
        assert_eq!(
            finding_path(Path::new("src"), Path::new("other/lib.rs")),
            "other/lib.rs"
        );
    }

    #[test]
    fn json_strings_escape_quotes_backslashes_and_control_characters() {
        let expected = "\"a\\\"b\\\\c\\nd\\u0001\"";
        assert_eq!(json_string("a\"b\\c\nd\u{1}"), expected);
    }

    #[cfg(unix)]
    #[test]
    fn non_utf8_path_components_are_displayed_with_replacement_characters_without_panicking() {
        use std::ffi::OsStr;
        use std::os::unix::ffi::OsStrExt;

        let p = |s: &str| PathBuf::from(s);
        assert_eq!(finding_path(&p("."), &p("./src/lib.rs")), "src/lib.rs");

        let file = PathBuf::from(OsStr::from_bytes(b"./src/\xff/lib-\xfe.rs"));
        assert_eq!(finding_path(&p("."), &file), "src/\u{fffd}/lib-\u{fffd}.rs");
    }

    #[cfg(unix)]
    #[test]
    fn non_utf8_file_name_is_audited_under_a_lossy_path_when_collecting() {
        use std::ffi::OsStr;
        use std::os::unix::ffi::OsStrExt;

        let tree = TempTree::new("non-utf8");
        let name = OsStr::from_bytes(b"lib-\xff.rs");
        let path = tree.root.join(name);
        std::fs::write(&path, "fn f(a: i128) -> i128 {\n    a + 1i128\n}\n")
            .expect("write non-UTF8 source");

        let collection = collect(&tree.root);
        assert_eq!(displays(&collection), vec!["lib-\u{fffd}.rs"]);

        let (path, display) = &collection.files[0];
        let mut findings = Vec::new();
        let mut skipped = Vec::new();
        audit_file(path, display, &Rule::ALL, &mut findings, &mut skipped)
            .expect("audit non-UTF8 file");
        assert!(skipped.is_empty(), "{skipped:?}");
        assert_eq!(findings.len(), 1, "{findings:?}");
        assert_eq!(findings[0].file, "lib-\u{fffd}.rs");
    }

    #[test]
    fn function_line_is_the_fn_keyword_line_when_a_doc_comment_precedes_it() {
        let src = "#[contractimpl]\nimpl Vault {\n    /// Pays `to`.\n    /// Second doc line.\n    #[allow(dead_code)]\n    pub fn pay(\n        env: Env,\n        to: Address,\n    ) {}\n}\n";
        assert_eq!(auth_findings(src), vec![6]);
    }

    #[test]
    fn function_line_ignores_block_and_line_comments_above_multiline_signatures() {
        let src = "/*\n * block\n */\n// line comment\nfn helper() {}\n#[contractimpl]\nimpl Vault {\n    pub fn pay(\n        env: Env,\n        to: Address,\n    ) {}\n}\n";
        assert_eq!(auth_findings(src), vec![8]);
    }

    #[test]
    fn storage_finding_points_at_the_fn_line_when_documented_and_multiline() {
        let src = "/// Reads the balance.\n#[allow(unused)]\nfn balance(\n    env: Env,\n) -> i128 {\n    env.storage().persistent().get(&key)\n}\n";
        assert_eq!(
            rules_of(src, &[Rule::MissingTtlBump]),
            vec![("missing-ttl-bump", 3)]
        );
    }

    #[test]
    fn multiline_function_line_number_is_the_function_definition_line() {
        let src = r#"
        #[contractimpl]
        impl C {
            pub fn pay(
                env: Env,
                to: Address,
            ) {}
        }
        "#;
        assert_eq!(auth_findings(src), vec![4]);
    }

    #[test]
    fn arithmetic_findings_point_at_operator_lines_in_multiline_functions() {
        let src = r#"
fn pay(
    amount: i128,
) {
    let first = amount
        + 1;
    let second = amount
        * 2;
}
"#;
        let mut findings = Vec::new();
        audit_source(src, "test.rs", &Rule::ALL, &mut findings, &mut Vec::new());

        let lines: Vec<_> = findings
            .iter()
            .filter(|finding| finding.rule == "unchecked-i128-arithmetic")
            .map(|finding| finding.line)
            .collect();
        assert_eq!(lines, vec![6, 8], "{findings:?}");
    }

    #[test]
    fn traversal_errors_name_the_unreadable_path_and_the_io_cause() {
        let message = format_traversal_error(Path::new("src"), "permission denied").0;

        assert!(
            message.starts_with("cannot audit `src`: permission denied"),
            "{message}"
        );
        assert!(message.contains("make the directory readable"), "{message}");
    }

    #[test]
    fn multiline_comment_does_not_affect_function_line_detection() {
        let src = r#"
        /*
         * Multi-line comment
         * spanning multiple lines
         */
        #[contractimpl]
        impl C {
            pub fn documented_function(addr: Address) {
                // body
            }
        }
        "#;
        // The finding is at the `pub fn` line (line 8), not at a comment line.
        assert_eq!(auth_findings(src), vec![8]);
    }

    #[test]
    fn require_auth_detection_ignores_formatting() {
        // Verify that require_auth is detected regardless of whitespace/formatting
        let src = r#"
        #[contractimpl]
        impl C {
            pub fn transfer(to: Address, amount: i128) {
                if amount > 0 {
                    // Various formatting of require_auth calls
                    to.require_auth();
                }
            }
        }
        "#;
        let mut findings = Vec::new();
        audit_source(src, "test.rs", &Rule::ALL, &mut findings, &mut Vec::new());
        // Should NOT report missing-require-auth because require_auth is called
        let missing_auth = findings.iter().any(|f| f.rule == "missing-require-auth");
        assert!(
            !missing_auth,
            "require_auth should be detected despite formatting variations"
        );
    }

    #[test]
    fn storage_access_detection_ignores_formatting() {
        // Verify that .persistent().get() and .temporary().get() are detected
        // regardless of whitespace
        let src = r#"
        fn read_storage() {
            let x = env
                .storage()
                .persistent()
                .get(&key);
        }
        "#;
        let mut findings = Vec::new();
        audit_source(src, "test.rs", &Rule::ALL, &mut findings, &mut Vec::new());
        // Should report missing-ttl-bump because there's no extend_ttl call
        let missing_ttl = findings.iter().any(|f| f.rule == "missing-ttl-bump");
        assert!(
            missing_ttl,
            "storage access should be detected despite formatting variations"
        );
    }

    #[test]
    fn extend_ttl_detection_works_with_various_formatting() {
        let src = r#"
        fn safe_read() {
            let x = env.storage().persistent().get(&key);
            env.storage().persistent().extend_ttl(&key, 100000, 100000);
        }
        "#;
        let mut findings = Vec::new();
        audit_source(src, "test.rs", &Rule::ALL, &mut findings, &mut Vec::new());
        // Should NOT report missing-ttl-bump
        let missing_ttl = findings.iter().any(|f| f.rule == "missing-ttl-bump");
        assert!(
            !missing_ttl,
            "extend_ttl should be detected despite formatting variations"
        );
    }

    fn rules_of(src: &str, rules: &[Rule]) -> Vec<(&'static str, usize)> {
        let mut findings = Vec::new();
        audit_source(src, "test.rs", rules, &mut findings, &mut Vec::new());
        findings.iter().map(|f| (f.rule, f.line)).collect()
    }

    #[test]
    fn chained_i128_arithmetic_is_one_finding_at_its_line() {
        let src = "fn f(a: i128, b: i128, c: i128) -> i128 {\n    let x = 1;\n    (a + b) * c - a / b\n}\n";
        assert_eq!(
            rules_of(src, &Rule::ALL),
            vec![("unchecked-i128-arithmetic", 3)]
        );
    }

    #[test]
    fn arithmetic_inside_a_call_argument_is_a_separate_expression() {
        let src = "fn f(a: i128, b: i128) -> i128 {\n    a + g(\n        b * 2i128,\n    )\n}\n";
        assert_eq!(
            rules_of(src, &Rule::ALL),
            vec![
                ("unchecked-i128-arithmetic", 2),
                ("unchecked-i128-arithmetic", 3),
            ]
        );
    }

    #[test]
    fn non_i128_arithmetic_still_reaches_nested_i128_expressions() {
        let src = "fn f(n: u32, a: i128) -> u32 {\n    n + h(\n        a - 1i128,\n    )\n}\n";
        assert_eq!(
            rules_of(src, &Rule::ALL),
            vec![("unchecked-i128-arithmetic", 3)]
        );
    }

    #[test]
    fn nested_functions_are_audited_once_as_themselves() {
        let src = r#"
#[contractimpl]
impl C {
pub fn outer(env: Env, owner: Address) {
    fn inner(env: Env, owner: Address, a: i128) -> i128 {
        owner.require_auth();
        let v = env.storage().temporary().get(&k);
        a * 2i128
    }
    impl Helper {
        fn method(env: Env) {
            let v = env.storage().persistent().get(&k);
        }
    }
}
}
"#;
        let mut found = rules_of(src, &Rule::ALL);
        found.sort();
        assert_eq!(
            found,
            vec![
                // outer: inner's require_auth does not authorize outer
                ("missing-require-auth", 4),
                // inner: its own storage read and arithmetic, once each
                ("missing-ttl-bump", 5),
                // method in the nested impl block
                ("missing-ttl-bump", 11),
                ("unchecked-i128-arithmetic", 8),
            ]
        );
    }

    #[test]
    fn only_selected_rules_run() {
        let src = "#[contractimpl]\nimpl C {\npub fn f(env: Env, who: Address, a: i128) -> i128 {\n    let v = env.storage().persistent().get(&k);\n    a + 1i128\n}\n}\n";
        assert_eq!(rules_of(src, &Rule::ALL).len(), 3);
        assert_eq!(
            rules_of(src, &[Rule::MissingRequireAuth, Rule::MissingTtlBump]),
            vec![("missing-require-auth", 3), ("missing-ttl-bump", 3)]
        );
        assert_eq!(
            rules_of(src, &[Rule::UncheckedI128Arithmetic]),
            vec![("unchecked-i128-arithmetic", 5)]
        );
    }

    #[test]
    fn rule_selection_resolves_include_then_exclude_in_a_fixed_order() {
        use Rule::*;
        assert_eq!(selected_rules(&[], &[]).unwrap(), Rule::ALL.to_vec());
        assert_eq!(
            selected_rules(&[MissingTtlBump, MissingRequireAuth], &[]).unwrap(),
            vec![MissingRequireAuth, MissingTtlBump]
        );
        assert_eq!(
            selected_rules(&[], &[UncheckedI128Arithmetic]).unwrap(),
            vec![MissingRequireAuth, MissingTtlBump]
        );
        assert_eq!(
            selected_rules(&[MissingTtlBump, MissingTtlBump], &[]).unwrap(),
            vec![MissingTtlBump]
        );
        assert!(selected_rules(&[MissingTtlBump], &[MissingTtlBump])
            .unwrap_err()
            .contains("`missing-ttl-bump` is both selected"));
        assert!(selected_rules(&[], &Rule::ALL)
            .unwrap_err()
            .contains("nothing to audit"));
    }

    #[test]
    fn rule_ids_match_the_cli_value_names() {
        for rule in Rule::ALL {
            let value = rule.to_possible_value().unwrap();
            assert_eq!(value.get_name(), rule.id());
        }
    }

    #[test]
    fn json_lists_the_rules_that_ran() {
        let json = render_json(&[], &[], &[Rule::MissingTtlBump]);
        assert!(json.contains("\"rules\":[\"missing-ttl-bump\"],"), "{json}");
    }

    // ---- #134: auth findings only for contract entry points -------------

    fn auth_findings(src: &str) -> Vec<usize> {
        rules_of(src, &[Rule::MissingRequireAuth])
            .into_iter()
            .map(|(_, line)| line)
            .collect()
    }

    #[test]
    fn free_helper_functions_are_not_entry_points() {
        let src = "fn helper(env: Env, who: Address) -> u32 {\n    1\n}\npub fn also_helper(who: Address) {}\n";
        assert_eq!(auth_findings(src), Vec::<usize>::new());
    }

    #[test]
    fn only_pub_fns_of_a_contractimpl_block_are_entry_points() {
        let src = r#"
#[contractimpl]
impl Vault {
    pub fn withdraw(env: Env, to: Address) {}
    fn private_helper(env: Env, to: Address) {}
    pub(crate) fn crate_helper(to: Address) {}
}
"#;
        assert_eq!(auth_findings(src), vec![4]);
    }

    #[test]
    fn helper_impls_without_contractimpl_are_not_entry_points() {
        let src = "impl Vault {\n    pub fn withdraw(env: Env, to: Address) {}\n}\nimpl Helper for Vault {\n    fn check(to: Address) {}\n}\n";
        assert_eq!(auth_findings(src), Vec::<usize>::new());
    }

    #[test]
    fn every_fn_of_a_contractimpl_trait_impl_is_an_entry_point() {
        let src = "#[contractimpl]\nimpl Token for Vault {\n    fn transfer(env: Env, from: Address) {}\n}\n";
        assert_eq!(auth_findings(src), vec![3]);
    }

    #[test]
    fn contractimpl_is_recognised_in_any_spelling() {
        let qualified =
            "#[soroban_sdk::contractimpl]\nimpl Vault {\n    pub fn pay(to: Address) {}\n}\n";
        let with_args = "#[contractimpl(contracttrait)]\nimpl Token for Vault {\n    fn pay(to: Address) {}\n}\n";
        assert_eq!(auth_findings(qualified), vec![3]);
        assert_eq!(auth_findings(with_args), vec![3]);
    }

    #[test]
    fn helpers_nested_in_an_entry_point_are_not_entry_points() {
        let src = r#"
#[contractimpl]
impl Vault {
    pub fn pay(env: Env, to: Address) {
        to.require_auth();
        fn inner(to: Address) {}
        impl Local {
            pub fn method(to: Address) {}
        }
    }
}
"#;
        assert_eq!(auth_findings(src), Vec::<usize>::new());
    }

    #[test]
    fn entry_points_after_a_nested_helper_impl_are_still_checked() {
        let src = r#"
#[contractimpl]
impl Vault {
    pub fn first(env: Env) {
        impl Local {
            fn helper() {}
        }
    }
    pub fn second(env: Env, to: Address) {}
}
"#;
        assert_eq!(auth_findings(src), vec![9]);
    }

    #[test]
    fn other_rules_still_apply_to_helper_functions() {
        let src = "fn helper(env: Env, a: i128) -> i128 {\n    let v = env.storage().persistent().get(&k);\n    a + 1i128\n}\n";
        let mut found = rules_of(src, &Rule::ALL);
        found.sort();
        assert_eq!(
            found,
            vec![("missing-ttl-bump", 1), ("unchecked-i128-arithmetic", 3)]
        );
    }

    // ---- #142: --include / --exclude globs ------------------------------

    fn filter(include: &[&str], exclude: &[&str]) -> PathFilter {
        let owned = |globs: &[&str]| globs.iter().map(|g| g.to_string()).collect::<Vec<_>>();
        PathFilter::new(&owned(include), &owned(exclude)).unwrap()
    }

    #[test]
    fn no_globs_select_every_file() {
        let f = filter(&[], &[]);
        assert!(!f.is_active());
        assert!(f.selects("src/lib.rs"));
    }

    #[test]
    fn a_bare_name_matches_at_any_depth_like_gitignore() {
        let f = filter(&[], &["generated", "*_pb.rs"]);
        assert!(f.excludes("generated"));
        assert!(!f.selects("generated/types.rs"));
        assert!(!f.selects("src/deep/generated/types.rs"));
        assert!(!f.selects("src/proto/msg_pb.rs"));
        assert!(f.selects("src/lib.rs"));
        assert!(f.selects("src/generated_by_hand.rs"));
    }

    #[test]
    fn a_glob_with_a_slash_is_anchored_at_the_root() {
        let f = filter(&[], &["src/gen/**"]);
        assert!(!f.selects("src/gen/a.rs"));
        assert!(!f.selects("src/gen/nested/b.rs"));
        assert!(f.selects("other/src/gen/a.rs"));
        let star = filter(&[], &["src/*.rs"]);
        assert!(!star.selects("src/lib.rs"));
        assert!(star.selects("src/nested/lib.rs"));
    }

    #[test]
    fn double_star_spans_zero_or_more_segments() {
        let f = filter(&[], &["**/vendor/**"]);
        assert!(!f.selects("vendor/x.rs"));
        assert!(!f.selects("a/b/vendor/c/x.rs"));
        assert!(f.selects("a/vendors/x.rs"));
    }

    #[test]
    fn include_limits_the_audit_and_exclude_wins() {
        let f = filter(&["contracts/**"], &["*_test.rs"]);
        assert!(f.selects("contracts/vault/lib.rs"));
        assert!(!f.selects("scripts/tool.rs"));
        assert!(!f.selects("contracts/vault/lib_test.rs"));
        let dir = filter(&["contracts"], &[]);
        assert!(dir.selects("contracts/vault/lib.rs"));
    }

    #[test]
    fn question_mark_matches_one_character_in_a_segment() {
        let f = filter(&[], &["v?.rs"]);
        assert!(!f.selects("src/v1.rs"));
        assert!(f.selects("src/v10.rs"));
        assert!(f.selects("src/v/.rs"));
    }

    #[test]
    fn leading_dot_slash_trailing_slash_and_backslashes_are_normalised() {
        let f = filter(&[], &["./src\\gen/"]);
        assert!(!f.selects("src/gen/a.rs"));
        assert!(f.selects("src/lib.rs"));
    }

    #[test]
    fn an_empty_glob_is_a_usage_error() {
        let err = PathFilter::new(&[], &["/".to_string()]).unwrap_err();
        assert!(err.contains("--exclude `/` is empty"), "{err}");
    }

    // ---- #144: unparseable files are reported, not dropped --------------

    #[test]
    fn unparseable_source_is_recorded_with_position_and_parser_message() {
        let mut findings = Vec::new();
        let mut skipped = Vec::new();
        audit_source(
            "pub struct Ok;\n\nfn broken( {\n",
            "src/gen.rs",
            &Rule::ALL,
            &mut findings,
            &mut skipped,
        );
        assert!(findings.is_empty());
        assert_eq!(skipped.len(), 1, "{skipped:?}");
        let skip = &skipped[0];
        assert_eq!(skip.file, "src/gen.rs");
        assert_eq!(skip.line, 3, "{skip:?}");
        assert!(skip.column >= 1, "{skip:?}");
        assert!(!skip.message.is_empty(), "{skip:?}");
    }

    #[test]
    fn parse_positions_are_one_based_and_point_at_the_offending_token() {
        let skip = |src: &str| {
            let mut skipped = Vec::new();
            audit_source(src, "x.rs", &Rule::ALL, &mut Vec::new(), &mut skipped);
            skipped.pop().expect("source should be skipped")
        };
        // `let` is not an item: line 2, column 3 (after two spaces).
        let item = skip("fn a() {}\n  let x = ;\n");
        assert_eq!((item.line, item.column), (2, 3), "{item:?}");
        assert!(item.message.starts_with("expected one of"), "{item:?}");
        // The unclosed `{` is the 12th character of line 3.
        let unbalanced = skip("pub struct Ok;\n\nfn broken( {\n");
        assert_eq!(
            (unbalanced.line, unbalanced.column),
            (3, 12),
            "{unbalanced:?}"
        );
        assert!(
            unbalanced.message.starts_with("unbalanced delimiter"),
            "{unbalanced:?}"
        );
    }

    #[test]
    fn empty_and_valid_sources_are_not_skipped() {
        let mut skipped = Vec::new();
        audit_source("", "a.rs", &Rule::ALL, &mut Vec::new(), &mut skipped);
        audit_source(
            "fn ok() {}\n",
            "b.rs",
            &Rule::ALL,
            &mut Vec::new(),
            &mut skipped,
        );
        assert_eq!(skipped, vec![]);
    }

    #[test]
    fn json_lists_skipped_files_before_the_findings() {
        let skipped = vec![Skipped {
            file: "src/gen.rs".to_string(),
            line: 3,
            column: 12,
            message: "expected `)`".to_string(),
        }];
        let json = render_json(&[], &skipped, &Rule::ALL);
        assert!(
            json.contains(
                "\"skipped\":[{\"file\":\"src/gen.rs\",\"line\":3,\"column\":12,\"message\":\"expected `)`\"}],\"finding_count\":0"
            ),
            "{json}"
        );
        assert!(
            json.ends_with(",\"finding_count\":0,\"findings\":[]}"),
            "{json}"
        );
    }

 fix/audit-136-138-139
    #[test]
    fn i128_remainder_negation_and_compound_assignments_are_flagged() {
        let src = r#"
fn ops(mut a: i128, b: i128) -> i128 {
    let rem = a % b;
    let neg = -a;
    a += b;
    a -= 1;
    a *= 2;
    a /= 3;
    a %= 5;
    let lit_neg = -1i128;
    lit_neg
}
"#;
        assert_eq!(
            rules_of(src, &[Rule::UncheckedI128Arithmetic]),
            vec![
                ("unchecked-i128-arithmetic", 3),
                ("unchecked-i128-arithmetic", 4),
                ("unchecked-i128-arithmetic", 5),
                ("unchecked-i128-arithmetic", 6),
                ("unchecked-i128-arithmetic", 7),
                ("unchecked-i128-arithmetic", 8),
                ("unchecked-i128-arithmetic", 9),
            ]
        );
    }

    #[test]
    fn non_i128_remainder_negation_and_compound_assignments_are_not_flagged() {
        let src = r#"
fn safe_u64(mut a: u64, b: u64, mut c: i64) -> i64 {
    let r = a % b;
    a += b;
    a -= 1;
    a *= 2;
    a /= 3;
    a %= 5;
    c = -c;
    c
}
"#;
        assert_eq!(
            rules_of(src, &[Rule::UncheckedI128Arithmetic]),
            Vec::<(&'static str, usize)>::new()
        );
    }

    #[test]
    fn checked_and_saturating_method_chains_are_not_flagged() {
        let src = r#"
fn safe_chains(a: i128, b: i128, c: i128) -> Option<i128> {
    let x = a.checked_add(b)?.checked_mul(c)?;
    let y = x.saturating_add(1).wrapping_sub(2);
    let z = i128::checked_div(y, 3)?;
    Some(z)
=======
    // ---- #153: auth and TTL checks read the AST, not formatting ----------

    #[test]
    fn require_auth_in_a_comment_or_string_does_not_satisfy_the_check() {
        let src = "#[contractimpl]\nimpl Vault {\n    pub fn pay(env: Env, to: Address) {\n        // to.require_auth();\n        let note = \"to.require_auth()\";\n    }\n}\n";
        assert_eq!(auth_findings(src), vec![3]);
    }

    #[test]
    fn require_auth_is_found_regardless_of_spacing_and_line_breaks() {
        let compact = "#[contractimpl]\nimpl Vault {\n    pub fn pay(env: Env, to: Address) {\n        to.require_auth();\n    }\n}\n";
        let spread = "#[contractimpl]\nimpl Vault {\n    pub fn pay(env: Env, to: Address) {\n        to\n            .require_auth ( ) ;\n    }\n}\n";
        assert_eq!(auth_findings(compact), Vec::<usize>::new());
        assert_eq!(auth_findings(spread), Vec::<usize>::new());
    }

    #[test]
    fn storage_read_is_found_regardless_of_spacing_and_line_breaks() {
        let src = "fn read(env: Env) {\n    let v = env\n        .storage ( )\n        . temporary ( )\n        .get ( &k ) ;\n}\n";
        assert_eq!(
            rules_of(src, &[Rule::MissingTtlBump]),
            vec![("missing-ttl-bump", 1)]
        );
    }

    #[test]
    fn extend_ttl_in_a_comment_does_not_satisfy_the_ttl_check() {
        let src = "fn read(env: Env) {\n    // env.storage().persistent().extend_ttl(&k, 1, 1);\n    let v = env.storage().persistent().get(&k);\n}\n";
        assert_eq!(
            rules_of(src, &[Rule::MissingTtlBump]),
            vec![("missing-ttl-bump", 1)]
        );
    }

    // ---- #151: symlinks and directory traversal -------------------------

    /// A throwaway directory under the system temp dir, removed on drop.
    struct TempTree {
        root: PathBuf,
    }

    impl TempTree {
        fn new(name: &str) -> Self {
            use std::time::{SystemTime, UNIX_EPOCH};

            let stamp = SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .expect("system clock after epoch")
                .as_nanos();
            let root = std::env::temp_dir().join(format!(
                "soroban-testkit-audit-{name}-{}-{stamp}",
                std::process::id()
            ));
            std::fs::create_dir_all(&root).expect("create temp audit tree");
            TempTree { root }
        }

        fn write(&self, relative: &str, contents: &str) -> PathBuf {
            let path = self.root.join(relative);
            std::fs::create_dir_all(path.parent().expect("file has a parent"))
                .expect("create parent directories");
            std::fs::write(&path, contents).expect("write temp source");
            path
        }
    }

    impl Drop for TempTree {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.root);
        }
    }

    fn collect(root: &Path) -> Collection {
        collect_sources(root, &PathFilter::default()).expect("collect temp audit tree")
    }

    fn displays(collection: &Collection) -> Vec<&str> {
        collection
            .files
            .iter()
            .map(|(_, display)| display.as_str())
            .collect()
    }

    #[cfg(unix)]
    #[test]
    fn symlinked_directory_inside_the_tree_is_not_followed_when_collecting() {
        let tree = TempTree::new("linked-dir");
        tree.write("src/lib.rs", "fn ok() {}\n");
        tree.write("outside/gen.rs", "fn gen() {}\n");
        std::os::unix::fs::symlink(tree.root.join("outside"), tree.root.join("src/linked"))
            .expect("symlink directory");

        let collection = collect(&tree.root.join("src"));
        assert_eq!(displays(&collection), vec!["lib.rs"]);
        assert_eq!(
            collection.skipped,
            vec![(tree.root.join("src/linked"), "symlink, not followed")]
        );
    }

    #[cfg(unix)]
    #[test]
    fn symlinked_rust_file_inside_the_tree_is_not_audited_twice() {
        let tree = TempTree::new("linked-file");
        let real = tree.write("src/lib.rs", "fn ok() {}\n");
        std::os::unix::fs::symlink(&real, tree.root.join("src/alias.rs")).expect("symlink file");

        let collection = collect(&tree.root.join("src"));
        assert_eq!(displays(&collection), vec!["lib.rs"]);
        assert_eq!(
            collection.skipped,
            vec![(tree.root.join("src/alias.rs"), "symlink, not followed")]
        );
    }

    #[cfg(unix)]
    #[test]
    fn symlinked_root_directory_is_followed_when_named_explicitly() {
        let tree = TempTree::new("linked-root-dir");
        tree.write("real/src/lib.rs", "fn ok() {}\n");
        std::os::unix::fs::symlink(tree.root.join("real/src"), tree.root.join("crate"))
            .expect("symlink root directory");

        let collection = collect(&tree.root.join("crate"));
        assert_eq!(displays(&collection), vec!["lib.rs"]);
        assert!(collection.skipped.is_empty(), "{:?}", collection.skipped);
    }

    #[cfg(unix)]
    #[test]
    fn symlinked_root_file_is_audited_under_its_own_file_name() {
        let tree = TempTree::new("linked-root-file");
        let real = tree.write("real.rs", "fn ok() {}\n");
        let link = tree.root.join("link.rs");
        std::os::unix::fs::symlink(&real, &link).expect("symlink root file");

        let collection = collect(&link);
        assert_eq!(displays(&collection), vec!["link.rs"]);
    }

    #[test]
    fn excluded_directories_are_pruned_and_listed_when_collecting() {
        let tree = TempTree::new("pruned");
        tree.write("src/lib.rs", "fn ok() {}\n");
        tree.write("target/gen.rs", "fn gen() {}\n");

        let collection =
            collect_sources(&tree.root, &filter(&[], &["target"])).expect("collect temp tree");
        assert_eq!(displays(&collection), vec!["src/lib.rs"]);
        assert_eq!(
            collection.skipped,
            vec![(tree.root.join("target"), "--exclude")]
        );
    }

    // ---- #141: auth rule exemptions -------------------------------------

    #[test]
    fn auth_suppression_allows_intentional_open_entry_points() {
        let src = r#"
#[contractimpl]
impl Contract {
    #[allow(soroban_testkit::missing_require_auth)]
    pub fn open_scoped(who: Address) {}

    #[allow(missing_require_auth)]
    pub fn open_unscoped(who: Address) {}

    #[allow(testkit::missing_require_auth)]
    pub fn open_testkit(who: Address) {}

    #[allow(clippy::too_many_arguments, soroban_testkit::missing_require_auth)]
    pub fn open_combined(who: Address) {}

    pub fn unannotated(who: Address) {}
}

#[allow(soroban_testkit::missing_require_auth)]
#[contractimpl]
impl OpenImpl {
    pub fn init(admin: Address) {}
}
"#;
        assert_eq!(auth_findings(src), vec![16]);
    }

    #[test]
    fn malformed_allow_annotations_do_not_suppress_or_panic() {
        let src = r#"
#[contractimpl]
impl Contract {
    #[allow]
    pub fn bare_allow(who: Address) {}

    #[allow = "missing_require_auth"]
    pub fn name_value_allow(who: Address) {}

    #[allow()]
    pub fn empty_allow(who: Address) {}

    #[allow(123)]
    pub fn literal_allow(who: Address) {}

    #[allow(unused_variables)]
    pub fn unrelated_allow(who: Address) {}

    #[allow(clippy::missing_require_auth)]
    pub fn wrong_scope_allow(who: Address) {}
}
"#;
        assert_eq!(auth_findings(src), vec![5, 8, 11, 14, 17, 20]);
    }

    // ---- #135: authorization checked in helper --------------------------

    #[test]
    fn delegated_helper_direct_check_satisfies_auth_rule() {
        let src = r#"
fn check_caller(caller: &Address) {
    caller.require_auth();
}

impl Contract {
    fn verify_self(&self, user: &Address) {
        user.require_auth_for_args(());
    }

    fn assert_admin(admin: &Address) {
        admin.require_auth();
    }
}

#[contractimpl]
impl Contract {
    pub fn via_free_fn(env: Env, caller: Address) {
        check_caller(&caller);
    }

    pub fn via_self_method(&self, env: Env, user: Address) {
        self.verify_self(&user);
    }

    pub fn via_assoc_fn(env: Env, admin: Address) {
        Self::assert_admin(&admin);
    }
}
"#;
        assert_eq!(auth_findings(src), Vec::<usize>::new());
    }

    #[test]
    fn delegated_helper_indirect_check_satisfies_auth_rule() {
        let src = r#"
fn step_one(user: &Address) {
    step_two(user);
}

fn step_two(user: &Address) {
    user.require_auth();
}

#[contractimpl]
impl Contract {
    pub fn execute(env: Env, user: Address) {
        step_one(&user);
    }
}
"#;
        assert_eq!(auth_findings(src), Vec::<usize>::new());
    }

    #[test]
    fn recursive_helpers_without_require_auth_still_report() {
        let src = r#"
fn ping(a: &Address) {
    pong(a);
}

fn pong(a: &Address) {
    ping(a);
}

#[contractimpl]
impl Contract {
    pub fn execute(env: Env, user: Address) {
        ping(&user);
    }
}
"#;
        assert_eq!(auth_findings(src), vec![12]);
    }

    // ---- #140: arithmetic i128 reference tracking -----------------------

    #[test]
    fn i128_reference_parameters_and_deref_arithmetic_are_detected() {
        let src = r#"
fn add_ref(a: &i128) -> i128 {
    *a + 1
}

fn sub_mut_ref(a: &mut i128) -> i128 {
    *a - 2
 main
}
"#;
        assert_eq!(
            rules_of(src, &[Rule::UncheckedI128Arithmetic]),
            Vec::<(&'static str, usize)>::new()
        );
    }

    #[test]
 fix/audit-136-138-139
    fn unchecked_method_forms_and_mixed_chains_are_flagged() {
        let src = r#"
use core::ops::Add;

fn mixed(a: i128, b: i128, c: i128) -> i128 {
    let m1 = a.add(b).mul(c);
    let m2 = a.unchecked_add(b);
    let m3 = i128::add(a, b);
    let m4 = Add::add(a, b);
    let m5 = a.checked_add(b).unwrap() + 1;
    let m6 = (a + b).checked_mul(c).unwrap();
    let m7 = a.checked_add(b + c).unwrap();
    let m8 = a.saturating_add(b) * 2;
    m1 + m2 + m3 + m4 + m5 + m6 + m7 + m8

    fn i128_simple_aliases_and_reference_bindings_are_detected() {
        let src = r#"
type Balance = i128;

fn aliases(a: i128, b: Balance) -> i128 {
    let alias = a;
    let r = &a;
    let deref_alias = *r;
    let x = alias + 1;
    let y = *r * 2;
    let z = deref_alias - 3;
    b / 2
 main
}
"#;
        assert_eq!(
            rules_of(src, &[Rule::UncheckedI128Arithmetic]),
            vec![
 fix/audit-136-138-139
                ("unchecked-i128-arithmetic", 5),
                ("unchecked-i128-arithmetic", 6),
                ("unchecked-i128-arithmetic", 7),

 main
                ("unchecked-i128-arithmetic", 8),
                ("unchecked-i128-arithmetic", 9),
                ("unchecked-i128-arithmetic", 10),
                ("unchecked-i128-arithmetic", 11),
 fix/audit-136-138-139
                ("unchecked-i128-arithmetic", 12),
                ("unchecked-i128-arithmetic", 13),
            ]
        );
    }

    // ---- #136: authorization parameter matching -------------------------

    #[test]
    fn require_auth_on_wrong_address_or_non_param_is_flagged() {
        let src = r#"
#[contractimpl]
impl Contract {
    pub fn wrong_local(env: Env, user: Address) {
        let admin = get_admin(&env);
        admin.require_auth();
    }

    pub fn wrong_field(&self, env: Env, user: Address) {
        self.admin.require_auth();
    }

    pub fn shadowed_param(env: Env, user: Address) {
        let user = get_admin(&env);
        user.require_auth();
    }
}
"#;
        assert_eq!(auth_findings(src), vec![4, 9, 13]);
    }

    #[test]
    fn require_auth_on_tracked_address_parameter_or_alias_passes() {
        let src = r#"
#[contractimpl]
impl Contract {
    pub fn direct(env: Env, user: Address) {
        user.require_auth();
    }

    pub fn via_alias(env: Env, from: Address, to: Address) {
        let signer = from.clone();
        signer.require_auth_for_args(());
    }

    pub fn via_ref(env: Env, owner: &Address) {
        (*owner).require_auth();
    }
}
"#;
        assert_eq!(auth_findings(src), Vec::<usize>::new());
            ]
        );
    }

    #[test]
    fn non_i128_aliases_and_references_are_not_flagged() {
        let src = r#"
fn safe_u128(a: u128) -> u128 {
    let b = a;
    let r = &b;
    *r + 1
}
"#;
        assert_eq!(
            rules_of(src, &[Rule::UncheckedI128Arithmetic]),
            Vec::<(&'static str, usize)>::new()
        );
    }

    // ---- #137: qualified Address parameter detection --------------------

    #[test]
    fn qualified_and_aliased_address_parameters_are_detected() {
        let src = r#"
use soroban_sdk::Address as AccountId;
type UserAddress = soroban_sdk::Address;
type ChainedAddress = UserAddress;

#[contractimpl]
impl Contract {
    pub fn pay_qualified(to: soroban_sdk::Address) {}
    pub fn pay_rooted(to: ::soroban_sdk::Address) {}
    pub fn pay_ref_qualified(to: &soroban_sdk::Address) {}
    pub fn pay_use_alias(to: AccountId) {}
    pub fn pay_ref_use_alias(to: &AccountId) {}
    pub fn pay_type_alias(to: UserAddress) {}
    pub fn pay_chained_alias(to: ChainedAddress) {}
}
"#;
        assert_eq!(auth_findings(src), vec![8, 9, 10, 11, 12, 13, 14]);
 main
    }
}
