use super::*;

impl SkillEvolution {
    pub(crate) fn build_generation_prompt(&self, context: &EvolutionContext) -> Result<String> {
        match context.layout {
            SkillLayout::PromptTool => return self.build_prompt_only_generation_prompt(context),
            SkillLayout::LocalScript => return self.build_local_script_generation_prompt(context),
            SkillLayout::Hybrid => return self.build_hybrid_generation_prompt(context),
            SkillLayout::RhaiOrchestration => {}
        }

        let has_existing_source = context.source_snippet.is_some();
        let is_manual = matches!(context.trigger, TriggerReason::ManualRequest { .. });

        let mut prompt = String::new();

        // System context: Rhai language
        prompt.push_str(
            "You are a Rhai skill evolution assistant for the blockcell agent framework.\n",
        );
        prompt
            .push_str("All skills MUST be written in the Rhai scripting language (.rhai files).\n");
        prompt
            .push_str("Do NOT generate JavaScript, Python, TypeScript, or any other language.\n\n");

        prompt.push_str("## Rhai Language Quick Reference\n");
        prompt.push_str("- Variables: `let x = 42;` (immutable by default), `let x = 42; x = 100;` (reassign ok)\n");
        prompt.push_str("- Strings: `let s = \"hello\";` with interpolation `\"value: ${x}\"`\n");
        prompt.push_str("- Arrays: `let a = [1, 2, 3];` Maps: `let m = #{x: 1, y: 2};`\n");
        prompt.push_str("- Functions: `fn add(a, b) { a + b }`\n");
        prompt.push_str(
            "- Control: `if x > 0 { } else { }`, `for i in 0..10 { }`, `while x > 0 { }`\n",
        );
        prompt.push_str("- String methods: `.len()`, `.contains()`, `.split()`, `.trim()`, `.to_upper()`, `.to_lower()`\n");
        prompt.push_str("- Array methods: `.push()`, `.pop()`, `.len()`, `.filter()`, `.map()`\n");
        prompt.push_str("- Built-in helpers: `len(value)`, `str_sub(text, start, len)`, `str_truncate(text, max_chars)`, `str_lines(text, max_lines)`, `arr_join(items, sep)`\n");
        prompt.push_str("- No classes/structs — use maps (object maps) `#{}` instead\n");
        prompt.push_str("- No `import`/`require` — all capabilities come from the host engine\n");
        prompt.push_str("- Print: `print(\"msg\");`\n\n");

        prompt.push_str("## Stable Built-in Helper Functions\n");
        prompt.push_str(
            "- `len(value)` -> length of string / array / map, returns 0 for null-like values\n",
        );
        prompt.push_str("- `str_sub(text, start, len)` -> safe substring by character index\n");
        prompt.push_str(
            "- `str_truncate(text, max_chars)` -> truncate text safely at character boundary\n",
        );
        prompt.push_str("- `str_lines(text, max_lines)` -> return the first N lines as an array\n");
        prompt.push_str("- `arr_join(items, sep)` -> join array items into a string\n\n");

        // Enriched context: project rules, SKILL.md, evolution history, adjacent skills
        let enriched = self.gather_evolution_context(context);
        prompt.push_str(&self.format_enriched_context(&enriched));

        // Task description
        if is_manual {
            if let TriggerReason::ManualRequest { ref description } = context.trigger {
                if Self::is_openclaw_import_description(description) {
                    prompt.push_str(&format!("## Task\n{}\n\n", description));
                } else {
                    prompt.push_str(&format!(
                        "## Task\nCreate or improve a Blockcell Rhai skill for: {}\n\n",
                        description
                    ));
                }
            }
        } else {
            prompt.push_str(&format!(
                "## Task\nFix the following issue in the existing Blockcell Rhai skill '{}'. Preserve the skill's purpose and only change what is necessary to correct the problem.\n\n",
                context.skill_name
            ));
            prompt.push_str(&format!("Trigger: {:?}\n\n", context.trigger));
        }

        if let Some(error) = &context.error_stack {
            prompt.push_str(&format!(
                "## Error\n```\n{}\n```\n\n",
                Self::sanitize_for_code_fence(error)
            ));
        }

        // Existing source code
        if let Some(snippet) = &context.source_snippet {
            prompt.push_str(&format!(
                "## Current SKILL.rhai Source\n```rhai\n{}\n```\n\n",
                Self::sanitize_for_code_fence(snippet)
            ));
        }

        if !context.tool_schemas.is_empty() {
            prompt.push_str("## Available Host Tools\n");
            for tool in &context.tool_schemas {
                prompt.push_str(&format!("- {}\n", tool));
            }
            prompt.push('\n');
        }

        // Output format — P0-2: always request complete script (never diff)
        prompt.push_str("## Output Format\n");
        prompt.push_str("Generate the COMPLETE SKILL.rhai file content.\n");
        prompt.push_str("When the skill returns structured results, prefer returning `display_text` for final user-facing text. If the result still needs runtime/LLM polishing, return `summary_data` as a lightweight structured summary and keep large raw content out of `summary_data`.\n");
        prompt.push_str(
            "If you output `meta.yaml`, it must follow the minimal meta rules below.\n\n",
        );
        prompt.push_str(Self::trigger_rules_prompt());
        prompt.push_str("Output ONLY the Rhai code in a ```rhai code block.\n");
        prompt.push_str(
            "The script must be a valid, self-contained Rhai script with no syntax errors.\n",
        );
        let _ = has_existing_source; // suppress unused warning

        Ok(prompt)
    }

