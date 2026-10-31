//! Compatibility adapters for established verification runner interfaces.
//! An annotation describes intent; only recognized, status-preserving runner
//! invocations can become completion evidence. Unknown scripts remain unverified.
use crate::evidence::{ExecutionAnnotations, VerificationIntent, VerificationKind};
use serde_json::Value;

pub const PROHIBITED_EXECUTION_HINT: &str = "Verification is prohibited or deferred by the current task contract. Unknown commands, scripts, delegated execution and shell input are blocked. Use file tools or a simple read-only command; report changes as unverified.";

fn shell_command_segments(command: &str) -> Vec<&str> {
    let mut segments = Vec::new();
    let mut quote = None;
    let mut escaped = false;
    let mut start = 0;
    let mut chars = command.char_indices().peekable();
    while let Some((index, ch)) = chars.next() {
        if escaped {
            escaped = false;
            continue;
        }
        if quote != Some('\'') && ch == '\\' {
            escaped = true;
            continue;
        }
        if let Some(active_quote) = quote {
            if ch == active_quote {
                quote = None;
            }
            continue;
        }
        if ch == '\'' || ch == '"' {
            quote = Some(ch);
            continue;
        }
        if matches!(ch, ';' | '|' | '\n' | '\r')
            || (ch == '&' && chars.peek().is_some_and(|(_, next)| *next == '&'))
        {
            segments.push(&command[start..index]);
            let width = ch.len_utf8();
            let mut end = index + width;
            if matches!(ch, '|' | '&') && chars.peek().is_some_and(|(_, next)| *next == ch) {
                if let Some((next_index, next)) = chars.next() {
                    end = next_index + next.len_utf8();
                }
            }
            start = end;
        }
    }
    segments.push(&command[start..]);
    segments
}

fn shell_command_words(segment: &str) -> Vec<String> {
    let mut words = Vec::new();
    let mut word = String::new();
    let mut quote = None;
    let mut escaped = false;
    let mut active = false;
    for ch in segment.chars() {
        if escaped {
            word.push(ch);
            escaped = false;
            active = true;
            continue;
        }
        if quote != Some('\'') && ch == '\\' {
            escaped = true;
            active = true;
            continue;
        }
        if let Some(active_quote) = quote {
            if ch == active_quote {
                quote = None;
            } else {
                word.push(ch);
            }
            active = true;
            continue;
        }
        match ch {
            '\'' | '"' => {
                quote = Some(ch);
                active = true;
            }
            ch if ch.is_whitespace() => {
                if active {
                    words.push(std::mem::take(&mut word));
                    active = false;
                }
            }
            _ => {
                word.push(ch);
                active = true;
            }
        }
    }
    if escaped {
        word.push('\\');
    }
    if active {
        words.push(word);
    }
    words
}

fn is_environment_assignment(word: &str) -> bool {
    let Some((name, _)) = word.split_once('=') else {
        return false;
    };
    !name.is_empty()
        && name.chars().enumerate().all(|(index, ch)| {
            ch == '_' || ch.is_ascii_alphanumeric() && (index > 0 || !ch.is_ascii_digit())
        })
}

fn strip_verification_prefixes(mut words: Vec<String>) -> Vec<String> {
    loop {
        while words
            .first()
            .is_some_and(|word| is_environment_assignment(word))
        {
            words.remove(0);
        }
        match words
            .first()
            .map(|word| word.to_ascii_lowercase())
            .as_deref()
        {
            Some("timeout")
                if words.get(1).is_some_and(|duration| {
                    duration
                        .trim_end_matches(['s', 'm', 'h', 'd'])
                        .parse::<f64>()
                        .is_ok()
                }) =>
            {
                words.drain(..2);
            }
            Some("env" | "time") => {
                words.remove(0);
                if words.first().is_some_and(|word| word == "-p") {
                    words.remove(0);
                }
            }
            Some("uv" | "poetry")
                if words
                    .get(1)
                    .is_some_and(|subcommand| subcommand.eq_ignore_ascii_case("run")) =>
            {
                words.drain(..2);
            }
            _ => return words,
        }
    }
}

fn is_python_executable(command: &str) -> bool {
    command == "python"
        || command == "python3"
        || command.strip_prefix("python3.").is_some_and(|version| {
            !version.is_empty()
                && version
                    .chars()
                    .all(|character| character.is_ascii_digit() || character == '.')
        })
}

