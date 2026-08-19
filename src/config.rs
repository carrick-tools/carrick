use std::{collections::HashSet, io, path::PathBuf};

use serde::{Deserialize, Serialize};

/// Classification + location for a single service.
///
/// In single-service repos a flat `carrick.json` deserializes directly into one
/// of these (with `directory`/`tsconfig`/`include` left empty). In a monorepo,
/// each entry of the top-level `services` array is one of these — see
/// [`Config::load_services`].
#[derive(Debug, Serialize, Deserialize, Default, Clone)]
pub struct Config {
    #[serde(default)]
    #[serde(rename = "serviceName", alias = "name")]
    pub service_name: Option<String>,
    /// Service root directory, relative to the `carrick.json` location.
    /// `None` means the repository root (single-service mode).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub directory: Option<String>,
    /// Path to this service's `tsconfig.json`, relative to `directory`.
    /// `None` lets the sidecar fall back to its default discovery.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tsconfig: Option<String>,
    /// Extra source roots to pull into this service for type/function
    /// resolution (e.g. shared libraries that are copied in at build time).
    /// Relative to the `carrick.json` location.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub include: Vec<String>,
    #[serde(default)]
    #[serde(rename = "internalEnvVars")]
    pub internal_env_vars: HashSet<String>,
    #[serde(default)]
    #[serde(rename = "internalDomains")]
    pub internal_domains: HashSet<String>,
    #[serde(default)]
    #[serde(rename = "externalEnvVars")]
    pub external_env_vars: HashSet<String>,
    #[serde(default)]
    #[serde(rename = "externalDomains")]
    pub external_domains: HashSet<String>,
}

/// File-level shape of `carrick.json`: either a single flat service (the flat
/// fields, captured via `flatten`) or an explicit `services` array for a
/// monorepo. Resolved by [`Config::load_services`].
#[derive(Debug, Deserialize)]
struct RootConfig {
    #[serde(default)]
    services: Vec<Config>,
    #[serde(flatten)]
    flat: Config,
}

impl Config {
    // Get vec of filepaths and create HashSet from json
    pub fn new(file_paths: Vec<PathBuf>) -> Result<Self, std::io::Error> {
        let mut merged_config = Config::default();
        for path in file_paths.iter() {
            let config_content = std::fs::read_to_string(path)?;
            let config: Config = serde_json::from_str(&config_content)
                .map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e))?;

            // Use first non-None service_name encountered
            if merged_config.service_name.is_none() {
                merged_config.service_name = config.service_name;
            }

