use blockcell_core::{Config, Paths};
use std::io::{self, Write};

const AGENTS_MD: &str = r#"# Agent Guidelines

You are blockcell, a helpful AI assistant.

## Core Behaviors
- Be helpful, accurate, and concise
- Use tools when needed to accomplish tasks
- Ask for clarification when instructions are ambiguous
- Respect user privacy and security

## Tool Usage
- Use `read_file` to read file contents
- Use `write_file` to create or overwrite files
- Use `edit_file` for precise text replacements
- Use `exec` to run shell commands
- Use `web_search` and `web_fetch` for web information
"#;

const SOUL_MD: &str = r#"# Personality

I am blockcell, a thoughtful and capable AI assistant.

## Values
- Honesty and transparency
- Respect for user autonomy
- Continuous learning and improvement
- Security and privacy awareness

## Communication Style
- Clear and concise
- Professional yet friendly
- Proactive in offering help
- Patient with complex requests
"#;

const USER_MD: &str = r#"# User Preferences

<!-- Add your preferences here -->

## Language
- Preferred language: English

## Work Style
- Prefer concise responses
- Show code examples when helpful
"#;

const MEMORY_MD: &str = r#"# Long-term Memory

<!-- Important information to remember across sessions -->
"#;

const HEARTBEAT_MD: &str = r#"# Heartbeat Tasks

<!-- Add tasks here that should be checked periodically -->
<!-- Empty file or only comments = no action needed -->
"#;

const TOOLS_MD: &str = r#"# Available Tools

This document describes the tools available to the agent.

## File System Tools

### read_file
Read the contents of a file.
- **path**: Path to the file to read

### write_file
Write content to a file, creating parent directories if needed.
- **path**: Path to the file to write
- **content**: Content to write to the file

### edit_file
Edit a file by replacing old_text with new_text. old_text must match exactly and appear only once.
- **path**: Path to the file to edit
- **old_text**: Text to find and replace (must match exactly)
- **new_text**: Text to replace old_text with

### list_dir
List contents of a directory.
- **path**: Path to the directory to list

## Command Execution

### exec
Execute a shell command.
- **command**: The command to execute
- **working_dir**: Working directory for the command (optional)

**Safety**: Dangerous commands (rm -rf, dd, format, shutdown, etc.) are blocked.

## Web Tools

### web_search
Search the web using Brave Search API.
- **query**: Search query
- **count**: Number of results (1-10, default 5)

**Note**: Requires Brave Search API key in config.

### web_fetch
Fetch and extract content from a URL.
- **url**: URL to fetch (must be http or https)
- **extractMode**: Content extraction mode (markdown or text, default: markdown)
- **maxChars**: Maximum characters to return (default: 50000)

## Communication

### message
Send a message to a specific channel/chat.
- **content**: Message content
- **channel**: Target channel (optional)
- **chat_id**: Target chat ID (optional)

**Note**: Only use this for sending to specific channels. For normal conversation, respond directly.

### spawn
Start a background task (subagent).
- **task**: Task description
- **label**: Task label (optional)

**Note**: Subagents run in isolation and report back when complete.
"#;

pub async fn run(force: bool) -> anyhow::Result<()> {
    let paths = Paths::new();

    // Check if config exists
    if paths.config_file().exists() && !force {
        print!("Config already exists. Overwrite? [y/N] ");
        io::stdout().flush()?;
        
        let mut input = String::new();
        io::stdin().read_line(&mut input)?;
        
        if !input.trim().eq_ignore_ascii_case("y") {
            println!("Aborted.");
            return Ok(());
        }
    }

    // Create directories
    paths.ensure_dirs()?;

    // Create default config
    let config = Config::default();
    config.save(&paths.config_file())?;
    println!("✓ Created config: {}", paths.config_file().display());

    // Create workspace files
    write_if_not_exists(&paths.agents_md(), AGENTS_MD)?;
    write_if_not_exists(&paths.soul_md(), SOUL_MD)?;
    write_if_not_exists(&paths.user_md(), USER_MD)?;
    write_if_not_exists(&paths.tools_md(), TOOLS_MD)?;
    write_if_not_exists(&paths.memory_md(), MEMORY_MD)?;
    write_if_not_exists(&paths.heartbeat_md(), HEARTBEAT_MD)?;

    println!("✓ Created workspace: {}", paths.workspace().display());
    println!();
    println!("Next steps:");
    println!("  1. Edit {} to add your API keys", paths.config_file().display());
    println!("  2. Run `blockcell status` to verify configuration");
    println!("  3. Run `blockcell agent` to start chatting");

    Ok(())
}

fn write_if_not_exists(path: &std::path::Path, content: &str) -> io::Result<()> {
    if !path.exists() {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        std::fs::write(path, content)?;
        println!("  ✓ Created {}", path.file_name().unwrap().to_string_lossy());
    }
    Ok(())
}
