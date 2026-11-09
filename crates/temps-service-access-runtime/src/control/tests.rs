// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

use super::*;
use ed25519_dalek::SigningKey;
use tokenless_core::audit::AuditEvent;
use tokenless_dataplane::DataPlaneError;

struct Audit;
#[async_trait::async_trait]
impl AuditShipper for Audit {
    async fn ship(&self, _: &[AuditEvent]) -> Result<(), DataPlaneError> {
        Ok(())
    }
}

fn control() -> WorkerControl {
    WorkerControl::new(Arc::new(Audit))
}

fn authority(control: &WorkerControl) -> WorkerAuthority {
    WorkerAuthority {
        boot_id: control.identity().boot_id,
        node_id: Uuid::new_v4(),
        snapshot_verify_key: SigningKey::generate(&mut rand08::rngs::OsRng)
            .verifying_key()
            .to_bytes(),
    }
}

#[test]
fn every_worker_boot_has_distinct_identity_and_sealing_keys() {
    let first = control();
    let second = control();
    assert_ne!(first.identity().boot_id, second.identity().boot_id);
    assert_ne!(
        first.identity().seal_public_key,
        second.identity().seal_public_key
    );
    assert!(!first.identity().boot_id.is_nil());
    assert_eq!(first.identity(), first.identity());
}

#[tokio::test]
async fn unconfigured_worker_refuses_binding_without_starting_a_proxy() {
    let control = control();
    let result = control
        .bind(
            control.identity().boot_id,
            BindingSpec {
                binding_id: "test-incarnation".into(),
                generation: 1,
                sessions: vec![],
                version: 1,
                max_connections: 1,
            },
            1,
        )
        .await;
    assert!(matches!(
        result,
        Err(RuntimeError::Invalid {
            reason: "worker authority is not configured",
            ..
        })
    ));
    assert!(control.initialized.get().is_none());
}

#[tokio::test]
async fn registration_is_idempotent_but_cannot_replace_authority_or_node() {
    let control = control();
    let registration = authority(&control);
    control.configure(registration.clone()).await.unwrap();
    control.configure(registration.clone()).await.unwrap();
    let mut different_node = registration.clone();
    different_node.node_id = Uuid::new_v4();
    assert!(control.configure(different_node).await.is_err());
    let mut different_key = registration.clone();
    different_key.snapshot_verify_key = authority(&control).snapshot_verify_key;
    assert!(control.configure(different_key).await.is_err());
    assert_eq!(control.initialized.get().unwrap().authority, registration);
}

#[tokio::test]
async fn previous_boot_registration_and_commands_cannot_initialize_new_worker() {
    let old = control();
    let new = control();
    let registration = authority(&old);
    assert!(new.configure(registration.clone()).await.is_err());
    assert!(matches!(
        new.initialized(registration.boot_id),
        Err(RuntimeError::Invalid {
            reason: "worker boot changed",
            ..
        })
    ));
    assert!(new.initialized.get().is_none());
}

#[tokio::test]
async fn invalid_authority_does_not_poison_later_valid_registration() {
    let control = control();
    let mut invalid = authority(&control);
    invalid.node_id = Uuid::nil();
    assert!(control.configure(invalid).await.is_err());
    let mut weak = authority(&control);
    weak.snapshot_verify_key = [0; 32];
    assert!(control.configure(weak).await.is_err());
    assert!(control.initialized.get().is_none());
    control.configure(authority(&control)).await.unwrap();
}

#[tokio::test]
async fn concurrent_registrations_pin_exactly_one_authority() {
    let control = control();
    let first = authority(&control);
    let second = authority(&control);
    let (a, b) = tokio::join!(
        control.configure(first.clone()),
        control.configure(second.clone())
    );
    assert_ne!(a.is_ok(), b.is_ok());
    let winner = if a.is_ok() { first } else { second };
    assert_eq!(control.initialized.get().unwrap().authority, winner);
    control.configure(winner).await.unwrap();
}
