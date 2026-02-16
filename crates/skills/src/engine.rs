use blockcell_core::{Error, Result};
use rhai::{Engine, AST, Scope, Dynamic, EvalAltResult};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};
use tracing::{debug, warn};

const DEFAULT_MAX_OPERATIONS: u64 = 100_000;
const DEFAULT_TIMEOUT_SECS: u64 = 30;

#[derive(Debug, Clone)]
pub struct EngineConfig {
    pub max_operations: u64,
    pub timeout_secs: u64,
    pub max_string_size: usize,
    pub max_array_size: usize,
    pub max_map_size: usize,
    pub max_call_stack_depth: usize,
}

impl Default for EngineConfig {
    fn default() -> Self {
        Self {
            max_operations: DEFAULT_MAX_OPERATIONS,
            timeout_secs: DEFAULT_TIMEOUT_SECS,
            max_string_size: 1_000_000,
            max_array_size: 10_000,
            max_map_size: 10_000,
            max_call_stack_depth: 64,
        }
    }
}

pub struct RhaiEngine {
    config: EngineConfig,
}

impl RhaiEngine {
    pub fn new(config: EngineConfig) -> Self {
        Self { config }
    }

    fn create_engine(&self) -> Engine {
        let mut engine = Engine::new();

        // Set limits
        engine.set_max_string_size(self.config.max_string_size);
        engine.set_max_array_size(self.config.max_array_size);
        engine.set_max_map_size(self.config.max_map_size);
        engine.set_max_call_levels(self.config.max_call_stack_depth);

        // Set expression depth limits
        engine.set_max_expr_depths(64, 64);

        engine
    }

    fn create_engine_with_limits(&self) -> (Engine, Arc<AtomicU64>, Instant) {
        let mut engine = self.create_engine();

        let operations = Arc::new(AtomicU64::new(0));
        let start_time = Instant::now();
        let max_ops = self.config.max_operations;
        let timeout = Duration::from_secs(self.config.timeout_secs);

        let ops_counter = operations.clone();

        engine.on_progress(move |_| {
            let count = ops_counter.fetch_add(1, Ordering::Relaxed);

            // Check operation limit
            if count >= max_ops {
                return Some(Dynamic::from(format!(
                    "Operation limit exceeded: {} operations",
                    max_ops
                )));
            }

            // Check timeout
            if start_time.elapsed() > timeout {
                return Some(Dynamic::from(format!(
                    "Timeout exceeded: {} seconds",
                    timeout.as_secs()
                )));
            }

            None
        });

        (engine, operations, start_time)
    }

    pub fn compile(&self, script: &str) -> Result<AST> {
        let engine = self.create_engine();
        engine
            .compile(script)
            .map_err(|e| Error::Skill(format!("Compilation error: {}", e)))
    }

    pub fn run(&self, ast: &AST, scope: &mut Scope) -> Result<Dynamic> {
        let (engine, operations, start_time) = self.create_engine_with_limits();

        let result = engine.run_ast_with_scope(scope, ast);

        let final_ops = operations.load(Ordering::Relaxed);
        let elapsed = start_time.elapsed();

        debug!(
            operations = final_ops,
            elapsed_ms = elapsed.as_millis(),
            "Rhai script execution completed"
        );

        match result {
            Ok(_) => Ok(Dynamic::UNIT),
            Err(e) => {
                if let EvalAltResult::ErrorTerminated(ref reason, _) = *e {
                    warn!(reason = %reason, "Script terminated");
                    return Err(Error::Skill(format!("Script terminated: {}", reason)));
                }
                Err(Error::Skill(format!("Runtime error: {}", e)))
            }
        }
    }

    pub fn eval(&self, ast: &AST, scope: &mut Scope) -> Result<Dynamic> {
        let (engine, operations, start_time) = self.create_engine_with_limits();

        let result = engine.eval_ast_with_scope::<Dynamic>(scope, ast);

        let final_ops = operations.load(Ordering::Relaxed);
        let elapsed = start_time.elapsed();

        debug!(
            operations = final_ops,
            elapsed_ms = elapsed.as_millis(),
            "Rhai script evaluation completed"
        );

        match result {
            Ok(value) => Ok(value),
            Err(e) => {
                if let EvalAltResult::ErrorTerminated(ref reason, _) = *e {
                    warn!(reason = %reason, "Script terminated");
                    return Err(Error::Skill(format!("Script terminated: {}", reason)));
                }
                Err(Error::Skill(format!("Runtime error: {}", e)))
            }
        }
    }

    pub fn eval_expression(&self, expr: &str, scope: &mut Scope) -> Result<Dynamic> {
        let engine = self.create_engine();
        let ast = engine
            .compile_expression(expr)
            .map_err(|e| Error::Skill(format!("Expression compilation error: {}", e)))?;

        self.eval(&ast, scope)
    }
}

impl Default for RhaiEngine {
    fn default() -> Self {
        Self::new(EngineConfig::default())
    }
}

#[derive(Debug, Clone)]
pub struct ExecutionResult {
    pub value: Dynamic,
    pub operations: u64,
    pub elapsed_ms: u64,
}

pub struct SkillExecutor {
    engine: RhaiEngine,
}

impl SkillExecutor {
    pub fn new(config: EngineConfig) -> Self {
        Self {
            engine: RhaiEngine::new(config),
        }
    }

    pub fn execute_script(&self, script: &str, variables: Vec<(&str, Dynamic)>) -> Result<ExecutionResult> {
        let start = Instant::now();

        let ast = self.engine.compile(script)?;

        let mut scope = Scope::new();
        for (name, value) in variables {
            scope.push(name, value);
        }

        let value = self.engine.eval(&ast, &mut scope)?;

        Ok(ExecutionResult {
            value,
            operations: 0, // Would need to track this differently
            elapsed_ms: start.elapsed().as_millis() as u64,
        })
    }

    pub fn execute_file(&self, path: &std::path::Path, variables: Vec<(&str, Dynamic)>) -> Result<ExecutionResult> {
        let script = std::fs::read_to_string(path)
            .map_err(|e| Error::Skill(format!("Failed to read script file: {}", e)))?;

        self.execute_script(&script, variables)
    }
}

impl Default for SkillExecutor {
    fn default() -> Self {
        Self::new(EngineConfig::default())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_simple_script() {
        let executor = SkillExecutor::default();
        let result = executor.execute_script("let x = 1 + 2; x", vec![]).unwrap();
        assert_eq!(result.value.as_int().unwrap(), 3);
    }

    #[test]
    fn test_with_variables() {
        let executor = SkillExecutor::default();
        let result = executor
            .execute_script("a + b", vec![("a", Dynamic::from(10_i64)), ("b", Dynamic::from(20_i64))])
            .unwrap();
        assert_eq!(result.value.as_int().unwrap(), 30);
    }

    #[test]
    fn test_operation_limit() {
        let config = EngineConfig {
            max_operations: 100,
            ..Default::default()
        };
        let executor = SkillExecutor::new(config);

        // This should exceed the operation limit
        let result = executor.execute_script(
            r#"
            let sum = 0;
            for i in 0..10000 {
                sum += i;
            }
            sum
            "#,
            vec![],
        );

        assert!(result.is_err());
        let err = result.unwrap_err().to_string();
        assert!(err.contains("Operation limit") || err.contains("terminated"));
    }

    #[test]
    fn test_compilation_error() {
        let executor = SkillExecutor::default();
        let result = executor.execute_script("let x = ", vec![]);
        assert!(result.is_err());
    }
}
