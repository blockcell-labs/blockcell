fn truncate_chars(input: &str, max_chars: usize) -> String {
    if input.chars().count() <= max_chars {
        return input.to_string();
    }

    match input.char_indices().nth(max_chars) {
        Some((idx, _)) => input[..idx].to_string(),
        None => input.to_string(),
    }
}

pub(crate) struct SkillSummaryFormatter;

impl SkillSummaryFormatter {
    pub(crate) fn build_prompt(
        user_question: &str,
        skill_name: &str,
        method_name: Option<&str>,
        skill_md: &str,
        execution_result: &str,
    ) -> String {
        let skill_md_brief = truncate_chars(skill_md, 1200);
        let result_brief = truncate_chars(execution_result, 4000);
        let method_line = method_name
            .filter(|value| !value.trim().is_empty())
            .map(|value| format!("方法：{}\n", value))
            .unwrap_or_default();

        format!(
            "用户问题：{}\n技能：{}\n{}\
             \n技能说明摘要：\n{}\n\n执行结果：\n{}",
            user_question, skill_name, method_line, skill_md_brief, result_brief
        )
    }
}
