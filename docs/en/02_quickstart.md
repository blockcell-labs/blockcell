# Article 02: Get Started with blockcell in 5 Minutes — From Installation to Your First Chat

> Series: *In-Depth Analysis of the Open Source Project “blockcell”* — 2/14

---

## Preface

In the previous article, we introduced what blockcell is. In this one, we’ll get hands-on and have it running within 5 minutes.

**What you need:**
- A macOS or Linux computer (Windows is also supported; this article uses macOS as the example)
- An LLM API key (OpenAI, DeepSeek, Kimi are all fine — we’ll cover how to choose)

---

## The fastest 5-minute path (follow these steps)

If you only want to run it once as quickly as possible, do these 5 steps:

1. Install: run the installer script
2. Initialize: `blockcell onboard`
3. Configure: edit `~/.blockcell/config.json` and set an available provider (DeepSeek or Kimi recommended)
4. Check: `blockcell status` and make sure everything is ✓
5. Start: `blockcell agent` and send a message to test

The rest of this article goes deeper (multi-provider options, common commands, FAQ, deployment suggestions). After you get the first run working, you can read those parts slowly.

---

## Step 1: Install

### Option A: One-line install script (recommended)

```bash
curl -fsSL https://raw.githubusercontent.com/blockcell-labs/blockcell/refs/heads/main/install.sh | sh
```

After installation, the `blockcell` command will be available under `~/.local/bin/`. If your shell can’t find it, add that path to your `PATH`:

```bash
echo 'export PATH="$HOME/.local/bin:$PATH"' >> ~/.zshrc
source ~/.zshrc
```

### Option B: Build from source

If you want to compile it yourself (requires Rust 1.75+):

```bash
git clone https://github.com/blockcell-labs/blockcell.git
cd blockcell/blockcell
cargo build --release
cp target/release/blockcell ~/.local/bin/
```

### Verify installation

```bash
blockcell --version
# blockcell 0.x.x
```

---

## Step 2: Initialize

```bash
blockcell onboard
```

This command will:
1. Create the `~/.blockcell/` directory structure
2. Generate the default config file `~/.blockcell/config.json`
3. Print a short onboarding guide

The directory structure looks like this:

```
~/.blockcell/
├── config.json          # Main config
└── workspace/           # The AI’s working directory
    ├── memory/          # Memory database
    ├── sessions/        # Session history
    ├── skills/          # User-installed skills
    ├── media/           # Screenshots, audio, etc.
    └── audit/           # Operation audit logs
```

---

## Step 3: Configure your API key

Open the config file:

```bash
# macOS
open ~/.blockcell/config.json

# Or use a terminal editor
nano ~/.blockcell/config.json
```

Find the `providers` section and fill in your API key.

### Option A: DeepSeek (cheap; recommended for beginners)

DeepSeek’s API is very inexpensive and great for testing:

```json
{
  "providers": {
    "deepseek": {
      "apiKey": "sk-your-deepseek-key",
      "apiBase": "https://api.deepseek.com/v1"
    }
  },
  "agents": {
    "defaults": {
      "model": "deepseek-chat"
    }
  }
}
```

### Option B: Kimi/Moonshot (stable access in China)

```json
{
  "providers": {
    "kimi": {
      "apiKey": "sk-your-kimi-key",
      "apiBase": "https://api.moonshot.cn/v1"
    }
  },
  "agents": {
    "defaults": {
      "model": "kimi/moonshot-v1-8k"
    }
  }
}
```

### Option C: OpenRouter (one key for many models)

```json
{
  "providers": {
    "openrouter": {
      "apiKey": "sk-or-your-openrouter-key",
      "apiBase": "https://openrouter.ai/api/v1"
    }
  },
  "agents": {
    "defaults": {
      "model": "anthropic/claude-sonnet-4-20250514"
    }
  }
}
```

### Option D: Ollama (fully local, free)

If you already have Ollama installed and pulled a model:

```json
{
  "providers": {
    "ollama": {
      "apiBase": "http://localhost:11434"
    }
  },
  "agents": {
    "defaults": {
      "model": "ollama/llama3"
    }
  }
}
```