    pub(crate) fn build_prompt_only_generation_prompt(
        &self,
        context: &EvolutionContext,
    ) -> Result<String> {
        let is_manual = matches!(context.trigger, TriggerReason::ManualRequest { .. });
        let mut prompt = String::new();

        prompt.push_str("You are a skill document writer for the blockcell agent framework.\n");
        prompt.push_str(
            "Your task is to write or improve a SKILL.md file — a prompt instruction document\n",
        );
        prompt.push_str(
            "that tells the AI agent how to handle specific user requests for this skill.\n\n",
        );

        prompt.push_str("## What is SKILL.md?\n");
        prompt.push_str("SKILL.md is an operation manual injected into the agent's system prompt when this skill is triggered.\n");
        prompt.push_str("It should contain:\n");
        prompt.push_str("- **Goal**: What the skill does and when it applies\n");
        prompt.push_str("- **Tools to use**: Which built-in tools to call and in what order\n");
        prompt.push_str("- **Output format**: What the final response should look like\n");
        prompt
            .push_str("- **Scenarios**: 2-4 concrete usage scenarios with step-by-step guidance\n");
        prompt.push_str("- **Fallback strategy**: What to do when tools fail\n\n");

        // Enriched context: project rules, evolution history, adjacent skills
        let enriched = self.gather_evolution_context(context);
        prompt.push_str(&self.format_enriched_context(&enriched));

        if is_manual {
            if let TriggerReason::ManualRequest { ref description } = context.trigger {
                if Self::is_openclaw_import_description(description) {
                    prompt.push_str(&format!("## Task\n{}\n\n", description));
                } else {
                    prompt.push_str(&format!(
                        "## Task\nCreate or improve a Blockcell SKILL.md for: {}\n\n",
                        description
                    ));
                }
            }
        } else {
            prompt.push_str(&format!(
                "## Task\nFix the existing Blockcell SKILL.md for skill '{}' to address the following issue. Preserve the skill's original scope and intent; only tighten or correct the instructions as needed.\n\n",
                context.skill_name
            ));
            if let Some(error) = &context.error_stack {
                prompt.push_str(&format!(
                    "## Issue\n```\n{}\n```\n\n",
                    Self::sanitize_for_code_fence(error)
                ));
            }
        }

        if let Some(snippet) = &context.source_snippet {
            prompt.push_str(&format!(
                "## Current SKILL.md Content\n```markdown\n{}\n```\n\n",
                Self::sanitize_for_code_fence(snippet)
            ));
        }

        if context.staged {
            if let Some(ref staging_dir) = context.staging_skills_dir {
                let staged_root = std::path::PathBuf::from(staging_dir);
                let staged_skill_dir = staged_root.join(&context.skill_name);
                let staged_md = staged_skill_dir.join("SKILL.md");
                if let Ok(md) = std::fs::read_to_string(&staged_md) {
                    if !md.trim().is_empty() {
                        prompt.push_str("## Current Staged SKILL.md (reference)\n");
                        prompt.push_str(&format!(
                            "```markdown\n{}\n```\n\n",
                            Self::sanitize_for_code_fence(&md)
                        ));
                    }
                }
                let staged_meta = staged_skill_dir.join("meta.yaml");
                if let Ok(meta) = std::fs::read_to_string(&staged_meta) {
                    if !meta.trim().is_empty() {
                        prompt.push_str("## Current Staged meta.yaml (reference)\n");
                        prompt.push_str(&format!(
                            "```yaml\n{}\n```\n\n",
                            Self::sanitize_for_code_fence(&meta)
                        ));
                    }
                }
            }
        }

        prompt.push_str("## Result Contract\n");
        prompt.push_str("If the skill can directly produce final user-facing text, return `display_text`. Otherwise return `summary_data` as a lightweight structured summary for runtime/LLM polishing. Do NOT place complete webpages, long markdown, or large raw logs into `summary_data`.\n\n");
        prompt.push_str("## Output Format\n");
        prompt.push_str("Generate the COMPLETE SKILL.md content.\n");
        prompt.push_str("Output the markdown content in a ```markdown code block.\n");
        prompt.push_str("Also output an updated meta.yaml in a ```yaml code block.\n");
        prompt.push_str(Self::trigger_rules_prompt());
        prompt.push_str(&format!(
            "The document must be at least {} characters, practical, and clearly structured.\n",
            crate::evolution::MIN_PROMPT_TOOL_CHARS
        ));

        Ok(prompt)
    }

