//! Accessibility tree snapshot and ref system.
//!
//! Converts Chrome's accessibility tree into a compact text representation
//! with deterministic element refs (@e1, @e2, ...) for AI-friendly interaction.

use serde_json::{json, Value};
use std::collections::HashMap;

/// An accessibility node with ref annotation.
#[derive(Debug, Clone)]
pub struct AXNode {
    pub role: String,
    pub name: String,
    pub value: String,
    pub description: String,
    pub node_id: Option<i64>,
    pub backend_node_id: Option<i64>,
    pub ref_id: Option<String>,
    pub properties: HashMap<String, Value>,
    pub children: Vec<AXNode>,
    pub depth: usize,
    pub interactive: bool,
    pub focused: bool,
    pub checked: Option<bool>,
    pub disabled: bool,
    pub expanded: Option<bool>,
    pub level: Option<i32>,
}

/// Roles considered interactive (buttons, inputs, links, etc.).
const INTERACTIVE_ROLES: &[&str] = &[
    "button", "link", "textbox", "searchbox", "combobox", "listbox",
    "menuitem", "menuitemcheckbox", "menuitemradio", "option",
    "radio", "checkbox", "switch", "slider", "spinbutton",
    "tab", "treeitem", "gridcell", "columnheader", "rowheader",
    "textField", "TextField", "select",
];

/// Roles that are structural/container (skip in compact mode if empty).
const STRUCTURAL_ROLES: &[&str] = &[
    "generic", "none", "presentation", "group", "region",
    "GenericContainer", "Section",
];

/// Parse the CDP accessibility tree response into our AXNode tree.
pub fn parse_ax_tree(cdp_response: &Value) -> Vec<AXNode> {
    let nodes = match cdp_response.get("nodes").and_then(|v| v.as_array()) {
        Some(arr) => arr,
        None => return Vec::new(),
    };

    if nodes.is_empty() {
        return Vec::new();
    }

    // Build a map of nodeId -> cdp node
    let mut node_map: HashMap<String, &Value> = HashMap::new();
    for node in nodes {
        if let Some(id) = node.get("nodeId").and_then(|v| v.as_str()) {
            node_map.insert(id.to_string(), node);
        }
    }

    // Build tree from root
    if let Some(root) = nodes.first() {
        let root_id = root.get("nodeId").and_then(|v| v.as_str()).unwrap_or("");
        vec![build_ax_node(root_id, &node_map, 0)]
    } else {
        Vec::new()
    }
}

fn build_ax_node(
    node_id: &str,
    node_map: &HashMap<String, &Value>,
    depth: usize,
) -> AXNode {
    let node = match node_map.get(node_id) {
        Some(n) => *n,
        None => {
            return AXNode {
                role: "unknown".to_string(),
                name: String::new(),
                value: String::new(),
                description: String::new(),
                node_id: None,
                backend_node_id: None,
                ref_id: None,
                properties: HashMap::new(),
                children: Vec::new(),
                depth,
                interactive: false,
                focused: false,
                checked: None,
                disabled: false,
                expanded: None,
                level: None,
            };
        }
    };

    let role = get_ax_value(node, "role");
    let name = get_ax_value(node, "name");
    let value = get_ax_value(node, "value");
    let description = get_ax_value(node, "description");

    let backend_node_id = node
        .get("backendDOMNodeId")
        .and_then(|v| v.as_i64());

    let interactive = INTERACTIVE_ROLES.iter().any(|r| r.eq_ignore_ascii_case(&role));

    // Parse properties
    let mut properties = HashMap::new();
    let mut focused = false;
    let mut checked = None;
    let mut disabled = false;
    let mut expanded = None;
    let mut level = None;

    if let Some(props) = node.get("properties").and_then(|v| v.as_array()) {
        for prop in props {
            let prop_name = prop.get("name").and_then(|v| v.as_str()).unwrap_or("");
            let prop_value = prop.get("value").and_then(|v| v.get("value")).cloned().unwrap_or(Value::Null);
            match prop_name {
                "focused" => focused = prop_value.as_bool().unwrap_or(false),
                "checked" => checked = prop_value.as_bool().or(Some(false)),
                "disabled" => disabled = prop_value.as_bool().unwrap_or(false),
                "expanded" => expanded = prop_value.as_bool(),
                "level" => level = prop_value.as_i64().map(|v| v as i32),
                _ => {}
            }
            properties.insert(prop_name.to_string(), prop_value);
        }
    }

    // Build children
    let children = if let Some(child_ids) = node.get("childIds").and_then(|v| v.as_array()) {
        child_ids
            .iter()
            .filter_map(|id| id.as_str())
            .map(|id| build_ax_node(id, node_map, depth + 1))
            .collect()
    } else {
        Vec::new()
    };

    AXNode {
        role,
        name,
        value,
        description,
        node_id: node.get("nodeId").and_then(|v| v.as_str()).and_then(|s| s.parse().ok()),
        backend_node_id,
        ref_id: None,
        properties,
        children,
        depth,
        interactive,
        focused,
        checked,
        disabled,
        expanded,
        level,
    }
}

