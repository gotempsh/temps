//! Envelope encryption for sensitive maps stored in `deployment_jobs.job_config`.
//!
//! ### Why this exists
//! The workflow planner gathers a deployment's resolved env vars + secrets and
//! writes them into `deployment_jobs.job_config` (a JSON column) so the
//! executor can pick them up later, possibly on a different node. Until now
//! that JSON contained the values in plaintext — a copy of every env var and
//! every secret for the deployment, sitting in the database.
//!
//! That is a poor place for plaintext: database backups, logical dumps,
//! support copies of `deployment_jobs`, and anyone with read access on that
//! one table all become credential exfiltration vectors.
//!
//! ### What this does
//! Each sensitive map (`environment_variables`, `remote_environment_variables`,
//! `secrets`, `build_args`) is serialized to JSON, encrypted as a single blob
//! with the platform `EncryptionService` (AES-256-GCM, key in
//! `~/.temps/encryption_key`), and stored under a `*_encrypted` key.
//!
//! The job config also stores `*_keys` — the *list of keys* without their
//! values — purely so an operator dumping `job_config` for debugging can see
//! which vars exist without seeing what they contain. Useful for ops, not
//! load-bearing for execution.
//!
//! ### Compatibility
//! Old-format rows that still have a plaintext `environment_variables` /
//! `secrets` / `build_args` key continue to work — the reader falls back to
//! the plaintext field when the encrypted one is absent. New writes always
//! produce the encrypted form. There is no plaintext-and-encrypted dual
//! write.

use std::collections::HashMap;
use std::sync::Arc;

use serde_json::Value;
use temps_core::EncryptionService;
use thiserror::Error;