    pub(crate) fn build_hybrid_generation_prompt(
        &self,
        context: &EvolutionContext,
    ) -> Result<String> {
        let mut prompt = String::new();
        prompt.push_str(
            "You are a hybrid skill evolution assistant for the blockcell agent framework.\n",
        );
        prompt.push_str("This skill combines SKILL.md instructions with local script assets. Keep the manual, entrypoint, and fallback behavior aligned.\n\n");
        self.append_hybrid_contract_notes(&mut prompt, context);

        let body = match context.skill_type {
            SkillType::Python => self.build_python_generation_prompt(context)?,
            SkillType::LocalScript => self.build_local_script_generation_prompt(context)?,
            _ => self.build_prompt_only_generation_prompt(context)?,
        };

        prompt.push_str(&body);
        Ok(prompt)
    }

    pub(crate) fn build_fix_prompt(
        &self,
        context: &EvolutionContext,
        current_feedback: &FeedbackEntry,
        history: &[FeedbackEntry],
    ) -> Result<String> {
        match context.layout {
            SkillLayout::PromptTool => {
                return self.build_prompt_only_fix_prompt(context, current_feedback, history)
            }
            SkillLayout::LocalScript => {
                return self.build_local_script_fix_prompt(context, current_feedback, history)
            }
            SkillLayout::Hybrid => {
                return self.build_hybrid_fix_prompt(context, current_feedback, history)
            }
            SkillLayout::RhaiOrchestration => {}
        }

        let is_manual = matches!(context.trigger, TriggerReason::ManualRequest { .. });

        let mut prompt = String::new();

        // System context
        prompt.push_str(
            "You are a Rhai skill evolution assistant for the blockcell agent framework.\n",
        );
        prompt
            .push_str("All skills MUST be written in the Rhai scripting language (.rhai files).\n");
        prompt
            .push_str("Do NOT generate JavaScript, Python, TypeScript, or any other language.\n\n");

        prompt.push_str("## Rhai Language Quick Reference\n");
        prompt.push_str("- Variables: `let x = 42;` (immutable by default), `let x = 42; x = 100;` (reassign ok)\n");
        prompt.push_str("- Strings: `let s = \"hello\";` with interpolation `\"value: ${x}\"`\n");
        prompt.push_str("- Arrays: `let a = [1, 2, 3];` Maps: `let m = #{x: 1, y: 2};`\n");
        prompt.push_str("- Functions: `fn add(a, b) { a + b }`\n");
        prompt.push_str(
            "- Control: `if x > 0 { } else { }`, `for i in 0..10 { }`, `while x > 0 { }`\n",
        );
        prompt.push_str("- String methods: `.len()`, `.contains()`, `.split()`, `.trim()`, `.to_upper()`, `.to_lower()`\n");
        prompt.push_str("- Array methods: `.push()`, `.pop()`, `.len()`, `.filter()`, `.map()`\n");
        prompt.push_str("- Built-in helpers: `len(value)`, `str_sub(text, start, len)`, `str_truncate(text, max_chars)`, `str_lines(text, max_lines)`, `arr_join(items, sep)`\n");
        prompt.push_str(
            "- Map access: `m.key` or `m[\"key\"]`, check existence with `\"key\" in m`\n",
        );
        prompt
            .push_str("- Null coalescing: `value ?? default` (use instead of .get with default)\n");
        prompt.push_str("- Type conversion: `.to_string()`, `.to_int()`, `.to_float()`\n");
        prompt.push_str("- String concat: use `+` only between strings, convert numbers with `.to_string()` first\n");
        prompt.push_str("- No classes/structs — use maps (object maps) `#{}` instead\n");
        prompt.push_str("- No `import`/`require` — all capabilities come from the host engine\n");
        prompt.push_str("- Print: `print(\"msg\");`\n\n");

        prompt.push_str("## Stable Built-in Helper Functions\n");
        prompt.push_str(
            "- `len(value)` -> length of string / array / map, returns 0 for null-like values\n",
        );
        prompt.push_str("- `str_sub(text, start, len)` -> safe substring by character index\n");
        prompt.push_str(
            "- `str_truncate(text, max_chars)` -> truncate text safely at character boundary\n",
        );
        prompt.push_str("- `str_lines(text, max_lines)` -> return the first N lines as an array\n");
        prompt.push_str("- `arr_join(items, sep)` -> join array items into a string\n\n");

        // Enriched context: project rules, SKILL.md, evolution history, adjacent skills
        let enriched = self.gather_evolution_context(context);
        prompt.push_str(&self.format_enriched_context(&enriched));

        // Task description
        if is_manual {
            if let TriggerReason::ManualRequest { ref description } = context.trigger {
                if Self::is_openclaw_import_description(description) {
                    prompt.push_str(&format!("## Original Task\n{}\n\n", description));
                } else {
                    prompt.push_str(&format!(
                        "## Original Task\nCreate or improve a Blockcell Rhai skill for: {}\n\n",
                        description
                    ));
                }
            }
        } else {
            prompt.push_str(&format!(
                "## Original Task\nFix the following issue in the existing Blockcell Rhai skill '{}'. Keep behavior changes minimal and targeted.\n\n",
                context.skill_name
            ));
        }

        // Previous code that had issues
        prompt.push_str("## Previous Code (has issues)\n");
        prompt.push_str(&format!(
            "```rhai\n{}\n```\n\n",
            Self::sanitize_for_code_fence(&current_feedback.previous_code)
        ));

        // Current feedback
        prompt.push_str(&format!("## Issues Found ({})\n", current_feedback.stage));
        prompt.push_str(&format!("{}\n\n", current_feedback.feedback));

        // Show history of previous attempts if any (excluding current)
        let prev_attempts: Vec<&FeedbackEntry> = history
            .iter()
            .filter(|h| h.attempt < current_feedback.attempt)
            .collect();
        if !prev_attempts.is_empty() {
            prompt.push_str("## Previous Attempt History\n");
            prompt.push_str("The following issues were found in earlier attempts. Make sure NOT to repeat them:\n\n");
            for entry in prev_attempts {
                prompt.push_str(&format!(
                    "### Attempt #{} ({} failure)\n",
                    entry.attempt, entry.stage
                ));
                prompt.push_str(&format!("{}\n\n", entry.feedback));
            }
        }

        // Output format
        prompt.push_str("## Instructions\n");
        prompt.push_str(
            "Fix ALL the issues listed above and generate the COMPLETE corrected Rhai script.\n",
        );
        prompt.push_str("If the skill can directly produce final user-facing text, return `display_text`. Otherwise return `summary_data` as a lightweight structured summary for runtime/LLM polishing. Do NOT place complete webpages, long markdown, or large raw logs into `summary_data`.\n");
        prompt.push_str(
            "If you output `meta.yaml`, it must follow the minimal meta rules below.\n\n",
        );
        prompt.push_str(Self::trigger_rules_prompt());
        prompt.push_str("Do NOT leave any of the reported issues unfixed.\n");
        prompt.push_str("Output ONLY the corrected Rhai code in a ```rhai code block.\n");
        prompt.push_str(
            "The script must be a valid, self-contained Rhai script with no syntax errors.\n",
        );

        Ok(prompt)
    }

