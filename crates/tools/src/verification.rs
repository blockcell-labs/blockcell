//! Compatibility adapters for established verification runner interfaces.
//! Custom scripts use a typed verification annotation; names/source text are not evidence.
use crate::evidence::{ExecutionAnnotations, VerificationIntent, VerificationKind};
use serde_json::Value;

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
    if args.iter().any(|arg| {
        matches!(
            arg.as_str(),
            "--version" | "--help" | "-h" | "--collect-only"
        )
    }) {
        return None;
    }
    let action = args.first().map(String::as_str);
    match (command, action) {
        ("pytest" | "tox", _) => Some(Test),
        ("ruff" | "mypy" | "shellcheck", _) => Some(Lint),
        ("tsc", _) => Some(Build),
        (python, Some("-m")) if is_python_executable(python) => match args.get(1)?.as_str() {
            "pytest" | "unittest" => Some(Test),
            "mypy" => Some(Lint),
            "py_compile" | "compileall" => Some(Syntax),
            _ => None,
        },
        ("cargo", Some("test")) => Some(Test),
        ("cargo", Some("nextest")) if args.get(1).is_some_and(|arg| arg == "run") => Some(Test),
        ("cargo", Some("check" | "build")) => Some(Build),
        ("cargo", Some("clippy")) => Some(Lint),
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
        ("npx", Some("jest" | "vitest")) => Some(Test),
        ("npx", Some("tsc")) => Some(Build),
        ("make", Some("test")) => Some(Test),
        ("make", Some("check" | "build")) => Some(Build),
        ("mvn", Some("test" | "verify")) => Some(Test),
        ("gradle" | "./gradlew", Some("test")) => Some(Test),
        ("gradle" | "./gradlew", Some("build")) => Some(Build),
        ("node", Some("--check" | "-c")) | ("bash" | "sh", Some("-n")) if args.get(1).is_some() => {
            Some(Syntax)
        }
        _ => None,
    }
}

/// Describes the intended check; callers must separately verify completion and
/// its exit status. This function alone never proves that a check passed.
pub fn command_intent(params: &Value) -> Option<VerificationIntent> {
    let annotations = ExecutionAnnotations::parse(params).ok()?;
    if let Some(intent) = annotations.verification {
        return Some(intent);
    }
    let command = params
        .get("command")
        .or_else(|| params.get("cmd"))?
        .as_str()?;
    // Do not infer a check through shell programs, redirections, pipelines,
    // substitutions or status-masking constructs. Declare custom checks explicitly.
    if command.contains(['|', ';', '>', '<', '$', '`', '\n', '\r']) {
        return None;
    }
    if command.replace("&&", "").contains('&') {
        return None;
    }
    let segments = shell_command_segments(command);
    let mut kind = None;
    for (index, segment) in segments.iter().enumerate() {
        let words = strip_verification_prefixes(shell_command_words(segment));
        if index == 0 && segments.len() > 1 && words.first().is_some_and(|word| word == "cd") {
            continue;
        }
        let current = runner_kind(segment)?;
        // Mixed verification kinds are represented explicitly rather than silently
        // upgrading a build/syntax check to a test.
        if kind.is_some_and(|previous| previous != current) {
            return None;
        }
        kind = Some(current);
    }
    Some(VerificationIntent {
        kind: kind?,
        target: command.to_owned(),
    })
}
