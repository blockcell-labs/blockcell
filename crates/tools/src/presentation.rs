//! 工具的三层定义（汇总为 [`ToolDefinition`](crate::ToolDefinition)）：
//!
//! - [`ToolSchema`](crate::ToolSchema)：model-facing，名称与参数；
//! - [`ToolExecutionSpec`](crate::ToolExecutionSpec)：权限、幂等性、执行环境与确认要求声明，
//!   Runtime 使用（执行环境仅用于审计，确认要求驱动准入）；
//! - [`ToolPresentation`]：模型 / WebUI / channel 看到什么（描述、可见性、分组、排序）。
//!
//! Presentation 不影响能否执行：可见性只决定是否出现在初始工具列表，权限仍由执行规格与
//! Runtime 的作用域、策略决定。同一工具可通过 agent 配置在不同 agent 中呈现不同描述。

pub use blockcell_core::config::{ToolPresentationConfig, ToolVisibility};
use serde::Serialize;

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize)]
pub struct ToolPresentation {
    pub visibility: ToolVisibility,
    /// 覆盖 schema 中的描述；为空时使用 schema 描述。
    #[serde(skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub group: Option<String>,
    /// 越小越靠前；相同时按名称。
    pub order: i32,
}

impl ToolPresentation {
    pub fn discoverable() -> Self {
        Self {
            visibility: ToolVisibility::Discoverable,
            ..Self::default()
        }
    }

    /// 叠加配置中已设置的字段。
    pub fn with_overrides(mut self, overrides: &ToolPresentationConfig) -> Self {
        if let Some(visibility) = overrides.visibility {
            self.visibility = visibility;
        }
        if let Some(description) = &overrides.description {
            self.description = Some(description.clone());
        }
        if let Some(group) = &overrides.group {
            self.group = Some(group.clone());
        }
        if let Some(order) = overrides.order {
            self.order = order;
        }
        self
    }

    pub fn is_visible(&self) -> bool {
        self.visibility == ToolVisibility::Visible
    }
}
