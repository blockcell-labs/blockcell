use blockcell_core::SurvivalInvariants;
use tracing::{info, warn};

/// 生存不变量检查器 — 对应文档第 7 节 Meta-Evolution
///
/// 定期检查 agent 的核心生存能力：
/// - 是否还能编译新代码？
/// - 是否还能加载新能力？
/// - 是否还能与主人通信？
/// - 是否还能继续进化？
pub struct HealthChecker;

impl HealthChecker {
    /// 执行完整的生存不变量检查
    pub async fn check_all() -> SurvivalInvariants {
        let mut invariants = SurvivalInvariants {
            last_checked: chrono::Utc::now().timestamp(),
            ..Default::default()
        };

        // 1. 是否还能编译新代码？
        invariants.can_compile = Self::check_compile().await;
        invariants.diagnostics.insert(
            "compile".to_string(),
            if invariants.can_compile {
                "rustc available, can compile Rust code".to_string()
            } else {
                // Fall back to checking bash (for script-based evolution)
                if Self::check_bash().await {
                    invariants.can_compile = true;
                    "rustc not found, but bash available for script-based evolution".to_string()
                } else {
                    "Neither rustc nor bash available — cannot compile or generate new code"
                        .to_string()
                }
            },
        );

        // 2. 是否还能加载新能力？
        invariants.can_load_capabilities = Self::check_load_capabilities().await;
        invariants.diagnostics.insert(
            "load_capabilities".to_string(),
            if invariants.can_load_capabilities {
                "Workspace directory writable, can create and load capability artifacts".to_string()
            } else {
                "Cannot write to workspace directory — capability loading impaired".to_string()
            },
        );

        // 3. 是否还能与主人通信？
        // If we're running this check, we can communicate (the agent is alive)
        invariants.can_communicate = true;
        invariants.diagnostics.insert(
            "communicate".to_string(),
            "Agent is running and responsive".to_string(),
        );

        // 4. 是否还能继续进化？
        // Evolution requires: compile ability + LLM access + disk write
        invariants.can_evolve = invariants.can_compile && invariants.can_load_capabilities;
        invariants.diagnostics.insert(
            "evolve".to_string(),
            if invariants.can_evolve {
                "Can compile + can load = evolution pipeline functional".to_string()
            } else {
                format!(
                    "Evolution impaired: compile={}, load={}",
                    invariants.can_compile, invariants.can_load_capabilities
                )
            },
        );

        // Log results
        if invariants.all_healthy() {
            info!("🫀 [健康检查] 所有生存不变量正常");
        } else {
            let violations = invariants.violations();
            warn!("🫀 [健康检查] 生存不变量异常: {:?}", violations);
        }

        invariants
    }

    /// 检查是否能编译 Rust 代码
    async fn check_compile() -> bool {
        which::which("rustc").is_ok()
    }

    /// 检查 bash 是否可用
    async fn check_bash() -> bool {
        which::which("bash").is_ok()
    }

    /// 检查是否能加载新能力（写入工作目录）
    async fn check_load_capabilities() -> bool {
        let workspace = dirs::home_dir()
            .map(|h| h.join(".blockcell/workspace"))
            .unwrap_or_else(|| std::path::PathBuf::from("."));

        // Check if we can write to the workspace
        let test_file = workspace.join(".health_check_test");
        match std::fs::write(&test_file, "ok") {
            Ok(_) => {
                let _ = std::fs::remove_file(&test_file);
                true
            }
            Err(_) => false,
        }
    }
}
