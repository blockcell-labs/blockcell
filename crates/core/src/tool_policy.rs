use glob::Pattern;
use regex::Regex;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::path::Path;
use tracing::{info, warn};

const MAX_REGEX_LEN: usize = 1024;

const PATH_ARG_KEYS: &[&str] = &[
    "path",
    "file_path",
    "filepath",
    "target",
    "dir",
    "directory",
    "destination",
    "output_path",
    "working_dir",
];

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ToolPolicyDecision {
    Allow,
    Ask,
    Deny,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ToolPolicyRule {
    pub name: String,
    /// Tool-name glob. Use `|` to provide multiple glob alternatives.
    pub tool: String,
    pub decision: ToolPolicyDecision,
    #[serde(default)]
    pub when: Option<ToolPolicyCondition>,
    #[serde(default)]
    pub description: Option<String>,
    #[serde(default)]
    pub inherit_from: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct ToolPolicyCondition {
    #[serde(default)]
    pub command: Option<String>,
    #[serde(default)]
    pub command_regex: Option<String>,
    #[serde(default)]
    pub path_glob: Option<String>,
    #[serde(default)]
    pub channel: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ToolPolicyConfig {
    #[serde(default = "default_version")]
    pub version: u32,
    #[serde(default = "default_allow")]
    pub default_decision: ToolPolicyDecision,
    #[serde(default)]
    pub simulation_mode: bool,
    #[serde(default)]
    pub rules: Vec<ToolPolicyRule>,
    #[serde(default)]
    pub rule_groups: HashMap<String, Vec<ToolPolicyRule>>,
}

fn default_version() -> u32 {
    1
}

fn default_allow() -> ToolPolicyDecision {
    ToolPolicyDecision::Allow
}

impl Default for ToolPolicyConfig {
    fn default() -> Self {
        Self {
            version: 1,
            default_decision: ToolPolicyDecision::Allow,
            simulation_mode: false,
            rules: Vec::new(),
            rule_groups: HashMap::new(),
        }
    }
}

pub struct ToolCallContext<'a> {
    pub tool_name: &'a str,
    pub tool_args: &'a serde_json::Value,
    pub channel: &'a str,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PolicyEvalResult {
    pub decision: ToolPolicyDecision,
    pub matched_rule: Option<String>,
    pub description: Option<String>,
    pub simulated: bool,
}

#[derive(Debug, Clone)]
struct CompiledRule {
    name: String,
    tool_globs: Vec<Pattern>,
    decision: ToolPolicyDecision,
    command_contains: Option<String>,
    command_regex: Option<Regex>,
    path_glob: Option<Pattern>,
    channel: Option<String>,
    description: Option<String>,
}

#[derive(Debug, Clone)]
pub struct ToolPolicy {
    compiled: Vec<CompiledRule>,
    default_decision: ToolPolicyDecision,
    simulation_mode: bool,
    pub from_file: bool,
}

impl ToolPolicy {
    pub fn load(policy_file: &Path) -> Self {
        if !policy_file.exists() {
            info!(
                path = %policy_file.display(),
                "Tool policy file not found; using permissive defaults"
            );
            return Self::permissive();
        }

        match std::fs::read_to_string(policy_file) {
            Ok(content) => match serde_yaml::from_str::<ToolPolicyConfig>(&content) {
                Ok(config) => Self::compile(config, true),
                Err(e) => {
                    warn!(
                        path = %policy_file.display(),
                        error = %e,
                        "Failed to parse tool policy; denying all tool calls"
                    );
                    Self::fail_closed(true)
                }
            },
            Err(e) => {
                warn!(
                    path = %policy_file.display(),
                    error = %e,
                    "Failed to read tool policy; denying all tool calls"
                );
                Self::fail_closed(true)
            }
        }
    }

    pub fn from_config(config: ToolPolicyConfig) -> Self {
        Self::compile(config, false)
    }

    pub fn permissive() -> Self {
        Self {
            compiled: Vec::new(),
            default_decision: ToolPolicyDecision::Allow,
            simulation_mode: false,
            from_file: false,
        }
    }

    fn fail_closed(from_file: bool) -> Self {
        Self {
            compiled: Vec::new(),
            default_decision: ToolPolicyDecision::Deny,
            simulation_mode: false,
            from_file,
        }
    }

    fn compile(config: ToolPolicyConfig, from_file: bool) -> Self {
        let mut compiled = Vec::new();
        let mut invalid = false;
        for rule in &config.rules {
            if let Some(group_name) = &rule.inherit_from {
                if let Some(group_rules) = config.rule_groups.get(group_name) {
                    for group_rule in group_rules {
                        if Self::push_compiled(&mut compiled, group_rule).is_err() {
                            invalid = true;
                        }
                    }
                } else {
                    warn!(
                        rule = %rule.name,
                        group = %group_name,
                        "Tool policy inherit_from references an unknown rule group"
                    );
                    invalid = true;
                }
            }
            if Self::push_compiled(&mut compiled, rule).is_err() {
                invalid = true;
            }
        }

        if invalid {
            return Self::fail_closed(from_file);
        }

        Self {
            compiled,
            default_decision: config.default_decision,
            simulation_mode: config.simulation_mode,
            from_file,
        }
    }

    fn push_compiled(out: &mut Vec<CompiledRule>, rule: &ToolPolicyRule) -> Result<(), ()> {
        let mut tool_globs = Vec::new();
        for glob in rule
            .tool
            .split('|')
            .map(str::trim)
            .filter(|s| !s.is_empty())
        {
            match Pattern::new(glob) {
                Ok(pattern) => tool_globs.push(pattern),
                Err(e) => {
                    warn!(
                        rule = %rule.name,
                        glob,
                        error = %e,
                        "Invalid tool glob in policy rule"
                    );
                    return Err(());
                }
            }
        }
        if tool_globs.is_empty() {
            return Err(());
        }

        let command_regex = match rule
            .when
            .as_ref()
            .and_then(|condition| condition.command_regex.as_ref())
        {
            Some(regex_text) if regex_text.len() > MAX_REGEX_LEN => {
                warn!(
                    rule = %rule.name,
                    len = regex_text.len(),
                    "Tool policy command_regex exceeds max length; skipping rule"
                );
                return Err(());
            }
            Some(regex_text) => match Regex::new(regex_text) {
                Ok(regex) => Some(regex),
                Err(e) => {
                    warn!(
                        rule = %rule.name,
                        regex = %regex_text,
                        error = %e,
                        "Invalid tool policy command_regex; skipping rule"
                    );
                    return Err(());
                }
            },
            None => None,
        };

        let path_glob = match rule
            .when
            .as_ref()
            .and_then(|condition| condition.path_glob.as_ref())
        {
            Some(glob_text) => {
                let expanded = expand_tilde(glob_text);
                match Pattern::new(&expanded) {
                    Ok(pattern) => Some(pattern),
                    Err(e) => {
                        warn!(
                            rule = %rule.name,
                            glob = %glob_text,
                            error = %e,
                            "Invalid tool policy path_glob; skipping rule"
                        );
                        return Err(());
                    }
                }
            }
            None => None,
        };

        out.push(CompiledRule {
            name: rule.name.clone(),
            tool_globs,
            decision: rule.decision,
            command_contains: rule
                .when
                .as_ref()
                .and_then(|condition| condition.command.clone()),
            command_regex,
            path_glob,
            channel: rule
                .when
                .as_ref()
                .and_then(|condition| condition.channel.clone()),
            description: rule.description.clone(),
        });
        Ok(())
    }

    pub fn evaluate(&self, ctx: &ToolCallContext<'_>) -> PolicyEvalResult {
        // Deny outranks Ask, which outranks Allow: a broad `allow` rule can never
        // shadow a more specific `deny` no matter where it is declared (the classic
        // first-match-wins pitfall). Within one decision the first declared
        // matching rule still wins.
        let mut ask: Option<&CompiledRule> = None;
        let mut allow: Option<&CompiledRule> = None;

        for rule in &self.compiled {
            if !rule
                .tool_globs
                .iter()
                .any(|pattern| pattern.matches(ctx.tool_name))
            {
                continue;
            }
            if !self.conditions_match(rule, ctx) {
                continue;
            }

            match rule.decision {
                ToolPolicyDecision::Deny => return self.eval_result(rule, ctx.tool_name),
                ToolPolicyDecision::Ask => {
                    if ask.is_none() {
                        ask = Some(rule);
                    }
                }
                ToolPolicyDecision::Allow => {
                    if allow.is_none() {
                        allow = Some(rule);
                    }
                }
            }
        }

        if let Some(rule) = ask.or(allow) {
            return self.eval_result(rule, ctx.tool_name);
        }

        PolicyEvalResult {
            decision: self.default_decision,
            matched_rule: None,
            description: None,
            simulated: false,
        }
    }

    /// 把命中规则转成评估结果；模拟模式下只报告命中项，实际一律放行。
    fn eval_result(&self, rule: &CompiledRule, tool_name: &str) -> PolicyEvalResult {
        if self.simulation_mode {
            info!(
                rule = %rule.name,
                tool = %tool_name,
                would_decide = ?rule.decision,
                "[SIMULATION] Tool policy matched; allowing call"
            );
        }

        PolicyEvalResult {
            decision: if self.simulation_mode {
                ToolPolicyDecision::Allow
            } else {
                rule.decision
            },
            matched_rule: Some(rule.name.clone()),
            description: rule.description.clone(),
            simulated: self.simulation_mode,
        }
    }

    fn conditions_match(&self, rule: &CompiledRule, ctx: &ToolCallContext<'_>) -> bool {
        if let Some(expected) = &rule.command_contains {
            let command = ctx
                .tool_args
                .get("command")
                .and_then(|value| value.as_str())
                .unwrap_or("");
            if !command.contains(expected.as_str()) {
                return false;
            }
        }

        if let Some(regex) = &rule.command_regex {
            let command = ctx
                .tool_args
                .get("command")
                .and_then(|value| value.as_str())
                .unwrap_or("");
            if !regex.is_match(command) {
                return false;
            }
        }

        if let Some(path_glob) = &rule.path_glob {
            let Some(path) = PATH_ARG_KEYS
                .iter()
                .find_map(|key| ctx.tool_args.get(*key).and_then(|value| value.as_str()))
            else {
                return false;
            };
            let expanded_path = expand_tilde(path);
            if !path_glob.matches(&expanded_path) {
                return false;
            }
        }

        if let Some(channel) = &rule.channel {
            if ctx.channel != channel {
                return false;
            }
        }

        true
    }
}

impl Default for ToolPolicy {
    fn default() -> Self {
        Self::permissive()
    }
}

fn expand_tilde(path: &str) -> String {
    if let Some(rest) = path.strip_prefix("~/") {
        if let Some(home) = dirs::home_dir() {
            return home.join(rest).to_string_lossy().into_owned();
        }
    }
    path.to_string()
}