fn get_ax_value(node: &Value, field: &str) -> String {
    node.get(field)
        .and_then(|v| {
            // CDP returns {type: "...", value: "..."} for role/name/value/description
            v.get("value")
                .and_then(|val| val.as_str())
                .or_else(|| v.as_str())
        })
        .unwrap_or("")
        .to_string()
}

/// Assign ref IDs to interactive elements in the tree.
/// Returns the updated ref counter and a map of ref_id -> node metadata.
pub fn assign_refs(
    nodes: &mut Vec<AXNode>,
    start_counter: u32,
    interactive_only: bool,
) -> (u32, HashMap<String, Value>) {
    let mut counter = start_counter;
    let mut ref_map = HashMap::new();
    for node in nodes.iter_mut() {
        assign_refs_recursive(node, &mut counter, &mut ref_map, interactive_only);
    }
    (counter, ref_map)
}

fn assign_refs_recursive(
    node: &mut AXNode,
    counter: &mut u32,
    ref_map: &mut HashMap<String, Value>,
    interactive_only: bool,
) {
    let should_assign = if interactive_only {
        node.interactive
    } else {
        // Assign to interactive + any named non-structural element
        node.interactive
            || (!node.name.is_empty()
                && !STRUCTURAL_ROLES.iter().any(|r| r.eq_ignore_ascii_case(&node.role)))
    };

    if should_assign {
        *counter += 1;
        let ref_id = format!("e{}", counter);
        node.ref_id = Some(ref_id.clone());
        ref_map.insert(
            ref_id,
            json!({
                "role": node.role,
                "name": node.name,
                "backendNodeId": node.backend_node_id,
                "interactive": node.interactive,
            }),
        );
    }

    for child in node.children.iter_mut() {
        assign_refs_recursive(child, counter, ref_map, interactive_only);
    }
}

/// Render the accessibility tree as a compact text representation.
pub fn render_tree(
    nodes: &[AXNode],
    compact: bool,
    max_depth: Option<usize>,
) -> String {
    let mut output = String::new();
    for node in nodes {
        render_node(&mut output, node, 0, compact, max_depth);
    }
    output
}

fn render_node(
    output: &mut String,
    node: &AXNode,
    indent: usize,
    compact: bool,
    max_depth: Option<usize>,
) {
    if let Some(max) = max_depth {
        if indent > max {
            return;
        }
    }

    // In compact mode, skip empty structural elements
    if compact && STRUCTURAL_ROLES.iter().any(|r| r.eq_ignore_ascii_case(&node.role))
        && node.name.is_empty() && node.ref_id.is_none() {
            // Skip this node but still render children
            for child in &node.children {
                render_node(output, child, indent, compact, max_depth);
            }
            return;
        }

    // Skip nodes with no useful content in compact mode
    if compact && node.role == "StaticText" && node.name.is_empty() {
        return;
    }

    let prefix = "  ".repeat(indent);

    // Build the line
    let mut line = format!("{}- {}", prefix, node.role);

    if !node.name.is_empty() {
        let name = if node.name.len() > 80 {
            format!("{}...", crate::safe_truncate(&node.name, 77))
        } else {
            node.name.clone()
        };
        line.push_str(&format!(" \"{}\"", name));
    }

    // Add ref
    if let Some(ref ref_id) = node.ref_id {
        line.push_str(&format!(" [ref={}]", ref_id));
    }

    // Add annotations
    if let Some(level) = node.level {
        line.push_str(&format!(" [level={}]", level));
    }
    if node.focused {
        line.push_str(" [focused]");
    }
    if let Some(true) = node.checked {
        line.push_str(" [checked]");
    }
    if node.disabled {
        line.push_str(" [disabled]");
    }
    if let Some(expanded) = node.expanded {
        line.push_str(if expanded { " [expanded]" } else { " [collapsed]" });
    }
    if !node.value.is_empty() && node.value != node.name {
        let val = if node.value.len() > 60 {
            format!("{}...", crate::safe_truncate(&node.value, 57))
        } else {
            node.value.clone()
        };
        line.push_str(&format!(" value=\"{}\"", val));
    }

    output.push_str(&line);
    output.push('\n');

    // Render children
    for child in &node.children {
        render_node(output, child, indent + 1, compact, max_depth);
    }
}

