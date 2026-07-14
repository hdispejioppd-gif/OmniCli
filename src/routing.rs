//! Role-based model routing: map agent roles to provider/model pairs
//! with fallback chains.

use std::collections::HashMap;

#[derive(Debug, thiserror::Error)]
pub enum RoutingError {
    #[error("no route for role '{0}' and no default")]
    NoRoute(String),
    #[error("empty fallback chain for role '{0}'")]
    EmptyChain(String),
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Deserialize, serde::Serialize)]
pub struct ModelTarget {
    pub provider: String,
    pub model: String,
}

#[derive(Debug, Clone, Default, serde::Deserialize, serde::Serialize)]
pub struct RoutingConfig {
    #[serde(default)]
    pub default: Vec<ModelTarget>,
    #[serde(default)]
    pub roles: HashMap<String, Vec<ModelTarget>>,
}

pub struct Router {
    config: RoutingConfig,
    cursor: HashMap<String, usize>,
}

impl Router {
    pub fn new(config: RoutingConfig) -> Self { Self { config, cursor: HashMap::new() } }

    fn chain(&self, role: &str) -> Result<&[ModelTarget], RoutingError> {
        let chain = self.config.roles.get(role).map(|v| v.as_slice()).filter(|v| !v.is_empty()).unwrap_or(self.config.default.as_slice());
        if chain.is_empty() { return Err(RoutingError::NoRoute(role.to_string())); }
        Ok(chain)
    }

    pub fn resolve(&self, role: &str) -> Result<ModelTarget, RoutingError> {
        let chain = self.chain(role)?;
        let idx = *self.cursor.get(role).unwrap_or(&0);
        Ok(chain[idx.min(chain.len() - 1)].clone())
    }

    pub fn report_failure(&mut self, role: &str) -> Result<ModelTarget, RoutingError> {
        let chain_len = self.chain(role)?.len();
        let idx = self.cursor.entry(role.to_string()).or_insert(0);
        if *idx + 1 >= chain_len { return Err(RoutingError::EmptyChain(role.to_string())); }
        *idx += 1;
        self.resolve(role)
    }

    pub fn reset(&mut self, role: &str) { self.cursor.remove(role); }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cfg() -> RoutingConfig {
        toml::from_str(r#"
            default = [{ provider = "ollama", model = "llama3.1" }]
            [roles]
            planner = [
                { provider = "anthropic", model = "claude-sonnet-4-20250514" },
                { provider = "openai", model = "gpt-4.1-mini" },
            ]
            executor = [{ provider = "llama-cpp", model = "qwen2.5-7b" }]
        "#).unwrap()
    }

    #[test]
    fn resolves_role_chain() {
        let r = Router::new(cfg());
        assert_eq!(r.resolve("planner").unwrap().provider, "anthropic");
        assert_eq!(r.resolve("executor").unwrap().model, "qwen2.5-7b");
    }

    #[test]
    fn unknown_role_uses_default() {
        let r = Router::new(cfg());
        assert_eq!(r.resolve("reviewer").unwrap().provider, "ollama");
    }

    #[test]
    fn failure_advances_to_fallback() {
        let mut r = Router::new(cfg());
        let next = r.report_failure("planner").unwrap();
        assert_eq!(next.provider, "openai");
        assert_eq!(r.resolve("planner").unwrap().provider, "openai");
    }

    #[test]
    fn exhausted_chain_errors() {
        let mut r = Router::new(cfg());
        assert!(r.report_failure("executor").is_err());
    }

    #[test]
    fn empty_config_errors() {
        let r = Router::new(RoutingConfig::default());
        assert!(r.resolve("planner").is_err());
    }

    #[test]
    fn reset_restores_primary() {
        let mut r = Router::new(cfg());
        r.report_failure("planner").unwrap();
        r.reset("planner");
        assert_eq!(r.resolve("planner").unwrap().provider, "anthropic");
    }
}