            merged_config
                .internal_env_vars
                .extend(config.internal_env_vars);
            merged_config
                .internal_domains
                .extend(config.internal_domains);
            merged_config
                .external_env_vars
                .extend(config.external_env_vars);
            merged_config
                .external_domains
                .extend(config.external_domains);
        }

        Ok(merged_config)
    }

    /// Resolve a repo's `carrick.json` file(s) into one [`Config`] per service.
    ///
    /// A flat config (no `services` key) yields a single service rooted at the
    /// repository root. A config with a non-empty `services` array yields one
    /// entry per declared service, each carrying its own `directory`,
    /// `tsconfig`, `include`, and call-classification fields. When `services`
    /// is present, any sibling flat fields are ignored. Multiple input files
    /// are concatenated.
    ///
    /// This is distinct from [`Config::new`], which merges many repos'
    /// classifiers into one for cross-repo analysis.
    pub fn load_services(file_paths: Vec<PathBuf>) -> Result<Vec<Config>, std::io::Error> {
        let mut services = Vec::new();
        for path in file_paths.iter() {
            let content = std::fs::read_to_string(path)?;
            let root: RootConfig = serde_json::from_str(&content)
                .map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e))?;
            if root.services.is_empty() {
                services.push(root.flat);
            } else {
                services.extend(root.services);
            }
        }
        Ok(services)
    }

    pub fn is_internal_call(&self, route: &str) -> bool {
        // Check if route starts with any internal env var
        if route.starts_with("ENV_VAR:") {
            let parts: Vec<&str> = route.split(':').collect();
            if parts.len() >= 2 {
                let env_var = parts[1];
                return self.internal_env_vars.iter().any(|var| var == env_var);
            }
        }

        // Check if route starts with any internal domain
        self.internal_domains
            .iter()
            .any(|domain| route.starts_with(domain))
    }

    /// True when `env_var` is declared in `externalEnvVars`.
    ///
    /// The same set [`Self::is_external_call`] consults for an
    /// `ENV_VAR:<name>:<path>` route, exposed by name so a caller already
    /// holding the variable name does not rebuild the route to ask. Keeping one
    /// implementation matters: a second copy of the membership test is how the
    /// egress channel would come to name a variable the matcher classifies
    /// differently.
    pub fn is_external_env_var(&self, env_var: &str) -> bool {
        self.external_env_vars.contains(env_var)
    }

    pub fn is_external_call(&self, route: &str) -> bool {
        // Check if route starts with any external env var
        if route.starts_with("ENV_VAR:") {
            let parts: Vec<&str> = route.split(':').collect();
            if parts.len() >= 2 {
                return self.is_external_env_var(parts[1]);
            }
        }

        // Check if route starts with any external domain
        self.external_domains
            .iter()
            .any(|domain| route.starts_with(domain))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_config_with_service_name() {
        let json = r#"{
            "serviceName": "order-service",
            "internalEnvVars": ["USER_SERVICE_URL"],
            "externalEnvVars": ["STRIPE_API"]
        }"#;

        let config: Config = serde_json::from_str(json).unwrap();
        assert_eq!(config.service_name, Some("order-service".to_string()));
        assert!(config.internal_env_vars.contains("USER_SERVICE_URL"));
        assert!(config.external_env_vars.contains("STRIPE_API"));
    }

    #[test]
    fn test_is_internal_call() {
        let config = Config {
            internal_env_vars: ["USER_SERVICE_URL".to_string()].into_iter().collect(),
            internal_domains: ["https://api.internal.com".to_string()]
                .into_iter()
                .collect(),
            ..Default::default()
        };

        // ENV_VAR pattern matching
        assert!(config.is_internal_call("ENV_VAR:USER_SERVICE_URL:/users"));
        assert!(!config.is_internal_call("ENV_VAR:UNKNOWN_URL:/users"));

        // Domain matching (route must start with domain)
        assert!(config.is_internal_call("https://api.internal.com/users"));
        assert!(!config.is_internal_call("https://unknown.com/users"));
    }

    #[test]
    fn test_is_external_call() {
        let config = Config {
            external_env_vars: ["STRIPE_API".to_string()].into_iter().collect(),
            external_domains: ["https://api.stripe.com".to_string()].into_iter().collect(),
            ..Default::default()
        };

        // ENV_VAR pattern matching
        assert!(config.is_external_call("ENV_VAR:STRIPE_API:/charges"));
        assert!(!config.is_external_call("ENV_VAR:UNKNOWN_URL:/users"));

        // Domain matching (route must start with domain)
        assert!(config.is_external_call("https://api.stripe.com/charges"));
        assert!(!config.is_external_call("https://unknown.com/users"));
    }

    #[test]
    fn test_flat_config_has_no_directory() {
        // A flat single-service config leaves the monorepo fields empty.
        let json = r#"{
            "serviceName": "order-service",
            "internalEnvVars": ["USER_SERVICE_URL"]
        }"#;
        let config: Config = serde_json::from_str(json).unwrap();
        assert_eq!(config.directory, None);
        assert_eq!(config.tsconfig, None);
        assert!(config.include.is_empty());
    }

    #[test]
    fn test_service_entry_uses_name_alias() {
        // Inside `services`, `name` is accepted as an alias for `serviceName`.
        let json = r#"{ "name": "mcp-server", "directory": "lambdas/mcp-server" }"#;
        let config: Config = serde_json::from_str(json).unwrap();
        assert_eq!(config.service_name, Some("mcp-server".to_string()));
        assert_eq!(config.directory, Some("lambdas/mcp-server".to_string()));
    }

    #[test]
    fn test_load_services_flat_yields_single_service() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("carrick.json");
        std::fs::write(
            &path,
            r#"{ "serviceName": "single", "internalDomains": ["api.internal.com"] }"#,
        )
        .unwrap();

        let services = Config::load_services(vec![path]).unwrap();
        assert_eq!(services.len(), 1);
        assert_eq!(services[0].service_name, Some("single".to_string()));
        assert_eq!(services[0].directory, None);
        assert!(services[0].internal_domains.contains("api.internal.com"));
    }

    #[test]
    fn test_load_services_array_yields_one_per_service() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("carrick.json");
        std::fs::write(
            &path,
            r#"{
                "services": [
                    {
                        "name": "check-or-upload",
                        "directory": "lambdas/check-or-upload",
                        "include": ["lambdas/_shared"],
                        "internalEnvVars": ["CARRICK_API_ENDPOINT"]
                    },
                    {
                        "name": "dashboard",
                        "directory": "app",
                        "tsconfig": "tsconfig.json"
                    }
                ]
            }"#,
        )
        .unwrap();

        let services = Config::load_services(vec![path]).unwrap();
        assert_eq!(services.len(), 2);

        let first = &services[0];
        assert_eq!(first.service_name, Some("check-or-upload".to_string()));
        assert_eq!(first.directory, Some("lambdas/check-or-upload".to_string()));
        assert_eq!(first.include, vec!["lambdas/_shared".to_string()]);
        assert!(first.internal_env_vars.contains("CARRICK_API_ENDPOINT"));

        let second = &services[1];
        assert_eq!(second.service_name, Some("dashboard".to_string()));
        assert_eq!(second.directory, Some("app".to_string()));
        assert_eq!(second.tsconfig, Some("tsconfig.json".to_string()));
    }

    #[test]
    fn test_load_services_empty_array_falls_back_to_flat() {
        // An explicit-but-empty `services` array falls back to the flat fields,
        // so the repo is still treated as one service.
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("carrick.json");
        std::fs::write(&path, r#"{ "serviceName": "flat", "services": [] }"#).unwrap();

        let services = Config::load_services(vec![path]).unwrap();
        assert_eq!(services.len(), 1);
        assert_eq!(services[0].service_name, Some("flat".to_string()));
    }
}