#[derive(Debug, Error)]
pub enum SensitiveEnvelopeError {
    #[error("Failed to serialize sensitive map: {0}")]
    Serialize(#[from] serde_json::Error),

    #[error("Failed to encrypt sensitive map: {0}")]
    Encrypt(String),

    #[error("Failed to decrypt sensitive map at field '{field}': {reason}")]
    Decrypt { field: String, reason: String },
}

/// Encrypts `map` into a JSON string suitable for storage. The output is the
/// AES-256-GCM ciphertext (base64) of the JSON-serialized map.
pub fn seal_map(
    enc: &EncryptionService,
    map: &HashMap<String, String>,
) -> Result<String, SensitiveEnvelopeError> {
    let json = serde_json::to_string(map)?;
    enc.encrypt_string(&json)
        .map_err(|e| SensitiveEnvelopeError::Encrypt(e.to_string()))
}

/// Opposite of `seal_map`.
pub fn open_map(
    enc: &EncryptionService,
    field: &str,
    ciphertext: &str,
) -> Result<HashMap<String, String>, SensitiveEnvelopeError> {
    let json = enc
        .decrypt_string(ciphertext)
        .map_err(|e| SensitiveEnvelopeError::Decrypt {
            field: field.to_string(),
            reason: e.to_string(),
        })?;
    serde_json::from_str(&json).map_err(SensitiveEnvelopeError::Serialize)
}

/// Writes a sensitive map into a job_config JSON object as a sealed ciphertext
/// + a `*_keys` array. Does not write the plaintext map under any name.
pub fn write_sealed(
    job_config: &mut serde_json::Map<String, Value>,
    enc: &EncryptionService,
    field: &str,
    map: &HashMap<String, String>,
) -> Result<(), SensitiveEnvelopeError> {
    if map.is_empty() {
        return Ok(());
    }
    let sealed = seal_map(enc, map)?;
    job_config.insert(format!("{}_encrypted", field), Value::String(sealed));
    let mut keys: Vec<String> = map.keys().cloned().collect();
    keys.sort();
    job_config.insert(
        format!("{}_keys", field),
        Value::Array(keys.into_iter().map(Value::String).collect()),
    );
    Ok(())
}

/// Reads a sensitive map from job_config: first the encrypted form, falling
/// back to a legacy plaintext map (for in-flight jobs queued before the
/// migration). Missing field returns an empty map.
pub fn read_sealed(
    job_config: &Value,
    enc: Option<&Arc<EncryptionService>>,
    field: &str,
) -> Result<HashMap<String, String>, SensitiveEnvelopeError> {
    let encrypted_field = format!("{}_encrypted", field);

    if let Some(ct) = job_config.get(&encrypted_field).and_then(|v| v.as_str()) {
        let enc = enc.ok_or_else(|| SensitiveEnvelopeError::Decrypt {
            field: field.to_string(),
            reason: "EncryptionService not configured on workflow executor".to_string(),
        })?;
        return open_map(enc.as_ref(), field, ct);
    }

    // Legacy plaintext fallback: previously the planner stored
    // `environment_variables: { KEY: VALUE, ... }` directly. Old queued jobs
    // can still execute; new writes never produce this shape.
    if let Some(obj) = job_config.get(field).and_then(|v| v.as_object()) {
        let mut out = HashMap::with_capacity(obj.len());
        for (k, v) in obj {
            if let Some(s) = v.as_str() {
                out.insert(k.clone(), s.to_string());
            }
        }
        return Ok(out);
    }

    Ok(HashMap::new())
}

/// Optional variant of `read_sealed` — returns `None` when neither the
/// encrypted nor the legacy plaintext field is present, so callers that
/// distinguish "no remote vars configured" from "empty map" keep their
/// semantics.
pub fn read_sealed_optional(
    job_config: &Value,
    enc: Option<&Arc<EncryptionService>>,
    field: &str,
) -> Result<Option<HashMap<String, String>>, SensitiveEnvelopeError> {
    let encrypted_field = format!("{}_encrypted", field);
    let has_encrypted = job_config.get(&encrypted_field).is_some();
    let has_plaintext = job_config.get(field).is_some();

    if !has_encrypted && !has_plaintext {
        return Ok(None);
    }

    read_sealed(job_config, enc, field).map(Some)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn make_enc() -> Arc<EncryptionService> {
        Arc::new(
            EncryptionService::new(
                "0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef",
            )
            .unwrap(),
        )
    }

    #[test]
    fn seal_and_open_roundtrip() {
        let enc = make_enc();
        let mut map = HashMap::new();
        map.insert("DB_PASSWORD".to_string(), "hunter2".to_string());
        map.insert("API_KEY".to_string(), "sk-live-x".to_string());

        let sealed = seal_map(&enc, &map).unwrap();
        // Sealed blob must not contain any plaintext value
        assert!(!sealed.contains("hunter2"));
        assert!(!sealed.contains("sk-live-x"));

        let opened = open_map(&enc, "environment_variables", &sealed).unwrap();
        assert_eq!(opened, map);
    }

    #[test]
    fn write_sealed_redacts_plaintext_from_job_config() {
        let enc = make_enc();
        let mut config = serde_json::Map::new();
        let mut map = HashMap::new();
        map.insert("DB_PASSWORD".to_string(), "hunter2".to_string());

        write_sealed(&mut config, &enc, "environment_variables", &map).unwrap();

        let json = serde_json::to_string(&config).unwrap();
        // Plaintext value never appears in the serialized job_config.
        assert!(!json.contains("hunter2"));
        // The ciphertext + key list are both present.
        assert!(config.contains_key("environment_variables_encrypted"));
        let keys = config.get("environment_variables_keys").unwrap();
        assert_eq!(keys.as_array().unwrap().len(), 1);
    }

    #[test]
    fn read_sealed_falls_back_to_legacy_plaintext() {
        let config = serde_json::json!({
            "environment_variables": { "PORT": "3000", "HOST": "0.0.0.0" }
        });
        let map = read_sealed(&config, None, "environment_variables").unwrap();
        assert_eq!(map.get("PORT").map(String::as_str), Some("3000"));
        assert_eq!(map.get("HOST").map(String::as_str), Some("0.0.0.0"));
    }

    #[test]
    fn read_sealed_optional_distinguishes_missing_from_empty() {
        let config = serde_json::json!({});
        let map = read_sealed_optional(&config, None, "remote_environment_variables").unwrap();
        assert!(map.is_none());

        let config2 = serde_json::json!({ "remote_environment_variables": {} });
        let map2 = read_sealed_optional(&config2, None, "remote_environment_variables").unwrap();
        assert_eq!(map2, Some(HashMap::new()));
    }

    #[test]
    fn read_sealed_empty_when_field_absent() {
        let config = serde_json::json!({});
        let map = read_sealed(&config, None, "environment_variables").unwrap();
        assert!(map.is_empty());
    }
}