/// Build a JSON representation of the snapshot for --json mode.
pub fn snapshot_to_json(
    tree_text: &str,
    refs: &HashMap<String, Value>,
) -> Value {
    json!({
        "snapshot": tree_text,
        "refs": refs,
        "ref_count": refs.len(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_interactive_roles() {
        assert!(INTERACTIVE_ROLES.contains(&"button"));
        assert!(INTERACTIVE_ROLES.contains(&"textbox"));
        assert!(INTERACTIVE_ROLES.contains(&"link"));
        assert!(!INTERACTIVE_ROLES.contains(&"generic"));
    }

    #[test]
    fn test_assign_refs() {
        let mut nodes = vec![AXNode {
            role: "button".to_string(),
            name: "Submit".to_string(),
            value: String::new(),
            description: String::new(),
            node_id: Some(1),
            backend_node_id: Some(10),
            ref_id: None,
            properties: HashMap::new(),
            children: vec![],
            depth: 0,
            interactive: true,
            focused: false,
            checked: None,
            disabled: false,
            expanded: None,
            level: None,
        }];

        let (counter, refs) = assign_refs(&mut nodes, 0, true);
        assert_eq!(counter, 1);
        assert!(refs.contains_key("e1"));
        assert_eq!(nodes[0].ref_id, Some("e1".to_string()));
    }

    #[test]
    fn test_render_tree() {
        let nodes = vec![AXNode {
            role: "heading".to_string(),
            name: "Example Domain".to_string(),
            value: String::new(),
            description: String::new(),
            node_id: Some(1),
            backend_node_id: Some(10),
            ref_id: Some("e1".to_string()),
            properties: HashMap::new(),
            children: vec![],
            depth: 0,
            interactive: false,
            focused: false,
            checked: None,
            disabled: false,
            expanded: None,
            level: Some(1),
        }];

        let text = render_tree(&nodes, false, None);
        assert!(text.contains("heading \"Example Domain\" [ref=e1] [level=1]"));
    }

    #[test]
    fn test_compact_skips_structural() {
        let nodes = vec![AXNode {
            role: "generic".to_string(),
            name: String::new(),
            value: String::new(),
            description: String::new(),
            node_id: None,
            backend_node_id: None,
            ref_id: None,
            properties: HashMap::new(),
            children: vec![AXNode {
                role: "button".to_string(),
                name: "Click me".to_string(),
                value: String::new(),
                description: String::new(),
                node_id: Some(2),
                backend_node_id: Some(20),
                ref_id: Some("e1".to_string()),
                properties: HashMap::new(),
                children: vec![],
                depth: 1,
                interactive: true,
                focused: false,
                checked: None,
                disabled: false,
                expanded: None,
                level: None,
            }],
            depth: 0,
            interactive: false,
            focused: false,
            checked: None,
            disabled: false,
            expanded: None,
            level: None,
        }];

        let text = render_tree(&nodes, true, None);
        // In compact mode, the empty generic container should be skipped
        assert!(!text.contains("generic"));
        assert!(text.contains("button \"Click me\" [ref=e1]"));
    }
}
