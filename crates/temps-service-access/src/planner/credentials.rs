// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0
use super::*;
use std::collections::BTreeSet;

const CONCURRENT_LOOKUPS: usize = 4;
const CREDENTIAL_BUDGET: Duration = Duration::from_secs(3);

impl PolicyPlanner {
    pub(super) async fn resolve_github(
        &self,
        boot_id: Uuid,
        connections: BTreeSet<i32>,
    ) -> BTreeMap<i32, Option<SecretString>> {
        let mut resolved = BTreeMap::new();
        if connections.is_empty() {
            return resolved;
        }
        // Share a bounded overall budget fairly across batches. Every distinct
        // connection gets a fresh window, even after an entire stalled batch.
        // At worker capacity this degrades by omitting timed-out credentials,
        // never by revoking consent or starting unbounded provider requests.
        let batches = connections.len().div_ceil(CONCURRENT_LOOKUPS);
        let budget = CREDENTIAL_BUDGET / batches as u32;
        let mut remaining = connections.into_iter();
        let mut tasks = tokio::task::JoinSet::new();
        loop {
            let deadline = tokio::time::Instant::now() + budget;
            for connection_id in remaining.by_ref().take(CONCURRENT_LOOKUPS) {
                let github = Arc::clone(&self.github);
                tasks.spawn(async move {
                    let credential =
                        tokio::time::timeout_at(deadline, github.resolve(connection_id))
                            .await
                            .ok()
                            .and_then(Result::ok);
                    (connection_id, credential)
                });
            }
            if tasks.is_empty() {
                break;
            }
            while let Some(result) = tasks.join_next().await {
                match result {
                    Ok((connection_id, credential)) => {
                        if credential.is_none() {
                            tracing::warn!(%boot_id, connection_id, "GitHub credential unavailable; omitting affected grants from this lease");
                        }
                        resolved.insert(connection_id, credential);
                    }
                    Err(_) => {
                        // Do not log task errors which may contain provider data.
                        tracing::warn!(%boot_id, "GitHub credential task failed; omitting affected grants from this lease");
                    }
                }
            }
        }
        resolved
    }
}
