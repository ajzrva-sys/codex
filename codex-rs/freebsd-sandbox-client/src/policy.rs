//! Concrete, portable policy inputs. These never confer authority beyond the
//! caller's filesystem access; the privileged service compiles and pins them.
use serde::Deserialize;
use serde::Serialize;
use std::path::Component;
use std::path::PathBuf;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Access {
    Read,
    Write,
    Deny,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Grant {
    pub path: PathBuf,
    pub access: Access,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Network {
    Restricted,
    Enabled,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Policy {
    pub grants: Vec<Grant>,
    pub network: Network,
}

impl Policy {
    /// Encode the existing managed-permission wire subset without importing
    /// Codex application types. Unsupported paths fail before connecting.
    pub fn permissions(&self) -> anyhow::Result<serde_json::Value> {
        let mut entries = vec![serde_json::json!({
            "path": {"type": "special", "value": {"kind": "minimal"}}, "access": "read"
        })];
        for grant in &self.grants {
            anyhow::ensure!(
                grant.path.is_absolute()
                    && grant.path != std::path::Path::new("/")
                    && grant.path.to_str().is_some()
                    && grant
                        .path
                        .components()
                        .all(|part| matches!(part, Component::RootDir | Component::Normal(_))),
                "sandbox grants require concrete absolute non-root paths"
            );
            entries.push(serde_json::json!({"path": {"type": "path", "path": grant.path}, "access": grant.access}));
        }
        Ok(
            serde_json::json!({"type": "managed", "file_system": {"type": "restricted", "entries": entries},
            "network": match self.network { Network::Restricted => "restricted", Network::Enabled => "enabled" }}),
        )
    }
}
