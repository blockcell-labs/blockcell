//! Stable initial schema exposure budgeting. Capability and discovery catalogs
//! remain the caller's responsibility and are never modified here.

use serde_json::Value;
use std::collections::HashSet;

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ExposureReport {
    pub initial_bytes: usize,
    pub exposed_bytes: usize,
    pub deferred_count: usize,
    pub over_budget: bool,
}

/// Apply a byte budget to the initial JSON schema array, without changing any
/// retained schema. Protected tools are reserved before selecting optional tools
/// in input order. Their presence (and the empty array's two bytes) may exceed
/// the budget, which is reported rather than removing required entry points.
/// Call once before the initial request; do not rebudget discoveries mid-turn.
pub fn apply_schema_budget(
    schemas: &mut Vec<Value>,
    budget: Option<usize>,
    protected: &HashSet<String>,
) -> ExposureReport {
    let sizes: Vec<_> = schemas
        .iter()
        .map(|schema| schema.to_string().len())
        .collect();
    let initial_count = schemas.len();
    let initial_bytes = array_bytes(sizes.iter().copied());
    let Some(limit) = budget else {
        return ExposureReport {
            initial_bytes,
            exposed_bytes: initial_bytes,
            deferred_count: 0,
            over_budget: false,
        };
    };

    let mandatory: Vec<_> = schemas
        .iter()
        .map(|schema| {
            schema
                .get("function")
                .and_then(|function| function.get("name"))
                .or_else(|| schema.get("name"))
                .and_then(Value::as_str)
                .is_some_and(|name| protected.contains(name))
        })
        .collect();
    let mut selected_count = mandatory.iter().filter(|&&keep| keep).count();
    let mut exposed_bytes = array_bytes(
        sizes
            .iter()
            .zip(&mandatory)
            .filter_map(|(&size, &keep)| keep.then_some(size)),
    );
    let mut index = 0;
    schemas.retain(|_| {
        let current = index;
        index += 1;
        if mandatory[current] {
            return true;
        }
        let candidate_bytes = exposed_bytes
            .saturating_add(sizes[current])
            .saturating_add(usize::from(selected_count > 0));
        if candidate_bytes > limit {
            return false;
        }
        selected_count += 1;
        exposed_bytes = candidate_bytes;
        true
    });
    ExposureReport {
        initial_bytes,
        exposed_bytes,
        deferred_count: initial_count - schemas.len(),
        over_budget: exposed_bytes > limit,
    }
}

fn array_bytes(sizes: impl Iterator<Item = usize>) -> usize {
    let mut bytes: usize = 2;
    for (index, size) in sizes.enumerate() {
        bytes = bytes
            .saturating_add(size)
            .saturating_add(usize::from(index > 0));
    }
    bytes
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn schema(name: &str, description: &str) -> Value {
        json!({"type":"function","function":{"name":name,"description":description,"parameters":{"type":"object","properties":{"path":{"type":"string"}},"required":["path"]}}})
    }

    fn bytes(schemas: &[Value]) -> usize {
        serde_json::to_vec(schemas)
            .expect("JSON values serialize")
            .len()
    }

    #[test]
    fn absent_budget_preserves_all_schemas_and_parameters() {
        let mut schemas = vec![
            schema("read_file", "Read files"),
            schema("write_file", "Write files"),
        ];
        let original = schemas.clone();
        let report = apply_schema_budget(&mut schemas, None, &HashSet::new());
        assert_eq!(schemas, original);
        assert_eq!(report.initial_bytes, bytes(&original));
        assert_eq!(report.exposed_bytes, bytes(&original));
        assert_eq!(report.deferred_count, 0);
        assert!(!report.over_budget);
    }

    #[test]
    fn protected_schemas_survive_tiny_budget_and_report_overflow() {
        let mut schemas = vec![
            schema("optional", "An optional tool"),
            schema("tool_search", "Find tools"),
            schema("activate_skill", "Load a skill"),
        ];
        let original = schemas.clone();
        let protected = HashSet::from(["tool_search".into(), "activate_skill".into()]);
        let report = apply_schema_budget(&mut schemas, Some(1), &protected);
        assert_eq!(schemas, original[1..]);
        assert!(report.over_budget);
        assert_eq!(report.initial_bytes, bytes(&original));
        assert_eq!(report.exposed_bytes, bytes(&schemas));
        assert_eq!(report.deferred_count, 1);
    }

    #[test]
    fn budget_reserves_late_protected_tools_and_preserves_input_order() {
        let optional = schema("first", "First optional");
        let second = schema("second", "Second optional");
        let core = schema("tool_search", "Find tools");
        let budget = bytes(&[optional.clone(), core.clone()]);
        let original = vec![optional.clone(), second, core.clone()];
        let mut schemas = original.clone();
        let protected = HashSet::from(["tool_search".into()]);
        let report = apply_schema_budget(&mut schemas, Some(budget), &protected);
        assert_eq!(schemas, vec![optional, core]);
        assert_eq!(report.exposed_bytes, budget);
        assert_eq!(report.deferred_count, 1);
        let mut repeated = original;
        assert_eq!(
            apply_schema_budget(&mut repeated, Some(budget), &protected),
            report
        );
        assert_eq!(repeated, schemas);
    }

    #[test]
    fn exact_boundary_counts_array_brackets_and_commas() {
        let original = vec![schema("first", "One"), schema("second", "Two")];
        let exact = bytes(&original);
        let mut schemas = original.clone();
        let report = apply_schema_budget(&mut schemas, Some(exact), &HashSet::new());
        assert_eq!(schemas, original);
        assert_eq!(report.exposed_bytes, exact);
        assert!(!report.over_budget);
        let mut schemas = original;
        let report = apply_schema_budget(&mut schemas, Some(exact - 1), &HashSet::new());
        assert_eq!(schemas.len(), 1);
        assert_eq!(report.deferred_count, 1);
        assert_eq!(report.exposed_bytes, bytes(&schemas));
        assert!(!report.over_budget);
    }

    #[test]
    fn unicode_budget_measures_utf8_bytes_and_keeps_schema_intact() {
        let original = vec![schema("read_file", "读取中文文件")];
        let serialized = serde_json::to_string(&original).unwrap();
        assert!(serialized.len() > serialized.chars().count());
        let mut schemas = original.clone();
        let report = apply_schema_budget(
            &mut schemas,
            Some(serialized.chars().count()),
            &HashSet::new(),
        );
        assert!(schemas.is_empty());
        assert_eq!(report.initial_bytes, serialized.len());
        assert_eq!(report.exposed_bytes, 2);
        let mut schemas = original.clone();
        apply_schema_budget(&mut schemas, Some(serialized.len()), &HashSet::new());
        assert_eq!(schemas, original);
    }

    #[test]
    fn zero_budget_has_empty_array_overhead_and_never_panics() {
        let mut schemas = vec![schema("optional", "Optional")];
        let report = apply_schema_budget(&mut schemas, Some(0), &HashSet::new());
        assert!(schemas.is_empty());
        assert_eq!(report.exposed_bytes, 2);
        assert!(report.over_budget);
        assert_eq!(report.deferred_count, 1);
        let report = apply_schema_budget(&mut schemas, Some(2), &HashSet::new());
        assert_eq!(report.initial_bytes, 2);
        assert!(!report.over_budget);
    }
}
