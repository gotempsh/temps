// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! How a settings write stores Shield modes (ADR 066 D1, I10).
//!
//! `shield_mode` replaces `attack_mode`, but the API keeps writing both for
//! one release so a console that only knows attack mode keeps reading and
//! writing the right thing: a mode of `attack` means `attack_mode = true`,
//! any other mode `false`, and an environment that inherits one inherits
//! both. A write that only names the legacy `attack_mode` is mapped onto a
//! mode the same way.

use temps_shield::ShieldMode;

/// What a project settings write stores, or `None` when it leaves both
/// columns alone. `current` is the project's mode as stored.
///
/// `requested_mode` wins over `requested_attack` when both are sent.
pub fn project_shield_write(
    requested_mode: Option<ShieldMode>,
    requested_attack: Option<bool>,
    current: ShieldMode,
) -> Option<(ShieldMode, bool)> {
    let mode = match (requested_mode, requested_attack) {
        (Some(mode), _) => mode,
        (None, Some(true)) => ShieldMode::Attack,
        // Turning attack mode off goes back to the default; any other mode
        // already is "not attack" and stays.
        (None, Some(false)) if current == ShieldMode::Attack => ShieldMode::Log,
        (None, Some(false)) => current,
        (None, None) => return None,
    };
    Some((mode, mode == ShieldMode::Attack))
}

/// What an environment settings write stores for `(shield_mode,
/// attack_mode)`, `None` meaning inherit, or `None` overall when it leaves
/// both columns alone.
///
/// `current` is the environment's own mode, `project` the mode its project
/// runs in; it is only consulted for a legacy `attack_mode: false`, which
/// must keep an environment out of attack even when its project is in it.
pub fn environment_shield_write(
    requested_mode: Option<Option<ShieldMode>>,
    requested_attack: Option<Option<bool>>,
    current: Option<ShieldMode>,
    project: impl FnOnce() -> ShieldMode,
) -> Option<(Option<ShieldMode>, Option<bool>)> {
    let mode = match (requested_mode, requested_attack) {
        (Some(mode), _) => mode,
        (None, Some(None)) => None,
        (None, Some(Some(true))) => Some(ShieldMode::Attack),
        (None, Some(Some(false))) => match current {
            Some(ShieldMode::Attack) => Some(ShieldMode::Log),
            Some(mode) => Some(mode),
            None if project() == ShieldMode::Attack => Some(ShieldMode::Log),
            // Still inherits the mode, with attack explicitly off as asked.
            None => return Some((None, Some(false))),
        },
        (None, None) => return None,
    };
    Some((mode, mode.map(|mode| mode == ShieldMode::Attack)))
}

#[cfg(test)]
mod tests {
    use super::*;
    use ShieldMode::{Attack, Log, Off, Selective};

    #[test]
    fn a_project_mode_writes_attack_mode_to_match() {
        assert_eq!(
            project_shield_write(Some(Attack), None, Log),
            Some((Attack, true))
        );
        assert_eq!(
            project_shield_write(Some(Selective), None, Attack),
            Some((Selective, false))
        );
        // The mode wins over a legacy flag sent alongside it.
        assert_eq!(
            project_shield_write(Some(Off), Some(true), Log),
            Some((Off, false))
        );
        assert_eq!(project_shield_write(None, None, Selective), None);
    }

    #[test]
    fn a_legacy_project_attack_flag_maps_onto_a_mode() {
        assert_eq!(
            project_shield_write(None, Some(true), Selective),
            Some((Attack, true))
        );
        assert_eq!(
            project_shield_write(None, Some(false), Attack),
            Some((Log, false))
        );
        // Turning attack off on a project that is not in attack keeps its mode.
        assert_eq!(
            project_shield_write(None, Some(false), Selective),
            Some((Selective, false))
        );
    }

    #[test]
    fn an_environment_mode_or_inherit_writes_attack_mode_to_match() {
        let unused = || panic!("the project's mode is not needed here");
        assert_eq!(
            environment_shield_write(Some(Some(Attack)), None, None, unused),
            Some((Some(Attack), Some(true)))
        );
        assert_eq!(
            environment_shield_write(Some(Some(Log)), Some(Some(true)), None, unused),
            Some((Some(Log), Some(false)))
        );
        assert_eq!(
            environment_shield_write(Some(None), None, Some(Selective), unused),
            Some((None, None))
        );
        assert_eq!(
            environment_shield_write(None, None, Some(Off), unused),
            None
        );
    }

    #[test]
    fn a_legacy_environment_attack_flag_maps_onto_a_mode() {
        let unused = || panic!("the project's mode is not needed here");
        assert_eq!(
            environment_shield_write(None, Some(Some(true)), None, unused),
            Some((Some(Attack), Some(true)))
        );
        assert_eq!(
            environment_shield_write(None, Some(None), Some(Attack), unused),
            Some((None, None))
        );
        assert_eq!(
            environment_shield_write(None, Some(Some(false)), Some(Attack), unused),
            Some((Some(Log), Some(false)))
        );
        assert_eq!(
            environment_shield_write(None, Some(Some(false)), Some(Selective), unused),
            Some((Some(Selective), Some(false)))
        );
        // An inheriting environment of a project in attack mode must stop
        // inheriting, or it would stay in attack.
        assert_eq!(
            environment_shield_write(None, Some(Some(false)), None, || Attack),
            Some((Some(Log), Some(false)))
        );
        assert_eq!(
            environment_shield_write(None, Some(Some(false)), None, || Selective),
            Some((None, Some(false)))
        );
    }

    /// Whatever is written, the mode an ingress computes from the stored
    /// columns is the one asked for.
    #[test]
    fn the_written_columns_give_the_requested_mode() {
        for mode in ShieldMode::ALL {
            let (stored, attack) = project_shield_write(Some(mode), None, Log).unwrap();
            assert_eq!(
                ShieldMode::effective(None, stored.as_str(), None, attack),
                mode
            );
            for project in ShieldMode::ALL {
                let (env_mode, env_attack) =
                    environment_shield_write(Some(Some(mode)), None, None, || project).unwrap();
                assert_eq!(
                    ShieldMode::effective(
                        env_mode.map(ShieldMode::as_str),
                        project.as_str(),
                        env_attack,
                        project == Attack
                    ),
                    mode
                );
            }
        }
    }
}
