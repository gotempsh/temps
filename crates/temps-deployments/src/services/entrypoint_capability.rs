// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! "The image's entrypoint needs a Linux capability": an image whose
//! entrypoint was marked with `setcap` (to listen on port 80 as a non-root
//! user) cannot start under `cap_drop: ALL`. The kernel refuses to exec a
//! binary whose file capabilities are not in the bounding set, with EPERM,
//! and the container crash-loops with "Operation not permitted".
//!
//! The deploy job recognises that exec failure in the container's output
//! ([`detect_exec_eperm`]), reads the binary's `security.capability` to see
//! which capabilities it carries, and records the result as a Temps-written
//! annotation right after the fixed failure prefix
//! ([`EntrypointCapabilityFailure::annotate`]). The failure classifier
//! reads only that annotation ([`parse_annotation`]), never the container
//! output, so nothing the image prints can produce or alter the
//! classification. The binary path, which comes from the image, appears in
//! the job log only, never in the persisted failure reason.

use serde::{Deserialize, Serialize};
use temps_workload::{ContainerCapability, FileCapabilities};
use utoipa::ToSchema;

/// Failure prefixes the annotation follows, as the deploy job writes them.
const FAILURE_PREFIXES: [&str; 2] = [
    "Container exited during startup",
    "Container keeps restarting during startup",
];

const MARKER: &str = "[entrypoint_needs_capability: ";

/// Only the end of the output can hold the exec failure.
const MAX_SCANNED_LINES: usize = 200;
const MAX_PATH_BYTES: usize = 4096;
const MAX_CAPABILITY_NAME: usize = 32;
const MAX_CAPABILITIES: usize = 64;

/// Exit codes the exec failure produces: tini (`init: true`, the Temps
/// default) exits 1, a shell exits 126, runc reports 255.
const TINI_EXIT_CODES: [i32; 2] = [1, 126];
const RUNC_EXIT_CODES: [i32; 2] = [126, 255];
const SHELL_EXIT_CODE: i32 = 126;

fn plausible_path(path: &str) -> Option<&str> {
    let path = path.trim();
    (path.starts_with('/') && path.len() <= MAX_PATH_BYTES && !path.chars().any(char::is_control))
        .then_some(path)
}

/// The absolute path of the binary whose exec failed with EPERM, when the
/// container output shows that and the exit code matches the program that
/// printed it. Other 126 exits (EACCES "Permission denied", a missing
/// interpreter) and other crashes are not matched.
pub fn detect_exec_eperm(exit_code: Option<i32>, logs: &str) -> Option<String> {
    let exit_code = exit_code?;
    let lines: Vec<&str> = logs.lines().collect();
    let start = lines.len().saturating_sub(MAX_SCANNED_LINES);
    lines[start..]
        .iter()
        .rev()
        .find_map(|line| exec_eperm_path(exit_code, line.trim()))
        .map(str::to_string)
}

fn exec_eperm_path(exit_code: i32, line: &str) -> Option<&str> {
    // tini: `[FATAL tini (7)] exec /app/server failed: Operation not permitted`
    if TINI_EXIT_CODES.contains(&exit_code) {
        if let Some(rest) = line
            .strip_prefix("[FATAL tini (")
            .and_then(|rest| rest.split_once(")] exec "))
            .map(|(_, rest)| rest)
        {
            if let Some(path) = rest.strip_suffix(" failed: Operation not permitted") {
                return plausible_path(path);
            }
        }
    }
    // runc: `exec /app/server: operation not permitted`
    if RUNC_EXIT_CODES.contains(&exit_code) {
        if let Some(path) = line
            .strip_prefix("exec ")
            .and_then(|rest| rest.strip_suffix(": operation not permitted"))
        {
            return plausible_path(path);
        }
    }
    // A shell exec: `/app/start.sh: line 9: /app/server: Operation not permitted`
    // or `sh: /app/server: Operation not permitted`.
    if exit_code == SHELL_EXIT_CODE {
        let before = line.strip_suffix(": Operation not permitted")?;
        let path = before.rsplit(": ").next()?;
        return plausible_path(path);
    }
    None
}

/// What the deploy job found: the binary, the capabilities it carries, and
/// how sure Temps is of them.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EntrypointCapabilityFailure {
    /// The binary whose exec failed, from the container output. For the job
    /// log only.
    pub binary: String,
    /// Kernel names without `CAP_`, in bit order.
    pub required: Vec<String>,
    /// Read from the binary (true) or inferred because Temps could not
    /// read its file capabilities (false).
    pub verified: bool,
}

