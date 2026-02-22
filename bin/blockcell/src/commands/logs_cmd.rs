use blockcell_core::Paths;

/// Show recent agent logs.
pub async fn show(lines: usize, session: Option<String>) -> anyhow::Result<()> {
    let paths = Paths::default();
    let logs_dir = paths.workspace().join("logs");

    if !logs_dir.exists() {
        println!("(No logs. Logs are generated automatically when the agent runs.)");
        return Ok(());
    }

    // Find log files, sorted by modification time (newest first)
    let mut log_files: Vec<std::path::PathBuf> = Vec::new();
    if let Ok(entries) = std::fs::read_dir(&logs_dir) {
        for entry in entries.flatten() {
            let path = entry.path();
            if path.extension().is_some_and(|e| e == "log" || e == "jsonl") {
                log_files.push(path);
            }
        }
    }

    if log_files.is_empty() {
        // Also check for a single log file
        let single_log = logs_dir.join("agent.log");
        if single_log.exists() {
            log_files.push(single_log);
        } else {
            println!("(No log files)");
            return Ok(());
        }
    }

    log_files.sort_by(|a, b| {
        let ta = std::fs::metadata(a).and_then(|m| m.modified()).ok();
        let tb = std::fs::metadata(b).and_then(|m| m.modified()).ok();
        tb.cmp(&ta)
    });

    // Read the most recent log file
    let log_file = &log_files[0];
    let content = std::fs::read_to_string(log_file)?;
    let all_lines: Vec<&str> = content.lines().collect();

    // Filter by session if specified
    let filtered: Vec<&&str> = if let Some(ref sess) = session {
        all_lines.iter().filter(|line| line.contains(sess.as_str())).collect()
    } else {
        all_lines.iter().collect()
    };

    let start = filtered.len().saturating_sub(lines);
    let tail = &filtered[start..];

    println!("📋 Logs: {} (last {} lines)", log_file.display(), tail.len());
    println!();

    for line in tail {
        println!("{}", line);
    }

    if log_files.len() > 1 {
        println!();
        println!("({} log files total, showing latest)", log_files.len());
    }

    Ok(())
}

/// Follow logs in real-time (tail -f style).
pub async fn follow(session: Option<String>) -> anyhow::Result<()> {
    let paths = Paths::default();
    let logs_dir = paths.workspace().join("logs");

    if !logs_dir.exists() {
        println!("(No logs directory. Start the agent first.)");
        return Ok(());
    }

    // Find the most recent log file
    let mut log_files: Vec<std::path::PathBuf> = Vec::new();
    if let Ok(entries) = std::fs::read_dir(&logs_dir) {
        for entry in entries.flatten() {
            let path = entry.path();
            if path.extension().is_some_and(|e| e == "log" || e == "jsonl") {
                log_files.push(path);
            }
        }
    }

    let single_log = logs_dir.join("agent.log");
    if log_files.is_empty() && single_log.exists() {
        log_files.push(single_log);
    }

    if log_files.is_empty() {
        println!("(No log files)");
        return Ok(());
    }

    log_files.sort_by(|a, b| {
        let ta = std::fs::metadata(a).and_then(|m| m.modified()).ok();
        let tb = std::fs::metadata(b).and_then(|m| m.modified()).ok();
        tb.cmp(&ta)
    });

    let log_file = &log_files[0];
    println!("📋 Following logs: {} (Ctrl+C to exit)", log_file.display());
    println!();

    // Simple tail -f implementation
    let mut last_size = std::fs::metadata(log_file).map(|m| m.len()).unwrap_or(0);

    // Print last 10 lines first
    let content = std::fs::read_to_string(log_file)?;
    let all_lines: Vec<&str> = content.lines().collect();
    let start = all_lines.len().saturating_sub(10);
    for line in &all_lines[start..] {
        if let Some(ref sess) = session {
            if line.contains(sess.as_str()) {
                println!("{}", line);
            }
        } else {
            println!("{}", line);
        }
    }

    loop {
        tokio::time::sleep(std::time::Duration::from_millis(500)).await;

        let current_size = std::fs::metadata(log_file).map(|m| m.len()).unwrap_or(0);
        if current_size > last_size {
            // Read new content
            let file = std::fs::File::open(log_file)?;
            use std::io::{Read, Seek, SeekFrom};
            let mut file = std::io::BufReader::new(file);
            file.seek(SeekFrom::Start(last_size))?;
            let mut new_content = String::new();
            file.read_to_string(&mut new_content)?;

            for line in new_content.lines() {
                if let Some(ref sess) = session {
                    if line.contains(sess.as_str()) {
                        println!("{}", line);
                    }
                } else {
                    println!("{}", line);
                }
            }

            last_size = current_size;
        }
    }
}

/// Clear log files.
pub async fn clear(force: bool) -> anyhow::Result<()> {
    let paths = Paths::default();
    let logs_dir = paths.workspace().join("logs");

    if !logs_dir.exists() {
        println!("(No logs)");
        return Ok(());
    }

    if !force {
        print!("⚠ Clear all logs? [y/N] ");
        use std::io::Write;
        std::io::stdout().flush()?;

        let mut input = String::new();
        std::io::stdin().read_line(&mut input)?;

        if !input.trim().eq_ignore_ascii_case("y") {
            println!("Cancelled.");
            return Ok(());
        }
    }

    let mut count = 0;
    if let Ok(entries) = std::fs::read_dir(&logs_dir) {
        for entry in entries.flatten() {
            let path = entry.path();
            if path.is_file() {
                let _ = std::fs::remove_file(&path);
                count += 1;
            }
        }
    }

    println!("✓ Cleared {} log file(s)", count);
    Ok(())
}
