pub(crate) struct SkillDecisionEngine;

impl SkillDecisionEngine {
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

        // Fuzzy fallback: only accept a *unique* match. With same-prefix skill
        // names (e.g. "deploy" / "deploy-prod") a bidirectional `contains`
        // would pick whichever entry happens to come first, which is both
        // non-deterministic and frequently wrong. Too-short selections are
        // rejected as well to avoid matching half the catalogue.
        if selected.chars().count() < 3 {
            return None;
        }
        let mut matches = candidates.iter().filter(|(name, _)| {
            selected.contains(name.as_str()) || name.contains(selected)
        });
        let first = matches.next()?;
        if matches.next().is_some() {
            return None;
        }
        Some(first.0.clone())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn ambiguous_selection_is_rejected() {
        let candidates = vec![("deploy".into(), "".into()), ("deploy-prod".into(), "".into())];
        assert_eq!(SkillDecisionEngine::normalize_selected_skill_name("use deploy-prod please", &candidates), None);
        assert_eq!(SkillDecisionEngine::normalize_selected_skill_name("dep", &candidates), None);
    }
}