impl EntrypointCapabilityFailure {
    /// Combine the exec failure with what reading the binary's file
    /// capabilities returned. `Ok(None)` (no file capabilities) means the
    /// EPERM has another cause, so it is not this failure. When they could
    /// not be read, `NET_BIND_SERVICE` is inferred: it is what `setcap` on an
    /// entrypoint is nearly always for.
    pub fn from_probe<E>(
        binary: String,
        probe: Result<Option<FileCapabilities>, E>,
    ) -> Option<Self> {
        match probe {
            Ok(Some(capabilities)) if !capabilities.permitted.is_empty() => Some(Self {
                binary,
                required: capabilities.permitted,
                verified: true,
            }),
            Ok(_) => None,
            Err(_) => Some(Self {
                binary,
                required: vec![ContainerCapability::NetBindService.as_str().to_string()],
                verified: false,
            }),
        }
    }

    /// Whether every required capability is one this project already allows:
    /// then the setting is not the fix, and the failure is not annotated.
    pub fn already_allowed(&self, allowed: &[ContainerCapability]) -> bool {
        self.required.iter().all(|name| {
            ContainerCapability::parse(name).is_ok_and(|capability| allowed.contains(&capability))
        })
    }

    fn annotation(&self) -> String {
        format!(
            "{MARKER}{} {}]",
            self.required.join(","),
            if self.verified {
                "verified"
            } else {
                "inferred"
            }
        )
    }

    /// Put the annotation right after the fixed failure prefix, ahead of any
    /// text Docker or the image supplied. A message without a known prefix is
    /// returned unchanged.
    pub fn annotate(&self, message: String) -> String {
        match FAILURE_PREFIXES
            .iter()
            .find(|prefix| message.starts_with(*prefix))
        {
            Some(prefix) => format!("{prefix} {}{}", self.annotation(), &message[prefix.len()..]),
            None => message,
        }
    }

    /// The one deployment log line that explains the failure and both fixes.
    pub fn log_line(&self) -> String {
        let fix = SuggestedFix::new(&self.required, self.verified, None);
        let names = self.required.join(", ");
        let noun = if self.required.len() == 1 {
            "capability"
        } else {
            "capabilities"
        };
        let image_fix = format!(
            "remove {} from the image (`setcap -r {}`) and listen on a port above 1024",
            if self.required.len() == 1 {
                "it"
            } else {
                "them"
            },
            self.binary
        );
        let needs = if self.verified {
            format!("needs the Linux {noun} {names} (set with setcap)")
        } else {
            format!(
                "most likely needs the Linux {noun} {names} (set with setcap; Temps could not read the binary's file capabilities to confirm)"
            )
        };
        if fix.allow_capabilities.is_empty() {
            format!(
                "The image's entrypoint {} {needs}, which Temps removes and no project can allow. To start it, {image_fix}.",
                self.binary
            )
        } else {
            format!(
                "The image's entrypoint {} {needs}, which Temps removes by default. Allow it for this project and redeploy, or {image_fix}.",
                self.binary
            )
        }
    }
}

/// The capabilities named by the annotation on a lower-cased failure body
/// (what the classifier holds), and whether they were verified. Only an
/// annotation directly after a known prefix counts.
pub fn parse_annotation(lower_body: &str) -> Option<(Vec<String>, bool)> {
    let label = FAILURE_PREFIXES.iter().find_map(|prefix| {
        lower_body
            .strip_prefix(prefix.to_lowercase().as_str())?
            .strip_prefix(' ')?
            .strip_prefix(MARKER)
    })?;
    let (label, _) = label.split_once(']')?;
    let (names, evidence) = label.split_once(' ')?;
    let verified = match evidence {
        "verified" => true,
        "inferred" => false,
        _ => return None,
    };
    let names: Vec<String> = names
        .split(',')
        .map(|name| {
            (!name.is_empty()
                && name.len() <= MAX_CAPABILITY_NAME
                && name
                    .bytes()
                    .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'_'))
            .then(|| name.to_ascii_uppercase())
        })
        .collect::<Option<_>>()?;
    (!names.is_empty() && names.len() <= MAX_CAPABILITIES).then_some((names, verified))
}