    pub(crate) fn build_prompt_only_fix_prompt(
        &self,
        context: &EvolutionContext,
        current_feedback: &FeedbackEntry,
        history: &[FeedbackEntry],
    ) -> Result<String> {
        let is_manual = matches!(context.trigger, TriggerReason::ManualRequest { .. });
        let mut prompt = String::new();

        prompt.push_str("You are a skill document writer for the blockcell agent framework.\n");
        prompt
            .push_str("Your task is to fix issues in a SKILL.md prompt instruction document.\n\n");

        // Enriched context: project rules, evolution history, adjacent skills
        let enriched = self.gather_evolution_context(context);
        prompt.push_str(&self.format_enriched_context(&enriched));

        if is_manual {
            if let TriggerReason::ManualRequest { ref description } = context.trigger {
                if Self::is_openclaw_import_description(description) {
                    prompt.push_str(&format!("## Original Task\n{}\n\n", description));
                } else {
                    prompt.push_str(&format!(
                        "## Original Task\nCreate or improve a Blockcell SKILL.md for: {}\n\n",
                        description
                    ));
                }
            }
        } else {
            prompt.push_str(&format!(
                "## Original Task\nFix the existing Blockcell SKILL.md for skill '{}'. Keep the same skill scope and repair only the broken or unclear parts.\n\n",
                context.skill_name
            ));
        }

        prompt.push_str("## Previous Content (has issues)\n");
        prompt.push_str(&format!(
            "```markdown\n{}\n```\n\n",
            Self::sanitize_for_code_fence(&current_feedback.previous_code)
        ));

        prompt.push_str(&format!("## Issues Found ({})\n", current_feedback.stage));
        prompt.push_str(&format!("{}\n\n", current_feedback.feedback));

        let prev_attempts: Vec<&FeedbackEntry> = history
            .iter()
            .filter(|h| h.attempt < current_feedback.attempt)
            .collect();
        if !prev_attempts.is_empty() {
            prompt.push_str("## Previous Attempt History\n");
            for entry in prev_attempts {
                prompt.push_str(&format!(
                    "### Attempt #{} ({} failure)\n{}\n\n",
                    entry.attempt, entry.stage, entry.feedback
                ));
            }
        }

        prompt.push_str("## Instructions\n");
        prompt.push_str("Fix ALL the issues listed above and generate the COMPLETE corrected SKILL.md content.\n");
        prompt.push_str("Output the markdown content in a ```markdown code block.\n");
        prompt.push_str("Also output an updated meta.yaml in a ```yaml code block.\n");
        prompt.push_str(Self::trigger_rules_prompt());

        Ok(prompt)
    }

