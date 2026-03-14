use blockcell_skills::manager::SkillExecution;
use serde_json::{json, Map, Value};

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum RouteChoiceKind {
    ContinueRecentSkill,
    UseCurrentSkill,
    UseTools,
    Chat,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct RouteChoice {
    pub(crate) kind: RouteChoiceKind,
    pub(crate) skill_name: Option<String>,
}

pub(crate) struct SkillDecisionEngine;

impl SkillDecisionEngine {
    fn truncate_text(text: &str, max_chars: usize) -> String {
        if text.chars().count() <= max_chars {
            return text.to_string();
        }

        match text.char_indices().nth(max_chars) {
            Some((idx, _)) => format!("{}...", &text[..idx]),
            None => text.to_string(),
        }
    }

    fn compact_json_for_prompt(value: &Value, max_chars: usize) -> String {
        let raw = serde_json::to_string_pretty(value).unwrap_or_else(|_| value.to_string());
        Self::truncate_text(&raw, max_chars)
    }

    pub(crate) fn build_structured_skill_schema(
        skill_name: &str,
        execution: &SkillExecution,
    ) -> Value {
        let actions = execution
            .actions
            .iter()
            .map(|action| {
                let arguments = action
                    .arguments
                    .iter()
                    .map(|(name, arg)| {
                        (
                            name.clone(),
                            json!({
                                "type": arg.kind,
                                "required": arg.required,
                                "description": arg.description,
                                "enum": arg.enum_values,
                            }),
                        )
                    })
                    .collect::<Map<String, Value>>();

                json!({
                    "name": action.name,
                    "description": action.description,
                    "triggers": action.triggers,
                    "arguments": arguments,
                    "argv": action.argv,
                })
            })
            .collect::<Vec<_>>();

        json!({
            "skill": skill_name,
            "runtime_kind": execution.normalized_kind(),
            "dispatch_kind": execution.effective_dispatch_kind(),
            "summary_mode": execution.effective_summary_mode(),
            "actions": actions,
        })
    }

    pub(crate) fn normalize_selected_skill_name(
        selected: &str,
        candidates: &[(String, String)],
    ) -> Option<String> {
        let selected = selected.trim();
        if selected.is_empty() {
            return None;
        }

        if let Some((name, _)) = candidates.iter().find(|(name, _)| name == selected) {
            return Some(name.clone());
        }

        candidates
            .iter()
            .find(|(name, _)| selected.contains(name.as_str()) || name.contains(selected))
            .map(|(name, _)| name.clone())
    }

    pub(crate) fn build_method_decision_prompt(
        user_input: &str,
        skill_name: &str,
        execution: &SkillExecution,
        continuation_context: Option<&Value>,
        recent_trace: &[Value],
    ) -> String {
        let schema = Self::build_structured_skill_schema(skill_name, execution);
        let mut prompt = format!(
            "你是技能调度器，请根据用户请求选择最合适的方法并构造参数。\n\n\
            技能结构化 schema：\n{}\n\n\
            用户请求：{}\n\n",
            schema, user_input
        );

        if !recent_trace.is_empty() {
            let trace_json = Value::Array(recent_trace.to_vec());
            prompt.push_str("最近与该技能相关的执行记录：\n");
            prompt.push_str(&Self::compact_json_for_prompt(&trace_json, 1200));
            prompt.push_str("\n\n");
        }

        if let Some(context) = continuation_context {
            prompt.push_str("该技能可用的续接上下文：\n");
            prompt.push_str(&Self::compact_json_for_prompt(context, 2400));
            prompt.push_str("\n\n");
            prompt.push_str(
                "如果用户是在继续上一轮结果，优先使用续接上下文中的结构化信息，而不是重新发起搜索或改走无关工具。\n\n",
            );
        }

        prompt.push_str(
            "请严格输出 JSON，不要包含任何额外说明：\n\
            {\"method\": \"方法名\", \"arguments\": {\"参数名\": \"参数值\"}}",
        );
        prompt
    }

    pub(crate) fn build_routing_decision_prompt(
        user_input: &str,
        recent_execution_trace: &[Value],
        skill_candidates: &[String],
        tool_candidates: &[String],
        continuation_context_hints: &[String],
    ) -> String {
        let trace_json = Value::Array(recent_execution_trace.to_vec());
        let skills_json = Value::Array(
            skill_candidates
                .iter()
                .map(|value| Value::String(value.clone()))
                .collect(),
        );
        let tools_json = Value::Array(
            tool_candidates
                .iter()
                .map(|value| Value::String(value.clone()))
                .collect(),
        );
        let hints_json = Value::Array(
            continuation_context_hints
                .iter()
                .map(|value| Value::String(value.clone()))
                .collect(),
        );

        format!(
            "你是对话路由仲裁器。请根据用户输入、最近真实执行记录、当前候选 skill 和 tool，判断这轮应该继续最近 skill、使用当前命中的 skill、走 tools，还是普通聊天。\n\n\
            用户输入：{}\n\n\
            最近真实执行记录：\n{}\n\n\
            当前命中的 skill 候选：\n{}\n\n\
            当前命中的 tool 候选：\n{}\n\n\
            当前可用的续接上下文索引：\n{}\n\n\
            只允许输出 JSON，不要输出解释：\n\
            {{\"route\": \"continue_recent_skill|use_current_skill|use_tools|chat\", \"skill_name\": \"可选\"}}",
            user_input,
            Self::compact_json_for_prompt(&trace_json, 1600),
            Self::compact_json_for_prompt(&skills_json, 600),
            Self::compact_json_for_prompt(&tools_json, 800),
            Self::compact_json_for_prompt(&hints_json, 800),
        )
    }

    pub(crate) fn normalize_route_choice(
        payload: &Value,
        current_skill_candidates: &[String],
        recent_skill_names: &[String],
    ) -> Option<RouteChoice> {
        let route = payload.get("route").and_then(|v| v.as_str())?.trim();
        let skill_name = payload
            .get("skill_name")
            .and_then(|v| v.as_str())
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .map(str::to_string);

        match route {
            "continue_recent_skill" => {
                let skill_name = skill_name?;
                if recent_skill_names.iter().any(|name| name == &skill_name) {
                    Some(RouteChoice {
                        kind: RouteChoiceKind::ContinueRecentSkill,
                        skill_name: Some(skill_name),
                    })
                } else {
                    None
                }
            }
            "use_current_skill" => {
                let skill_name = skill_name?;
                if current_skill_candidates
                    .iter()
                    .any(|name| name == &skill_name)
                {
                    Some(RouteChoice {
                        kind: RouteChoiceKind::UseCurrentSkill,
                        skill_name: Some(skill_name),
                    })
                } else {
                    None
                }
            }
            "use_tools" => Some(RouteChoice {
                kind: RouteChoiceKind::UseTools,
                skill_name: None,
            }),
            "chat" => Some(RouteChoice {
                kind: RouteChoiceKind::Chat,
                skill_name: None,
            }),
            _ => None,
        }
    }
}