---

## Step 4: Check status

```bash
blockcell status
```

Example output:

```
✓ Config loaded
✓ Provider: deepseek (deepseek-chat)
✓ Workspace: ~/.blockcell/workspace
✓ Memory: SQLite (0 items)
✓ Skills: 0 user skills, 44 builtin skills
✓ Channels: none configured
```

If you see a red ✗, the configuration has an issue — adjust it according to the hints.

---

## Step 5: Start chatting

```bash
blockcell agent
```

You’ll see a welcome screen:

```
╔══════════════════════════════════════╗
║         blockcell agent              ║
║  Type /tasks to see background tasks ║
║  Type /quit to exit                  ║
╚══════════════════════════════════════╝

You:
```

Now you can start chatting.

---

## Try these commands

### Basic chat

```
You: Hi, introduce yourself
```

### Ask the AI to search the web

```
You: Search for today’s AI-related news
```

The AI will call `web_search` and then use `web_fetch` to retrieve content.

### Read local files

```
You: Read ~/Desktop/report.txt and summarize the key points
```

> ⚠️ Note: when reading files outside the working directory (`~/.blockcell/workspace`), blockcell will prompt for confirmation and you must type `y`. This is a safety mechanism.

### Run commands

```
You: Show me the files in the current directory
```

The AI will call the `exec` tool to run `ls`.

### Write a file

```
You: Create hello.txt in the working directory with content "Hello from blockcell"
```

---

## Common CLI commands

Besides interactive `agent` mode, blockcell provides many useful commands:

```bash
# List all available tools
blockcell tools

# View/manage memory
blockcell memory list
blockcell memory search "stock"

# View/manage skills
blockcell skills list

# List scheduled tasks
blockcell cron list

# Check channel status
blockcell channels status

# View evolution records
blockcell evolve list

# View alert rules
blockcell alerts list

# View real-time streams
blockcell streams list

# View knowledge graph
blockcell knowledge stats

# View logs
blockcell logs

# Self diagnostics
blockcell doctor
```

---

## Full config field overview

Key fields in `~/.blockcell/config.json`:

```json
{
  "providers": {
    "openai": {
      "apiKey": "sk-...",
      "apiBase": "https://api.openai.com/v1"
    }
  },
  "agents": {
    "defaults": {
      "model": "gpt-4o",
      "maxTokens": 4096,
      "temperature": 0.7
    }
  },
  "tools": {
    "tickIntervalSecs": 30
  },
  "gateway": {
    "host": "0.0.0.0",
    "port": 18790,
    "webuiPort": 18791,
    "apiToken": "optional access token"
  },
  "channels": {
    "telegram": {
      "botToken": "your bot token",
      "allowFrom": ["your user id"]
    }
  }
}
```

---

## Running into issues?

### Issue 1: Command not found

```bash
which blockcell
# If there’s no output, your PATH is not configured correctly
echo 'export PATH="$HOME/.local/bin:$PATH"' >> ~/.zshrc && source ~/.zshrc
```

### Issue 2: API calls fail

```bash
blockcell doctor
# Checks network connectivity and API configuration
```

### Issue 3: Want to switch models

Edit `agents.defaults.model` in `config.json`, then restart `blockcell agent`.

### Issue 4: Want to see which tools the AI used

In the chat, blockcell shows the tool name and parameters for each tool call. For more detailed logs:

```bash
blockcell logs --tail 50
```

---

## Summary

At this point you have:
- ✅ Installed blockcell
- ✅ Configured an API key
- ✅ Started your first chat
- ✅ Learned the basic commands

Next, we’ll dive into blockcell’s tool system — the heart of making AI actually “do work” with 50+ built-in tools.

---

*Previous: [What is blockcell? A self-evolving AI agent framework](./01_what_is_blockcell.md)*
*Next: [blockcell’s tool system — enabling AI to really execute tasks](./03_tools_system.md)*

*Repo: https://github.com/blockcell-labs/blockcell*
*Website: https://blockcell.dev*