    pub(crate) fn build_hybrid_fix_prompt(
        &self,
        context: &EvolutionContext,
        current_feedback: &FeedbackEntry,
        history: &[FeedbackEntry],
    ) -> Result<String> {
        let mut prompt = String::new();
        prompt.push_str(
            "You are a hybrid skill evolution assistant for the blockcell agent framework.\n",
        );
        prompt.push_str("This skill combines SKILL.md instructions with local script assets. Keep the prompt contract and the executable entrypoint consistent.\n\n");
        self.append_hybrid_contract_notes(&mut prompt, context);

        let body = match context.skill_type {
            SkillType::Python => {
                self.build_python_fix_prompt(context, current_feedback, history)?
            }
            SkillType::LocalScript => {
                self.build_local_script_fix_prompt(context, current_feedback, history)?
            }
            _ => self.build_prompt_only_fix_prompt(context, current_feedback, history)?,
        };

        prompt.push_str(&body);
        Ok(prompt)
    }

    pub(crate) fn build_local_script_generation_prompt(
        &self,
        context: &EvolutionContext,
    ) -> Result<String> {
        let is_manual = matches!(context.trigger, TriggerReason::ManualRequest { .. });
        let mut prompt = String::new();

        prompt.push_str(
            "You are a local script and CLI skill developer for the blockcell agent framework.\n",
        );
        prompt.push_str("Your task is to write or improve a local script asset that will be executed through exec_local inside the active skill directory.\n\n");

        prompt.push_str("## Requirements\n");
        prompt.push_str("- Keep the script runnable from inside the skill directory\n");
        prompt
            .push_str("- Read input from stdin, args, or environment variables when appropriate\n");
        prompt.push_str("- Write user-facing results to stdout\n");
        prompt.push_str("- Handle errors gracefully and exit non-zero on failure\n");
        prompt.push_str("- Avoid unsafe shell expansion and command injection\n");
        prompt.push_str("- Prefer small, deterministic entrypoints\n\n");

        if let Some(source_path) = &context.source_path {
            prompt.push_str(&format!("## Target File\n{}\n\n", source_path));
        }

        let enriched = self.gather_evolution_context(context);
        prompt.push_str(&self.format_enriched_context(&enriched));

        if is_manual {
            if let TriggerReason::ManualRequest { ref description } = context.trigger {
                prompt.push_str(&format!(
                    "## Task\nCreate or improve a Blockcell local script for: {}\n\n",
                    description
                ));
            }
        } else {
            prompt.push_str(&format!(
                "## Task\nFix the existing Blockcell local script for skill '{}' to address the following issue. Preserve the skill's purpose and change only what is needed.\n\n",
                context.skill_name
            ));
            if let Some(error) = &context.error_stack {
                prompt.push_str(&format!(
                    "## Issue\n```\n{}\n```\n\n",
                    Self::sanitize_for_code_fence(error)
                ));
            }
        }

        if let Some(snippet) = &context.source_snippet {
            let fence = context
                .source_path
                .as_deref()
                .and_then(|path| {
                    std::path::Path::new(path)
                        .extension()
                        .and_then(|ext| ext.to_str())
                })
                .map(|ext| match ext {
                    "sh" | "bash" | "zsh" => "bash",
                    "js" => "javascript",
                    "php" => "php",
                    "rb" => "ruby",
                    _ => "text",
                })
                .unwrap_or("text");
            prompt.push_str(&format!(
                "## Current Script Content\n```{}\n{}\n```\n\n",
                fence,
                Self::sanitize_for_code_fence(snippet)
            ));
        }

        prompt.push_str("## Output Format\n");
        prompt.push_str("Generate the COMPLETE local script content.\n");
        prompt.push_str("If the skill can directly produce final user-facing text, return `display_text`. Otherwise return `summary_data` as a lightweight structured summary for runtime/LLM polishing.\n");
        prompt.push_str("If this skill also needs a post-execution result polishing step, you may optionally add `## Summary {#summary}` to the paired SKILL.md. Treat that section as a soft hint for runtime summary handling, not a hard requirement.\n");
        prompt.push_str(
            "If you output `meta.yaml`, it must follow the minimal meta rules below.\n\n",
        );
        prompt.push_str(Self::trigger_rules_prompt());
        prompt.push_str("The script must be runnable by exec_local and should not rely on unsafe external assumptions.\n");

        Ok(prompt)
    }