fn runner_kind(segment: &str) -> Option<VerificationKind> {
    use VerificationKind::*;
    let words = strip_verification_prefixes(shell_command_words(segment));
    let command = words.first()?.as_str();
    let args = &words[1..];
    // Non-check modes and failure-masking switches cannot produce evidence,
    // including their --flag=value spelling. Decline conservatively even when
    // a particular runner might ignore a flag or interpret it differently.
    if args.iter().any(|arg| {
        matches!(
            arg.split('=').next().unwrap_or(arg),
            "--version"
                | "--help"
                | "-h"
                | "--collect-only"
                | "--co"
                | "--setup-only"
                | "--notest"
                | "--list"
                | "--listTests"
                | "--list-tests"
                | "--showConfig"
                | "--show-config"
                | "--show-files"
                | "--listFilesOnly"
                | "--dry-run"
                | "--dryRun"
                | "--explain"
                | "--init"
                | "--if-present"
                | "--watch"
                | "--watchAll"
                | "--watch-all"
                | "--skip-tests"
                | "-DskipTests"
                | "-Dmaven.test.skip"
                | "-Dmaven.test.failure.ignore"
                | "--exit-zero"
                | "--exitZero"
                | "--suppress-nonzero-exit-code"
                | "--ignore-scripts"
                | "--fixtures"
                | "--fixtures-per-test"
                | "--listenvs"
                | "--show"
                | "--clean"
                | "--exclude-task"
                | "--just-print"
                | "--recon"
                | "--question"
                | "--touch"
                | "--ignore-errors"
                | "--fail-never"
        )
    }) {
        return None;
    }
    let action = args.first().map(String::as_str);
    match (command, action) {
        ("tox", _) if args.iter().any(|arg| matches!(arg.as_str(), "-l" | "-a")) => None,
        ("pytest" | "tox", _) => Some(Test),
        ("ruff", Some("check")) => Some(Lint),
        ("mypy" | "shellcheck", _) => Some(Lint),
        ("tsc", _) => Some(Build),
        (python, Some("-m")) if is_python_executable(python) => match args.get(1)?.as_str() {
            "pytest" | "unittest" => Some(Test),
            "mypy" => Some(Lint),
            "py_compile" | "compileall" => Some(Syntax),
            _ => None,
        },
        ("cargo", Some("test")) if args.iter().any(|arg| arg == "--no-run") => Some(Build),
        ("cargo", Some("test")) => Some(Test),
        ("cargo", Some("nextest"))
            if args.get(1).is_some_and(|arg| arg == "run")
                && !args.iter().any(|arg| arg == "--no-run") =>
        {
            Some(Test)
        }
        ("cargo", Some("check" | "build")) => Some(Build),
        ("cargo", Some("clippy")) => Some(Lint),
        ("go", Some("test"))
            if args
                .iter()
                .any(|arg| arg == "-list" || arg.starts_with("-list=")) =>
        {
            None
        }
        ("go", Some("test")) if args.iter().any(|arg| arg == "-c" || arg == "-c=true") => {
            Some(Build)
        }
        ("go", Some("test")) => Some(Test),
        ("go", Some("build")) => Some(Build),
        ("go", Some("vet")) => Some(Lint),
        ("npm" | "pnpm" | "yarn", _) => match if action == Some("run") {
            args.get(1).map(String::as_str)
        } else {
            action
        } {
            Some("test") => Some(Test),
            Some("build" | "check") => Some(Build),
            Some("lint") => Some(Lint),
            _ => None,
        },
        ("npx", Some("jest")) => Some(Test),
        ("npx", Some("vitest")) if args.get(1).is_some_and(|arg| arg == "run") => Some(Test),
        ("npx", Some("tsc")) => Some(Build),
        ("make", _)
            if args.iter().any(|arg| {
                arg.starts_with('-')
                    && !arg.starts_with("--")
                    && arg[1..]
                        .chars()
                        .any(|ch| matches!(ch, 'n' | 'q' | 't' | 'i'))
            }) =>
        {
            None
        }
        ("make", Some("test")) => Some(Test),
        ("make", Some("check" | "build")) => Some(Build),
        ("mvn", _) if args.iter().any(|arg| arg == "-fn") => None,
        ("mvn", Some("test" | "verify")) => Some(Test),
        ("gradle" | "./gradlew", _)
            if args.iter().any(|arg| arg == "-m" || arg.starts_with("-x")) =>
        {
            None
        }
        ("gradle" | "./gradlew", Some("test")) => Some(Test),
        ("gradle" | "./gradlew", Some("build")) => Some(Build),
        ("node", Some("--check" | "-c")) | ("bash" | "sh", Some("-n")) if args.get(1).is_some() => {
            Some(Syntax)
        }
        _ => None,
    }
}

