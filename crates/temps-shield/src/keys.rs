// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

/// The key generation new tokens are signed with. Raising it in a release
/// rotates the keys; tokens and clearances signed with the previous
/// generation stay valid until they expire.
pub(crate) const CURRENT_KID: u8 = 1;

#[derive(Clone)]
pub(crate) struct KeyGeneration {
    pub(crate) kid: u8,
    pub(crate) challenge: [u8; 32],
    pub(crate) clearance: [u8; 32],
}

/// Signing keys for challenge tokens and clearances.
///
/// The keys are derived from the control plane's master key (see
/// `EncryptionService::derive_subkey`), one per purpose and generation, so
/// there is nothing to store or distribute separately, and every process
/// that holds the master key agrees on them.
#[derive(Clone)]
pub struct ShieldKeys {
    current: KeyGeneration,
    previous: Option<KeyGeneration>,
}

impl ShieldKeys {
    /// Derives the current and previous key generations with `derive`, which
    /// maps a domain-separation label to a 32-byte key.
    pub fn derive(derive: impl Fn(&str) -> [u8; 32]) -> Self {
        let generation = |kid: u8| KeyGeneration {
            kid,
            challenge: derive(&format!("temps-shield/challenge/k{kid}")),
            clearance: derive(&format!("temps-shield/clearance/k{kid}")),
        };
        Self {
            current: generation(CURRENT_KID),
            previous: CURRENT_KID
                .checked_sub(1)
                .filter(|&kid| kid > 0)
                .map(generation),
        }
    }

    pub(crate) fn current(&self) -> &KeyGeneration {
        &self.current
    }

    pub(crate) fn by_kid(&self, kid: u8) -> Option<&KeyGeneration> {
        if kid == self.current.kid {
            return Some(&self.current);
        }
        self.previous
            .as_ref()
            .filter(|previous| previous.kid == kid)
    }
}

impl std::fmt::Debug for ShieldKeys {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ShieldKeys")
            .field("current_kid", &self.current.kid)
            .finish_non_exhaustive()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn purposes_get_different_keys() {
        let keys = ShieldKeys::derive(|label| {
            let mut key = [0u8; 32];
            for (slot, byte) in key.iter_mut().zip(label.bytes().rev()) {
                *slot = byte;
            }
            key
        });
        assert_ne!(keys.current().challenge, keys.current().clearance);
        assert!(keys.by_kid(CURRENT_KID).is_some());
        assert!(keys.by_kid(CURRENT_KID.wrapping_add(1)).is_none());
    }
}