    pub(crate) fn build_local_script_fix_prompt(
        &self,
        context: &EvolutionContext,
        current_feedback: &FeedbackEntry,
        history: &[FeedbackEntry],
    ) -> Result<String> {
        let is_manual = matches!(context.trigger, TriggerReason::ManualRequest { .. });
        let mut prompt = String::new();

        prompt.push_str(
            "You are a local script and CLI skill developer for the blockcell agent framework.\n",
        );
        prompt.push_str("Your task is to fix issues in a local script asset that will be executed through exec_local.\n\n");

        let enriched = self.gather_evolution_context(context);
        prompt.push_str(&self.format_enriched_context(&enriched));

        if is_manual {
            if let TriggerReason::ManualRequest { ref description } = context.trigger {
                prompt.push_str(&format!(
                    "## Original Task\nCreate or improve a Blockcell local script for: {}\n\n",
                    description
                ));
            }
        } else {
            prompt.push_str(&format!(
                "## Original Task\nFix the existing Blockcell local script for skill '{}'. Keep the same scope and repair only the broken parts.\n\n",
                context.skill_name
            ));
        }

        prompt.push_str("## Previous Content (has issues)\n");
        prompt.push_str(&format!(
            "```\n{}\n```\n\n",
            Self::sanitize_for_code_fence(&current_feedback.previous_code)
        ));
        prompt.push_str(&format!(
            "## Issues Found ({})\n{}\n\n",
            current_feedback.stage, current_feedback.feedback
        ));

        let prev_attempts: Vec<&FeedbackEntry> = history
            .iter()
            .filter(|h| h.attempt < current_feedback.attempt)
            .collect();
        if !prev_attempts.is_empty() {
            prompt.push_str("## Previous Attempt History\n");
            for entry in prev_attempts {
                prompt.push_str(&format!(
                    "### Attempt #{} ({} failure)\n{}\n\n",
                    entry.attempt, entry.stage, entry.feedback
                ));
            }
        }

        prompt.push_str("## Instructions\n");
        prompt.push_str("Fix ALL the issues listed above and generate the COMPLETE corrected local script content.\n");
        prompt.push_str("If the skill can directly produce final user-facing text, return `display_text`. Otherwise return `summary_data` as a lightweight structured summary for runtime/LLM polishing.\n");
        prompt.push_str("If this skill also needs a post-execution result polishing step, you may optionally add `## Summary {#summary}` to the paired SKILL.md. Treat that section as a soft hint for runtime summary handling, not a hard requirement.\n");
        prompt.push_str(
            "If you output `meta.yaml`, it must follow the minimal meta rules below.\n\n",
        );
        prompt.push_str(Self::trigger_rules_prompt());
        prompt.push_str("Do NOT leave any of the reported issues unfixed.\n");

        Ok(prompt)
    }
}
