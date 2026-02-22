# blockcell Technical Article Series: “A Self-Evolving AI Agent Framework”

> An in-depth guide to blockcell for everyday developers and beginners. The main series contains 16 articles (this page is the index).

---

## Series overview

blockcell is an open-source AI agent framework written in **Rust**. It is not just a chatbot — it’s an AI workbench that can execute real tasks, maintain persistent memory, and evolve itself over time.

This series starts from zero and walks you through each core capability of blockcell.

The name blockcell comes from **Block + Cell** (a modular foundation + evolvable cells). For the full story, see:

*Appendix: [Name origin](./14_name_origin.md)*

---

## Table of contents

| # | Title | Key topics | Best for |
|----|------|-----------|---------|
| 01 | [What is blockcell?](./01_what_is_blockcell.md) | Overview, core features, differences vs ChatGPT | Everyone |
| 02 | [5-minute quickstart](./02_quickstart.md) | Install, configure API key, first chat | Beginners |
| 03 | [Tool system](./03_tools_system.md) | 50+ built-in tools: files/web/finance/media | Beginners |
| 04 | [Skill system](./04_skill_system.md) | Rhai scripts, SKILL.md, custom skills | Beginners |
| 05 | [Memory system](./05_memory_system.md) | SQLite + FTS5, persistent memory, auto injection | Intermediate |
| 06 | [Multi-channel access](./06_channels.md) | Telegram/Slack/Discord/Feishu configuration | Intermediate |
| 07 | [Browser automation](./07_browser_automation.md) | CDP, accessibility tree, 35+ actions | Intermediate |
| 08 | [Gateway mode](./08_gateway_mode.md) | HTTP API, WebSocket, server deployment | Intermediate |
| 09 | [Self-evolution](./09_self_evolution.md) | error-triggered evolution, code generation, canary rollout | Advanced |
| 10 | [Finance in practice](./10_finance_use_case.md) | CN stocks & crypto monitoring, alerts, daily reports | Hands-on |
| 11 | [Subagents and task concurrency](./11_subagents.md) | spawn, TaskManager, non-blocking chat | Advanced |
| 12 | [Architecture deep dive](./12_architecture.md) | why Rust, crate layout, design patterns | Advanced |
| 13 | [Message processing & evolution lifecycle](./13_message_processing_and_evolution.md) | end-to-end flow from message to evolution | Advanced |
| 14 | [Name origin](./14_name_origin.md) | the meaning and story of Block + Cell | Appendix |
| 15 | [Ghost Agent](./15_ghost_agent.md) | background maintenance, memory gardening, hub sync | Advanced |
| 16 | [Agent2Agent Community (Blockcell Hub)](./16_hub_community.md) | autonomous A2A vision: skills flow, node discovery, agent communication (roadmap) | Advanced |

---

## Recommended reading order

**If you are brand new:** 01 → 02 → 03 → 04

**If you want to deploy quickly:** 01 → 02 → 06 → 08

**If you care about finance:** 01 → 02 → 03 → 10

**If you want to contribute code:** 01 → 12 → 04 → 09

---

## Project info

- **GitHub**: https://github.com/blockcell-labs/blockcell
- **Website**: https://blockcell.dev
- **License**: MIT

---

*This series is written against the real blockcell codebase, and all examples are runnable.*