/// How to fix a deployment whose entrypoint needs a Linux capability. Temps
/// never applies it on its own: a person, or an agent they confirmed,
/// changes the setting and redeploys.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct SuggestedFix {
    /// Capabilities the entrypoint binary carries, kernel names without
    /// `CAP_` (e.g. `NET_BIND_SERVICE`).
    pub required_capabilities: Vec<String>,
    /// Whether Temps read them from the binary (true) or inferred them
    /// because it could not read its file capabilities (false).
    pub verified: bool,
    /// What to allow in the project's container security setting to fix it.
    /// Empty when the binary needs a capability no project can allow.
    pub allow_capabilities: Vec<ContainerCapability>,
    /// Required capabilities no project can allow; only changing the image
    /// fixes those.
    pub not_allowed_capabilities: Vec<String>,
    /// The project setting that fixes it.
    pub setting: String,
    /// The API path that reads and changes the setting, when the project is
    /// known (`/projects/{id}/container-security`).
    pub setting_path: Option<String>,
    /// The fix in the image instead, which keeps full hardening.
    pub image_fix: String,
}

impl SuggestedFix {
    pub const SETTING: &'static str = "allowed_capabilities";

    pub fn new(required: &[String], verified: bool, project_id: Option<i32>) -> Self {
        let parsed: Vec<Result<ContainerCapability, &String>> = required
            .iter()
            .map(|name| ContainerCapability::parse(name).map_err(|_| name))
            .collect();
        let not_allowed: Vec<String> = parsed
            .iter()
            .filter_map(|r| r.as_ref().err().map(|name| (*name).clone()))
            .collect();
        // Allowing only some of what the binary needs would not let it start.
        let allow = if not_allowed.is_empty() {
            let mut allow: Vec<ContainerCapability> = parsed
                .iter()
                .filter_map(|r| r.as_ref().ok().copied())
                .collect();
            allow.sort();
            allow.dedup();
            allow
        } else {
            Vec::new()
        };
        Self {
            required_capabilities: required.to_vec(),
            verified,
            allow_capabilities: allow,
            not_allowed_capabilities: not_allowed,
            setting: Self::SETTING.to_string(),
            setting_path: project_id.map(|id| format!("/projects/{id}/container-security")),
            image_fix: "Remove the file capabilities from the entrypoint binary in the image (`setcap -r <binary>`) and listen on a port above 1024.".to_string(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn nbs_caps() -> FileCapabilities {
        FileCapabilities {
            permitted: vec!["NET_BIND_SERVICE".to_string()],
            effective: true,
        }
    }

    #[test]
    fn the_exec_failure_is_recognised_from_tini_a_shell_and_runc() {
        let tini = "starting\n[FATAL tini (7)] exec /app/server failed: Operation not permitted\n";
        assert_eq!(
            detect_exec_eperm(Some(1), tini).as_deref(),
            Some("/app/server")
        );
        let shell = "/usr/bin/start: line 49: /opt/app/server: Operation not permitted";
        assert_eq!(
            detect_exec_eperm(Some(126), shell).as_deref(),
            Some("/opt/app/server")
        );
        let sh = "sh: /app/server: Operation not permitted";
        assert_eq!(
            detect_exec_eperm(Some(126), sh).as_deref(),
            Some("/app/server")
        );
        let runc = "exec /app/server: operation not permitted";
        assert_eq!(
            detect_exec_eperm(Some(255), runc).as_deref(),
            Some("/app/server")
        );
    }

    #[test]
    fn other_126_exits_and_other_crashes_are_not_recognised() {
        // EACCES: not executable.
        assert_eq!(
            detect_exec_eperm(Some(126), "sh: /app/server: Permission denied"),
            None
        );
        // A shell EPERM line, but the process exited for another reason.
        assert_eq!(
            detect_exec_eperm(
                Some(1),
                "/app/start: line 3: /app/server: Operation not permitted"
            ),
            None
        );
        // An app crash that merely logs EPERM for some file operation.
        assert_eq!(
            detect_exec_eperm(
                Some(1),
                "Error: EPERM: operation not permitted, open '/data/x'"
            ),
            None
        );
        assert_eq!(
            detect_exec_eperm(Some(126), "chown: /data: Operation not permitted extra"),
            None
        );
        // A relative path or no exit code.
        assert_eq!(
            detect_exec_eperm(Some(126), "sh: ./server: Operation not permitted"),
            None
        );
        assert_eq!(
            detect_exec_eperm(
                None,
                "[FATAL tini (7)] exec /a failed: Operation not permitted"
            ),
            None
        );
        assert_eq!(detect_exec_eperm(Some(137), "Killed"), None);
        assert_eq!(detect_exec_eperm(Some(126), ""), None);
    }

    #[test]
    fn the_probe_decides_whether_it_is_this_failure() {
        let found = EntrypointCapabilityFailure::from_probe::<()>(
            "/app/server".into(),
            Ok(Some(nbs_caps())),
        )
        .unwrap();
        assert!(found.verified);
        assert_eq!(found.required, vec!["NET_BIND_SERVICE"]);
        // No file capabilities: the EPERM has another cause.
        assert!(EntrypointCapabilityFailure::from_probe::<()>("/a".into(), Ok(None)).is_none());
        assert!(EntrypointCapabilityFailure::from_probe::<()>(
            "/a".into(),
            Ok(Some(FileCapabilities::default()))
        )
        .is_none());
        // Unreadable: inferred.
        let inferred = EntrypointCapabilityFailure::from_probe("/a".into(), Err("gone")).unwrap();
        assert!(!inferred.verified);
        assert_eq!(inferred.required, vec!["NET_BIND_SERVICE"]);
        assert!(inferred.already_allowed(&[ContainerCapability::NetBindService]));
        assert!(!inferred.already_allowed(&[]));
    }

    #[test]
    fn the_annotation_follows_the_prefix_and_round_trips() {
        let failure = EntrypointCapabilityFailure {
            binary: "/app/server".into(),
            required: vec!["NET_BIND_SERVICE".into()],
            verified: true,
        };
        let message = failure.annotate(
            "Container keeps restarting during startup (restarted 5 times; last exit: exit code 126)"
                .to_string(),
        );
        assert_eq!(
            message,
            "Container keeps restarting during startup [entrypoint_needs_capability: NET_BIND_SERVICE verified] (restarted 5 times; last exit: exit code 126)"
        );
        assert!(
            !message.contains("/app/server"),
            "the image's path is not persisted"
        );
        assert_eq!(
            parse_annotation(&message.to_lowercase()),
            Some((vec!["NET_BIND_SERVICE".to_string()], true))
        );
        let exited =
            failure.annotate("Container exited during startup (exit code 1) after 2s".into());
        assert!(parse_annotation(&exited.to_lowercase()).is_some());
        assert_eq!(failure.annotate("Something else".into()), "Something else");
    }

    #[test]
    fn an_annotation_anywhere_but_after_the_prefix_is_ignored() {
        for body in [
            "container exited during startup (exit code 1: [entrypoint_needs_capability: net_bind_service verified]) after 2s",
            "app said [entrypoint_needs_capability: net_bind_service verified]",
            "container exited during startup [entrypoint_needs_capability: net_bind_service maybe]",
            "container exited during startup [entrypoint_needs_capability:  verified]",
            "container exited during startup [entrypoint_needs_capability: net bind verified]",
        ] {
            assert_eq!(parse_annotation(body), None, "{body}");
        }
    }

    #[test]
    fn the_log_line_names_the_capability_and_both_fixes() {
        let failure = EntrypointCapabilityFailure {
            binary: "/app/server".into(),
            required: vec!["NET_BIND_SERVICE".into()],
            verified: true,
        };
        assert_eq!(
            failure.log_line(),
            "The image's entrypoint /app/server needs the Linux capability NET_BIND_SERVICE (set with setcap), which Temps removes by default. Allow it for this project and redeploy, or remove it from the image (`setcap -r /app/server`) and listen on a port above 1024."
        );
        let raw = EntrypointCapabilityFailure {
            required: vec!["NET_BIND_SERVICE".into(), "NET_RAW".into()],
            ..failure
        };
        let line = raw.log_line();
        assert!(line.contains("no project can allow"), "{line}");
        assert!(!line.contains("Allow it"), "{line}");
    }

    #[test]
    fn the_suggested_fix_only_offers_what_would_actually_fix_it() {
        let fix = SuggestedFix::new(&["NET_BIND_SERVICE".to_string()], true, Some(7));
        assert_eq!(
            fix.allow_capabilities,
            vec![ContainerCapability::NetBindService]
        );
        assert!(fix.not_allowed_capabilities.is_empty());
        assert_eq!(fix.setting, "allowed_capabilities");
        assert_eq!(
            fix.setting_path.as_deref(),
            Some("/projects/7/container-security")
        );
        assert!(fix.image_fix.contains("setcap -r"));
        assert_eq!(
            serde_json::to_value(&fix).unwrap()["allow_capabilities"],
            serde_json::json!(["NET_BIND_SERVICE"])
        );

        let mixed = SuggestedFix::new(
            &["NET_BIND_SERVICE".to_string(), "NET_RAW".to_string()],
            true,
            None,
        );
        assert!(mixed.allow_capabilities.is_empty());
        assert_eq!(mixed.not_allowed_capabilities, vec!["NET_RAW"]);
        assert_eq!(mixed.setting_path, None);
    }
}