/// Accept only simple commands joined by &&. Shell syntax is deliberately
/// restricted: parsing arbitrary shell is not proof of preserved exit status.
fn status_preserving_segments(command: &str) -> Option<Vec<&str>> {
    if command.trim().is_empty() || command.contains(['$', '`', '\n', '\r', '\\']) {
        return None;
    }
    let mut quote = None;
    let mut chars = command.chars().peekable();
    while let Some(ch) = chars.next() {
        if let Some(active) = quote {
            if ch == active {
                quote = None;
            }
            continue;
        }
        match ch {
            '\'' | '"' => quote = Some(ch),
            '&' if chars.next() == Some('&') => {}
            '&' | '|' | ';' | '>' | '<' | '(' | ')' | '{' | '}' | '#' | '!' => return None,
            _ => {}
        }
    }
    if quote.is_some() {
        return None;
    }
    let segments = shell_command_segments(command);
    if segments.iter().any(|segment| segment.trim().is_empty()) {
        return None;
    }
    Some(segments)
}

/// Eligible evidence, not merely a caller's declaration. An explicit kind must
/// agree with the runner. Unknown custom scripts cannot discharge the gate.
pub fn command_intent(params: &Value) -> Option<VerificationIntent> {
    let annotations = ExecutionAnnotations::parse(params).ok()?;
    let command = params
        .get("command")
        .or_else(|| params.get("cmd"))?
        .as_str()?;
    let segments = status_preserving_segments(command)?;
    let mut kind = None;
    for (index, segment) in segments.iter().enumerate() {
        let words = shell_command_words(segment);
        if index == 0 && segments.len() > 1 && words.first().is_some_and(|word| word == "cd") {
            // A directory change is the only non-check prefix allowed.
            if words.len() != 2 || words[1].starts_with('-') {
                return None;
            }
            continue;
        }
        let current = runner_kind(segment)?;
        if kind.is_some_and(|previous| previous != current) {
            return None;
        }
        kind = Some(current);
    }
    let kind = kind?;
    if let Some(intent) = annotations.verification {
        return (intent.kind == kind).then_some(intent);
    }
    Some(VerificationIntent {
        kind,
        target: command.to_owned(),
    })
}

/// Execution restrictions are separate from successful-check evidence. Unknown
/// commands fail closed under a forbidden/deferred contract; they are not
/// classified as ordinary work merely because no runner was recognized.
pub fn allowed_without_verification(params: &Value) -> bool {
    if params.get("verification").is_some() {
        return false;
    }
    let Some(command) = params
        .get("command")
        .or_else(|| params.get("cmd"))
        .and_then(Value::as_str)
    else {
        return false;
    };
    let Some(segments) = status_preserving_segments(command) else {
        return false;
    };
    segments.iter().all(|segment| {
        // No env/timeout/interpreter wrappers, assignments or arbitrary scripts.
        let words = shell_command_words(segment);
        matches!(
            words.first().map(String::as_str),
            Some("pwd" | "ls" | "cat" | "head" | "tail" | "wc" | "stat" | "readlink")
        )
    })
}

/// Apply a registered task prohibition to the actual tool/arguments. Script
/// executors and delegation fail closed even when they expose no command field.
pub fn violates_verification_prohibition(tool: &dyn crate::Tool, params: &Value) -> bool {
    use crate::evidence::CommandOperation;
    if params.get("verification").is_some() {
        return true;
    }
    match tool.command_operation(params) {
        CommandOperation::Launch => {
            !tool.supports_verification() || !allowed_without_verification(params)
        }
        CommandOperation::Input | CommandOperation::Opaque => true,
        CommandOperation::Poll | CommandOperation::Close => false,
        CommandOperation::None => false,
    }
}
