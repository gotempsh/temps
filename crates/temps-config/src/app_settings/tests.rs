// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

#[test]
fn loopback_ja4_header_names_are_normalized_and_checked() {
    assert_eq!(normalize_loopback_ja4_header(None), Ok(None));
    assert_eq!(normalize_loopback_ja4_header(Some("  ")), Ok(None));
    assert_eq!(
        normalize_loopback_ja4_header(Some(" X-JA4 ")),
        Ok(Some("x-ja4".to_string()))
    );
    assert_eq!(
        normalize_loopback_ja4_header(Some("X-Client-Ja4")),
        Ok(Some("x-client-ja4".to_string()))
    );
    assert_eq!(
        normalize_loopback_ja4_header(Some("x ja4")),
        Err(LoopbackJa4HeaderError::InvalidName("x ja4".to_string()))
    );
    assert_eq!(
        normalize_loopback_ja4_header(Some("x-ja4:")),
        Err(LoopbackJa4HeaderError::InvalidName("x-ja4:".to_string()))
    );
    assert_eq!(
        normalize_loopback_ja4_header(Some("Cookie")),
        Err(LoopbackJa4HeaderError::NotJa4("cookie".to_string()))
    );
}

#[test]
fn lowering_proxy_log_retention_holds_addresses_for_the_old_window() {
    let now = Utc::now();
    let settings = AnalyticsRetentionSettings::default().with_ip_hold(None, 90, 7, now);

    assert_eq!(
        settings.ip_hold_until,
        Some(now + chrono::Duration::days(90 + i64::from(IP_REFERENCE_SLACK_DAYS))),
        "logs written under the 90-day window can outlive the new 7-day one"
    );
}

#[test]
fn a_client_supplied_hold_is_not_trusted() {
    let now = Utc::now();
    let forged = AnalyticsRetentionSettings {
        ip_hold_until: Some(now + chrono::Duration::days(3000)),
        ..AnalyticsRetentionSettings::default()
    };

    let settings = forged.with_ip_hold(None, 30, 30, now);

    assert_eq!(settings.ip_hold_until, None);
}

#[test]
fn raising_the_window_keeps_a_hold_and_a_second_lowering_only_extends_it() {
    let now = Utc::now();
    let later = now + chrono::Duration::days(60);

    let kept = AnalyticsRetentionSettings::default().with_ip_hold(Some(later), 7, 30, now);
    assert_eq!(kept.ip_hold_until, Some(later));

    let extended = AnalyticsRetentionSettings::default().with_ip_hold(Some(later), 30, 7, now);
    assert_eq!(
        extended.ip_hold_until,
        Some(later),
        "a shorter hold never replaces a longer one"
    );
}

#[test]
fn a_hold_that_has_passed_is_dropped() {
    let now = Utc::now();
    let past = now - chrono::Duration::days(1);

    let settings = AnalyticsRetentionSettings::default().with_ip_hold(Some(past), 30, 30, now);

    assert_eq!(settings.ip_hold_until, None);
}

use super::*;

#[test]
fn a_worker_turns_cluster_dns_on_only_until_an_operator_decides() {
    let untouched = ClusterDnsSettings::default();
    assert!(untouched.should_enable_for_worker());

    // Already on: nothing to do.
    let on = ClusterDnsSettings {
        enabled: true,
        operator_chosen: false,
    };
    assert!(!on.should_enable_for_worker());

    // An operator turned it off: a later worker must not override that.
    let turned_off = ClusterDnsSettings::after_operator_save(
        &on,
        &ClusterDnsSettings {
            enabled: false,
            operator_chosen: false,
        },
    );
    assert!(!turned_off.enabled);
    assert!(turned_off.operator_chosen);
    assert!(!turned_off.should_enable_for_worker());
}

#[test]
fn saving_cluster_dns_unchanged_is_not_a_choice_and_a_choice_sticks() {
    let stored = ClusterDnsSettings::default();
    // A save that resends the stored value decides nothing.
    let resent = ClusterDnsSettings::after_operator_save(&stored, &stored);
    assert!(!resent.operator_chosen);

    // Once chosen, a client sending `operator_chosen: false` cannot clear it.
    let chosen = ClusterDnsSettings {
        enabled: true,
        operator_chosen: true,
    };
    let cleared = ClusterDnsSettings::after_operator_save(
        &chosen,
        &ClusterDnsSettings {
            enabled: true,
            operator_chosen: false,
        },
    );
    assert!(cleared.operator_chosen);
}

#[test]
fn ingest_override_validation_accepts_inherit_zero_and_max_only() {
    let v = ContainerLogSettings::validate_ingest_override;
    assert!(v(None, None).is_ok());
    assert!(v(Some(0), Some(0)).is_ok(), "0 = unlimited");
    assert!(v(Some(1_000_000), Some(1_048_576)).is_ok());
    let err = v(Some(-1), None).unwrap_err();
    assert!(err.contains("log_ingest_lines_per_sec -1"), "{err}");
    let err = v(None, Some(1_048_577)).unwrap_err();
    assert!(err.contains("log_ingest_kib_per_sec 1048577"), "{err}");
}

#[test]
fn container_logs_saved_before_the_ingest_budget_get_the_default_budget() {
    let saved = serde_json::json!({
        "max_size": "50m",
        "max_file": 3,
        "service_max_size": "20m",
        "service_max_file": 3,
        "cache_mb": 4096,
        "head_buffer_mb": 16
    });
    let logs: ContainerLogSettings = serde_json::from_value(saved).unwrap();
    assert_eq!(logs.cache_mb, 4096, "existing values are kept");
    let defaults = ContainerLogSettings::default();
    assert_eq!(logs.ingest_lines_per_sec, defaults.ingest_lines_per_sec);
    assert_eq!(logs.ingest_kib_per_sec, defaults.ingest_kib_per_sec);
}

#[test]
fn build_limits_saved_before_the_cache_cap_get_the_default_cap() {
    let restored: BuildLimitsSettings = serde_json::from_value(serde_json::json!({
        "max_concurrent": 3,
        "cpu_limit_cores": 1.5,
        "memory_limit_mb": 1024
    }))
    .expect("deserialize historical build limits");
    assert_eq!(restored.max_concurrent, 3);
    assert_eq!(restored.build_cache_max_gb, 10);
    assert_eq!(restored.build_cache_max_unused_days, 7);
    assert_eq!(
        restored.build_cache_max_bytes(),
        Some(10 * 1024 * 1024 * 1024)
    );
}

#[test]
fn build_cache_cap_of_zero_disables_it_and_unused_days_are_clamped() {
    let mut limits = BuildLimitsSettings {
        build_cache_max_gb: 0,
        build_cache_max_unused_days: 0,
        ..Default::default()
    };
    assert_eq!(limits.build_cache_max_bytes(), None);
    assert_eq!(limits.effective_build_cache_max_unused_days(), 1);
    limits.build_cache_max_unused_days = 10_000;
    assert_eq!(limits.effective_build_cache_max_unused_days(), 365);
}

#[test]
fn gateway_default_serializes_as_unpinned_across_releases() {
    let settings = PreviewGatewaySettings::default();
    assert_eq!(settings.image, "");
    let serialized = serde_json::to_value(&settings).expect("serialize gateway settings");
    assert_eq!(serialized["image"], "");
    let restored: PreviewGatewaySettings = serde_json::from_value(serde_json::json!({}))
        .expect("deserialize historical settings without image");
    assert!(restored.image.is_empty());
}

// ── ADR-042 §3: the bulk-activation throttle ──────────────────────

#[test]
fn a_new_instance_runs_bulk_activation_unthrottled() {
    // "Activate now" is what the customer paid for. A throttle that appears
    // by default would make every activation slower with nothing on screen
    // explaining why.
    assert_eq!(
        CloudSettings::default().effective_bulk_rate_limit_spans_per_sec(),
        None
    );
}

#[test]
fn a_settings_row_written_by_an_older_build_has_no_throttle() {
    // The field is additive, so a row serialized before it existed must
    // deserialize to "unthrottled" rather than failing the whole settings
    // read — which would take the settings page down on upgrade.
    let legacy = r#"{"backend_url":"https://app.temps.sh","telemetry_enabled":true,
             "backups_enabled":false,"notifications_enabled":false,
             "telemetry_outbox_max_bytes":536870912}"#;
    let parsed: CloudSettings =
        serde_json::from_str(legacy).expect("a legacy row must still parse");

    assert_eq!(parsed.effective_bulk_rate_limit_spans_per_sec(), None);
    assert!(parsed.telemetry_enabled);
}

#[test]
fn a_zero_throttle_reads_as_unthrottled_rather_than_as_a_stalled_job() {
    // Taken literally, zero spans per second is a job that runs forever
    // while reporting itself as running — the single worst state for an
    // operator with nobody to ask.
    let stalled = CloudSettings {
        telemetry_bulk_rate_limit_spans_per_sec: Some(0),
        ..Default::default()
    };
    assert_eq!(stalled.effective_bulk_rate_limit_spans_per_sec(), None);
}

#[test]
fn an_operator_set_throttle_is_used_verbatim() {
    let throttled = CloudSettings {
        telemetry_bulk_rate_limit_spans_per_sec: Some(5_000),
        ..Default::default()
    };
    assert_eq!(
        throttled.effective_bulk_rate_limit_spans_per_sec(),
        Some(5_000)
    );
}

// ── ADR-042 §6.3: the byte-budget anomaly factor ──────────────────

#[test]
fn a_new_instance_gets_the_documented_anomaly_factor_not_an_unguarded_job() {
    // The purchase path spends money with no human confirm. The guard must
    // therefore be on out of the box: an operator opting *into* a safety net
    // they do not know exists is not a safety net.
    assert_eq!(
        CloudSettings::default().effective_bulk_anomaly_factor(),
        DEFAULT_CLOUD_TELEMETRY_BULK_ANOMALY_FACTOR
    );
}

#[test]
fn a_settings_row_written_before_the_guard_existed_still_gets_the_guard() {
    // The important half of "additive": upgrading must not silently remove
    // a money guard from every instance that has a settings row already.
    let legacy = r#"{"backend_url":"https://app.temps.sh","telemetry_enabled":true,
             "backups_enabled":false,"notifications_enabled":false,
             "telemetry_outbox_max_bytes":536870912}"#;
    let parsed: CloudSettings =
        serde_json::from_str(legacy).expect("a legacy row must still parse");

    assert_eq!(parsed.telemetry_bulk_anomaly_factor, None);
    assert_eq!(
        parsed.effective_bulk_anomaly_factor(),
        DEFAULT_CLOUD_TELEMETRY_BULK_ANOMALY_FACTOR
    );
}

#[test]
fn an_operator_set_anomaly_factor_is_used_verbatim() {
    let tight = CloudSettings {
        telemetry_bulk_anomaly_factor: Some(2.5),
        ..Default::default()
    };
    assert_eq!(tight.effective_bulk_anomaly_factor(), 2.5);
}

#[test]
fn an_unusable_anomaly_factor_is_clamped_rather_than_bricking_activation() {
    // A factor below 1 pauses every project before it reaches its own
    // estimate, which would make an activation the customer has already paid
    // for impossible to finish. NaN/infinity come from a hand-edited row and
    // must not propagate into a comparison that is false for everything.
    for unusable in [Some(0.0), Some(-3.0), Some(f32::NAN), Some(f32::INFINITY)] {
        let settings = CloudSettings {
            telemetry_bulk_anomaly_factor: unusable,
            ..Default::default()
        };
        let effective = settings.effective_bulk_anomaly_factor();
        assert!(
            effective.is_finite() && effective >= MIN_CLOUD_TELEMETRY_BULK_ANOMALY_FACTOR,
            "{unusable:?} resolved to an unusable {effective}"
        );
    }
}

#[test]
fn an_enormous_anomaly_factor_is_clamped_rather_than_disabling_the_money_guard() {
    // Without a ceiling, one field on the settings document turns the
    // purchase path — which spends with no human confirm — back into an
    // unbounded one, and nothing on any screen says so. The write path
    // rejects such a value; this clamp is what protects a row that predates
    // the ceiling, was hand-edited, or came back from a restore.
    for absurd in [
        MAX_CLOUD_TELEMETRY_BULK_ANOMALY_FACTOR + 1.0,
        1_000.0,
        1e12,
        f32::MAX,
    ] {
        let settings = CloudSettings {
            telemetry_bulk_anomaly_factor: Some(absurd),
            ..Default::default()
        };
        assert_eq!(
            settings.effective_bulk_anomaly_factor(),
            MAX_CLOUD_TELEMETRY_BULK_ANOMALY_FACTOR,
            "{absurd} must clamp to the ceiling, not become an off switch"
        );
    }

    // The ceiling itself is still usable verbatim — clamping must not make
    // the widest legitimate setting unreachable.
    let widest = CloudSettings {
        telemetry_bulk_anomaly_factor: Some(MAX_CLOUD_TELEMETRY_BULK_ANOMALY_FACTOR),
        ..Default::default()
    };
    assert_eq!(
        widest.effective_bulk_anomaly_factor(),
        MAX_CLOUD_TELEMETRY_BULK_ANOMALY_FACTOR
    );
    // The ceiling has to leave real headroom above the default, or the
    // setting stops being a tuning knob at all.
    const _: () = assert!(
        MAX_CLOUD_TELEMETRY_BULK_ANOMALY_FACTOR > DEFAULT_CLOUD_TELEMETRY_BULK_ANOMALY_FACTOR
    );
}

// Issue #478: a project domain must never be allowed to claim the
// console hostname — doing so locks the operator out of the console and
// recovery requires the raw public IP.
#[test]
fn console_hostname_parses_url_and_bare_host() {
    let with_external_url = |raw: Option<&str>| AppSettings {
        external_url: raw.map(str::to_string),
        ..Default::default()
    };

    assert_eq!(
        with_external_url(Some("https://Console.Example.com:8443/"))
            .console_hostname()
            .as_deref(),
        Some("console.example.com")
    );
    assert_eq!(
        with_external_url(Some("console.example.com"))
            .console_hostname()
            .as_deref(),
        Some("console.example.com")
    );
    assert_eq!(with_external_url(Some("   ")).console_hostname(), None);
    assert_eq!(with_external_url(None).console_hostname(), None);
}

#[test]
fn reserved_hostname_covers_console_and_preview_apex() {
    let s = AppSettings {
        external_url: Some("https://console.example.com".to_string()),
        preview_domain: "apps.example.com".to_string(),
        ..Default::default()
    };

    assert!(s.is_reserved_hostname("console.example.com"));
    // Case and trailing-dot variants are the same host.
    assert!(s.is_reserved_hostname("CONSOLE.example.com."));
    assert!(s.is_reserved_hostname("apps.example.com"));

    // Ordinary project domains, including subdomains of the preview
    // domain, stay assignable.
    assert!(!s.is_reserved_hostname("shop.example.com"));
    assert!(!s.is_reserved_hostname("my-app.apps.example.com"));
    assert!(!s.is_reserved_hostname(""));
}

#[test]
fn reserved_hostname_is_inert_without_external_url() {
    let s = AppSettings {
        external_url: None,
        preview_domain: String::new(),
        ..Default::default()
    };
    assert!(!s.is_reserved_hostname("anything.example.com"));
}

// ADR-051: the public ingress hostname is a platform host, exactly like
// the console hostname. A project that claimed it would swallow every
// third-party callback.
#[test]
fn reserved_hostname_covers_the_public_ingress_host() {
    let s = AppSettings {
        external_url: Some("https://console.example.com".to_string()),
        public_ingress_url: Some("https://Hooks.example.com/".to_string()),
        preview_domain: "apps.example.com".to_string(),
        ..Default::default()
    };
    assert!(s.is_reserved_hostname("hooks.example.com"));
    assert!(s.is_reserved_hostname("HOOKS.example.com."));
    assert!(s.is_reserved_hostname("console.example.com"));
    assert!(!s.is_reserved_hostname("other.example.com"));
}

#[test]
fn settings_stored_before_public_ingress_existed_still_load() {
    let s = AppSettings::from_json(serde_json::json!({
        "external_url": "https://console.example.com",
        "preview_domain": "apps.example.com"
    }));
    assert_eq!(s.public_ingress_url, None);
    assert_eq!(
        s.effective_public_ingress_url(),
        "https://console.example.com"
    );
}

// ADR-024: cluster-DNS injection is experimental/beta and defaults OFF
// to avoid the DNS-timeout-cascade failure mode (22-27 s TCP delays when
// the injected resolver is transiently slow for external hostnames).
#[test]
fn cluster_dns_defaults_disabled() {
    let s = ClusterDnsSettings::default();
    assert!(
        !s.enabled,
        "cluster DNS must be opt-in (off by default) to avoid DNS cascade delays"
    );
}

#[test]
fn app_settings_default_has_cluster_dns_disabled() {
    let s = AppSettings::default();
    assert!(
        !s.cluster_dns.enabled,
        "AppSettings::default() must have cluster_dns.enabled = false"
    );
}

#[test]
fn cluster_dns_round_trips_through_json() {
    let mut s = AppSettings::default();
    s.cluster_dns.enabled = true;

    let json = s.to_json();
    let back = AppSettings::from_json(json);
    assert!(
        back.cluster_dns.enabled,
        "cluster_dns.enabled must survive JSON round-trip"
    );
}

#[test]
fn legacy_settings_json_without_cluster_dns_deserializes_as_disabled() {
    // Old `settings.data` rows have no `cluster_dns` key. `#[serde(default)]`
    // must fill it in with the disabled default so pre-ADR-024 rows keep
    // loading and the feature stays off.
    let legacy = serde_json::json!({
        "external_url": "https://paas.example.com",
        "preview_domain": "localho.st"
    });
    let parsed = AppSettings::from_json(legacy);
    assert!(
        !parsed.cluster_dns.enabled,
        "cluster_dns must default to disabled when deserializing a legacy settings row"
    );
}

/// A throwaway key so the encryption round-trip is exercised without
/// depending on a data directory.
fn test_encryption() -> EncryptionService {
    EncryptionService::new(&"a".repeat(64)).expect("build encryption service")
}

#[test]
fn geo_defaults_are_applied_when_unset() {
    let geo = GeoSettings::default();
    assert_eq!(geo.refresh_interval_hours, None);
    assert_eq!(geo.stale_lookup_days, None);
    assert_eq!(
        geo.effective_refresh_interval_hours(),
        DEFAULT_GEO_REFRESH_INTERVAL_HOURS
    );
    assert_eq!(
        geo.effective_stale_lookup_days(),
        DEFAULT_GEO_STALE_LOOKUP_DAYS
    );
    assert!(!geo.license_key_configured());
    assert_eq!(geo.age_days(Utc::now()), None);
    assert!(!geo.last_check_failed());
}

#[test]
fn geo_knobs_are_clamped_rather_than_rejected_at_read_time() {
    let geo = GeoSettings {
        refresh_interval_hours: Some(0),
        stale_lookup_days: Some(u32::MAX),
        ..GeoSettings::default()
    };
    assert_eq!(
        geo.effective_refresh_interval_hours(),
        MIN_GEO_REFRESH_INTERVAL_HOURS
    );
    assert_eq!(geo.effective_refresh_interval().as_secs(), 3600);
    assert_eq!(geo.effective_stale_lookup_days(), MAX_GEO_STALE_LOOKUP_DAYS);
}

#[test]
fn geo_age_prefers_the_build_epoch_over_the_download_time() {
    let now = DateTime::parse_from_rfc3339("2026-02-01T00:00:00Z")
        .expect("parse now")
        .with_timezone(&Utc);
    let geo = GeoSettings {
        build_epoch: Some(
            u64::try_from(
                DateTime::parse_from_rfc3339("2026-01-01T00:00:00Z")
                    .expect("parse build time")
                    .timestamp(),
            )
            .expect("positive epoch"),
        ),
        last_refreshed_at: Some(now),
        ..GeoSettings::default()
    };

    assert_eq!(geo.age_days(now), Some(31));
}

#[test]
fn geo_age_falls_back_to_the_download_time_and_is_never_negative() {
    let now = Utc::now();
    let downloaded_only = GeoSettings {
        last_refreshed_at: Some(now - chrono::Duration::days(3)),
        ..GeoSettings::default()
    };
    assert_eq!(downloaded_only.age_days(now), Some(3));

    let future_build = GeoSettings {
        build_epoch: u64::try_from((now + chrono::Duration::days(5)).timestamp()).ok(),
        ..GeoSettings::default()
    };
    assert_eq!(future_build.age_days(now), Some(0));
}

#[test]
fn geo_license_key_is_encrypted_on_submission_and_decrypts_back() {
    let encryption = test_encryption();
    let mut incoming = GeoSettings {
        maxmind_license_key: Some("  a-real-license-key  ".to_string()),
        ..GeoSettings::default()
    };

    incoming
        .apply_license_key_update(&GeoSettings::default(), &encryption)
        .expect("encrypt the submitted key");

    assert_eq!(
        incoming.maxmind_license_key, None,
        "plaintext must be consumed"
    );
    assert!(incoming.license_key_configured());
    let ciphertext = incoming
        .maxmind_license_key_encrypted
        .as_deref()
        .expect("ciphertext stored");
    assert!(
        !ciphertext.contains("a-real-license-key"),
        "the stored value must not embed the plaintext"
    );
    assert_eq!(
        incoming
            .decrypt_license_key(&encryption)
            .expect("decrypt")
            .as_deref(),
        Some("a-real-license-key"),
        "the key must round-trip with surrounding whitespace trimmed"
    );
}

#[test]
fn geo_blank_license_key_preserves_the_stored_one() {
    let encryption = test_encryption();
    let current = GeoSettings {
        maxmind_license_key_encrypted: Some("stored-ciphertext".to_string()),
        ..GeoSettings::default()
    };

    for submitted in [None, Some(String::new()), Some("   ".to_string())] {
        let mut incoming = GeoSettings {
            maxmind_license_key: submitted,
            ..GeoSettings::default()
        };
        incoming
            .apply_license_key_update(&current, &encryption)
            .expect("preserve the stored key");
        assert_eq!(
            incoming.maxmind_license_key_encrypted.as_deref(),
            Some("stored-ciphertext"),
            "a blank submission must not wipe the stored key"
        );
    }
}

#[test]
fn geo_license_key_can_be_explicitly_cleared() {
    let encryption = test_encryption();
    let current = GeoSettings {
        maxmind_license_key_encrypted: Some("stored-ciphertext".to_string()),
        ..GeoSettings::default()
    };
    let mut incoming = GeoSettings {
        clear_maxmind_license_key: true,
        ..GeoSettings::default()
    };

    incoming
        .apply_license_key_update(&current, &encryption)
        .expect("clear the stored key");

    assert_eq!(incoming.maxmind_license_key_encrypted, None);
    assert!(!incoming.license_key_configured());
    assert!(
        !incoming.clear_maxmind_license_key,
        "the write-only flag must be consumed so it never persists"
    );
}

#[test]
fn geo_a_new_key_wins_over_a_stale_clear_flag() {
    let encryption = test_encryption();
    let mut incoming = GeoSettings {
        maxmind_license_key: Some("replacement-key".to_string()),
        clear_maxmind_license_key: true,
        ..GeoSettings::default()
    };

    incoming
        .apply_license_key_update(&GeoSettings::default(), &encryption)
        .expect("encrypt the submitted key");

    assert_eq!(
        incoming
            .decrypt_license_key(&encryption)
            .expect("decrypt")
            .as_deref(),
        Some("replacement-key")
    );
}

#[test]
fn geo_plaintext_license_key_is_never_serialized() {
    let mut settings = AppSettings::default();
    settings.geo.maxmind_license_key = Some("must-not-persist".to_string());
    settings.geo.clear_maxmind_license_key = true;
    settings.geo.maxmind_license_key_encrypted = Some("ciphertext".to_string());

    let json = settings.to_json();
    let rendered = serde_json::to_string(&json).expect("render settings json");
    assert!(
        !rendered.contains("must-not-persist"),
        "a plaintext license key must never reach the settings document"
    );
    assert!(!rendered.contains("clear_maxmind_license_key"));

    let back = AppSettings::from_json(json);
    assert_eq!(back.geo.maxmind_license_key, None);
    assert!(!back.geo.clear_maxmind_license_key);
    assert_eq!(
        back.geo.maxmind_license_key_encrypted.as_deref(),
        Some("ciphertext"),
        "the ciphertext must survive the round trip"
    );
}

#[test]
fn geo_debug_output_never_contains_key_material() {
    let geo = GeoSettings {
        maxmind_license_key: Some("plaintext-key".to_string()),
        maxmind_license_key_encrypted: Some("ciphertext-blob".to_string()),
        ..GeoSettings::default()
    };
    let rendered = format!("{:?}", geo);
    assert!(!rendered.contains("plaintext-key"));
    assert!(!rendered.contains("ciphertext-blob"));
    assert!(rendered.contains("license_key_configured: true"));
}

/// The intent signal must agree with what `apply_license_key_update`
/// actually does, for every combination of the two write-only fields --
/// they are read in two different layers (handler and service) and a
/// divergence would either revert a just-saved key or fail to store one.
#[test]
fn geo_license_key_intent_matches_what_the_update_applies() {
    let encryption = test_encryption();
    let current = GeoSettings {
        maxmind_license_key_encrypted: Some("stored-ciphertext".to_string()),
        ..GeoSettings::default()
    };

    let cases = [
        (None, false, GeoLicenseKeyIntent::Unchanged),
        (
            Some("   ".to_string()),
            false,
            GeoLicenseKeyIntent::Unchanged,
        ),
        (None, true, GeoLicenseKeyIntent::Cleared),
        (Some("new-key".to_string()), false, GeoLicenseKeyIntent::Set),
        (Some("new-key".to_string()), true, GeoLicenseKeyIntent::Set),
    ];

    for (submitted, clear, expected) in cases {
        let mut incoming = GeoSettings {
            maxmind_license_key: submitted,
            clear_maxmind_license_key: clear,
            ..GeoSettings::default()
        };
        assert_eq!(incoming.license_key_intent(), expected);

        incoming
            .apply_license_key_update(&current, &encryption)
            .expect("apply the license key update");
        assert_eq!(
            incoming.license_key_intent(),
            GeoLicenseKeyIntent::Unchanged,
            "the write-only fields must be consumed"
        );

        match expected {
            GeoLicenseKeyIntent::Unchanged => assert_eq!(
                incoming.maxmind_license_key_encrypted.as_deref(),
                Some("stored-ciphertext")
            ),
            GeoLicenseKeyIntent::Cleared => {
                assert_eq!(incoming.maxmind_license_key_encrypted, None)
            }
            GeoLicenseKeyIntent::Set => assert_eq!(
                incoming
                    .decrypt_license_key(&encryption)
                    .expect("decrypt")
                    .as_deref(),
                Some("new-key")
            ),
        }
    }
}

#[test]
fn geo_recorded_state_is_restored_from_the_stored_document() {
    let now = Utc::now();
    let current = GeoSettings {
        last_refreshed_at: Some(now),
        source: Some(GEO_SOURCE_MAXMIND_OFFICIAL.to_string()),
        build_epoch: Some(1_767_225_600),
        last_check_at: Some(now),
        last_check_status: Some(GEO_CHECK_STATUS_OK.to_string()),
        last_error: None,
        ..GeoSettings::default()
    };
    // What a client PUT looks like: policy knobs only, no metadata.
    let mut incoming = GeoSettings {
        refresh_interval_hours: Some(6),
        source: Some("forged".to_string()),
        last_check_status: Some(GEO_CHECK_STATUS_ERROR.to_string()),
        last_error: Some("a failure the client invented".to_string()),
        ..GeoSettings::default()
    };

    incoming.preserve_recorded_state(&current);

    assert_eq!(incoming.refresh_interval_hours, Some(6));
    assert_eq!(
        incoming.source.as_deref(),
        Some(GEO_SOURCE_MAXMIND_OFFICIAL)
    );
    assert_eq!(incoming.build_epoch, Some(1_767_225_600));
    assert_eq!(
        incoming.last_check_status.as_deref(),
        Some(GEO_CHECK_STATUS_OK)
    );
    assert_eq!(incoming.last_error, None);
    assert!(!incoming.last_check_failed());
}

#[test]
fn geo_settings_round_trip_through_the_settings_document() {
    let now = Utc::now();
    let mut settings = AppSettings::default();
    settings.geo.refresh_interval_hours = Some(12);
    settings.geo.stale_lookup_days = Some(7);
    settings.geo.maxmind_license_key_encrypted = Some("ciphertext".to_string());
    settings.geo.source = Some(GEO_SOURCE_BUNDLED_GITHUB.to_string());
    settings.geo.build_epoch = Some(1_767_225_600);
    settings.geo.last_refreshed_at = Some(now);
    settings.geo.last_check_at = Some(now);
    settings.geo.last_check_status = Some(GEO_CHECK_STATUS_OK.to_string());
    settings.geo.last_error = None;

    let back = AppSettings::from_json(settings.to_json());
    assert_eq!(back.geo, settings.geo);
}

#[test]
fn legacy_settings_json_uses_geo_defaults() {
    let parsed = AppSettings::from_json(serde_json::json!({
        "external_url": "https://paas.example.com",
        "preview_domain": "localho.st"
    }));
    assert_eq!(parsed.geo, GeoSettings::default());
    assert!(!parsed.geo.license_key_configured());
}

#[test]
fn clickhouse_compression_defaults_to_lz4_for_legacy_settings_json() {
    let parsed = AppSettings::from_json(serde_json::json!({
        "external_url": "https://paas.example.com"
    }));

    assert_eq!(parsed.clickhouse.compression, ClickHouseCompression::Lz4);
    assert_eq!(parsed.clickhouse, ClickHouseSettings::default());
}

#[test]
fn clickhouse_compression_round_trips_through_json_as_snake_case() {
    let mut settings = AppSettings::default();
    settings.clickhouse.compression = ClickHouseCompression::None;

    let json = settings.to_json();
    assert_eq!(json["clickhouse"]["compression"], "none");

    let parsed = AppSettings::from_json(json);
    assert_eq!(parsed.clickhouse.compression, ClickHouseCompression::None);
}

#[test]
fn clickhouse_compression_survives_an_unrelated_malformed_section() {
    // `monitoring` is the wrong type, so the whole document fails to
    // decode; the saved ClickHouse mode must not fall back to the default.
    let parsed = AppSettings::from_json(serde_json::json!({
        "monitoring": "not an object",
        "clickhouse": { "compression": "none" }
    }));

    assert_eq!(parsed.clickhouse.compression, ClickHouseCompression::None);
}

#[test]
fn clickhouse_compression_rejects_unknown_wire_values() {
    let result: Result<ClickHouseSettings, _> =
        serde_json::from_value(serde_json::json!({ "compression": "zstd" }));
    assert!(result.is_err(), "zstd is not a supported client mode");
}

#[test]
fn legacy_settings_json_uses_observability_retention_defaults() {
    let parsed = AppSettings::from_json(serde_json::json!({
        "external_url": "https://paas.example.com",
        "preview_domain": "localho.st"
    }));

    assert_eq!(
        parsed.observability_retention,
        ObservabilityRetentionSettings::default()
    );
    assert_eq!(parsed.observability_retention.proxy_logs_days, 30);
    assert_eq!(parsed.observability_retention.otel_spans_days, 90);
}

#[test]
fn legacy_settings_json_uses_analytics_retention_defaults() {
    let parsed = AppSettings::from_json(serde_json::json!({
        "external_url": "https://paas.example.com"
    }));

    assert_eq!(parsed.analytics_retention.events_days, 365);
    // Upgrading must not start deleting visitors or error events.
    assert_eq!(parsed.analytics_retention.visitor_days, None);
    assert_eq!(parsed.analytics_retention.error_events_days, None);
    assert_eq!(parsed.analytics_retention.unreferenced_ip_days, Some(30));
}

#[test]
fn analytics_retention_round_trips_and_null_means_keep() {
    let parsed = AppSettings::from_json(serde_json::json!({
        "analytics_retention": {
            "events_days": 30,
            "visitor_days": 425,
            "unreferenced_ip_days": null
        }
    }));

    assert_eq!(parsed.analytics_retention.events_days, 30);
    assert_eq!(parsed.analytics_retention.visitor_days, Some(425));
    assert_eq!(parsed.analytics_retention.unreferenced_ip_days, None);

    let again = AppSettings::from_json(parsed.to_json());
    assert_eq!(again.analytics_retention, parsed.analytics_retention);
}

#[test]
fn analytics_event_default_applies_only_when_the_window_is_missing() {
    for data in [
        serde_json::json!({}),
        serde_json::json!({"visitor_days": 425}),
    ] {
        let settings: AnalyticsRetentionSettings = serde_json::from_value(data).unwrap();
        assert_eq!(settings.events_days, 365);
    }
    for days in [1, 90, 365, 730, 3650] {
        let saved: AnalyticsRetentionSettings =
            serde_json::from_value(serde_json::json!({"events_days": days})).unwrap();
        assert_eq!(
            saved.events_days, days,
            "saved windows must not be overwritten"
        );
        let reloaded: AnalyticsRetentionSettings =
            serde_json::from_value(serde_json::to_value(saved).unwrap()).unwrap();
        assert_eq!(reloaded.events_days, days);
    }
}

#[test]
fn analytics_retention_reports_the_first_window_out_of_range() {
    assert_eq!(AnalyticsRetentionSettings::default().out_of_range(), None);

    let zero = AnalyticsRetentionSettings {
        visitor_days: Some(0),
        ..Default::default()
    };
    assert_eq!(zero.out_of_range(), Some(("visitor_days", 0)));

    let too_long = AnalyticsRetentionSettings {
        events_days: 3651,
        ..Default::default()
    };
    assert_eq!(too_long.out_of_range(), Some(("events_days", 3651)));
}

#[test]
fn observability_retention_round_trips_through_json() {
    let mut settings = AppSettings::default();
    settings.observability_retention.proxy_logs_days = 14;
    settings.observability_retention.otel_spans_days = 60;
    settings.observability_retention.otel_logs_days = 45;
    settings.observability_retention.otel_metrics_days = 30;

    let parsed = AppSettings::from_json(settings.to_json());

    assert_eq!(
        parsed.observability_retention,
        settings.observability_retention
    );
}

#[test]
fn on_demand_tls_defaults_are_off_and_sensible() {
    let s = OnDemandTlsSettings::default();
    assert!(!s.enabled, "on-demand TLS must be opt-in (off by default)");
    assert_eq!(s.zone, None);
    assert_eq!(s.max_concurrent, 3);
    assert_eq!(s.hourly_cap, 10);
    assert_eq!(s.deployment_url_mode, "http");
}

#[test]
fn app_settings_default_enables_on_demand_tls_for_new_installations() {
    let s = AppSettings::default();
    assert!(s.on_demand_tls.enabled);
    assert_eq!(s.on_demand_tls.zone, None);
    assert_eq!(s.on_demand_tls.deployment_url_mode, "http");
}

#[test]
fn stored_section_without_enabled_stays_off() {
    // A partial section written by an older build keeps its explicit shape;
    // only a wholly absent section is treated as legacy-off too.
    let parsed = AppSettings::from_json(serde_json::json!({
        "on_demand_tls": { "zone": "apps.example.com" }
    }));
    assert!(!parsed.on_demand_tls.enabled);
    assert_eq!(
        parsed.on_demand_tls.zone.as_deref(),
        Some("apps.example.com")
    );
}

#[test]
fn explicitly_stored_opt_out_survives_a_malformed_sibling_section() {
    let parsed = AppSettings::from_json(serde_json::json!({
        "on_demand_tls": { "enabled": false },
        "clickhouse": "not-an-object"
    }));
    assert!(!parsed.on_demand_tls.enabled);
}

#[test]
fn unreadable_on_demand_section_falls_back_to_legacy_off() {
    let parsed = AppSettings::from_json(serde_json::json!({
        "on_demand_tls": "garbage",
        "clickhouse": "not-an-object"
    }));
    assert!(!parsed.on_demand_tls.enabled);
}

#[test]
fn legacy_settings_json_without_on_demand_tls_deserializes() {
    // An old `settings.data` row written before ADR-018 has no
    // `on_demand_tls` key. `#[serde(default)]` must fill it in with the
    // disabled default so pre-migration rows keep loading.
    let legacy = serde_json::json!({
        "external_url": "https://paas.example.com",
        "preview_domain": "localho.st"
    });
    let parsed = AppSettings::from_json(legacy);
    assert_eq!(
        parsed.external_url.as_deref(),
        Some("https://paas.example.com")
    );
    assert!(!parsed.on_demand_tls.enabled);
    assert_eq!(parsed.on_demand_tls.max_concurrent, 3);
    assert_eq!(parsed.on_demand_tls.hourly_cap, 10);
}

#[test]
fn legacy_preview_gateway_settings_default_to_enabled() {
    let legacy = serde_json::json!({
        "image": "ghcr.io/gotempsh/temps-preview-gateway:latest",
        "host_port": 8090,
        "auto_upgrade": true
    });

    let parsed: PreviewGatewaySettings =
        serde_json::from_value(legacy).expect("legacy preview gateway settings should parse");

    assert!(
        parsed.enabled,
        "legacy settings must keep preview gateway reconciliation enabled"
    );
}

#[test]
fn on_demand_tls_round_trips_through_json() {
    let mut s = AppSettings::default();
    s.on_demand_tls.enabled = true;
    s.on_demand_tls.zone = Some("1.2.3.4.sslip.io".to_string());
    s.on_demand_tls.max_concurrent = 5;
    s.on_demand_tls.hourly_cap = 25;
    s.on_demand_tls.deployment_url_mode = "redirect_to_env".to_string();

    let json = s.to_json();
    let back = AppSettings::from_json(json);
    assert!(back.on_demand_tls.enabled);
    assert_eq!(back.on_demand_tls.zone.as_deref(), Some("1.2.3.4.sslip.io"));
    assert_eq!(back.on_demand_tls.max_concurrent, 5);
    assert_eq!(back.on_demand_tls.hourly_cap, 25);
    assert_eq!(back.on_demand_tls.deployment_url_mode, "redirect_to_env");
}

#[test]
fn require_mfa_for_admins_defaults_to_false() {
    // MFA enforcement must be opt-in: an operator upgrading Temps should
    // never suddenly get locked out of their own Admin account because a
    // new default flipped a login-blocking setting on.
    let s = AppSettings::default();
    assert!(!s.require_mfa_for_admins);
}

#[test]
fn legacy_settings_json_without_require_mfa_for_admins_deserializes() {
    // A `settings.data` row written before this feature shipped has no
    // `require_mfa_for_admins` key. `#[serde(default)]` must fill it in
    // with `false` so pre-migration rows keep loading and don't
    // retroactively lock out admins who never enrolled MFA.
    let legacy = serde_json::json!({
        "external_url": "https://paas.example.com",
        "preview_domain": "localho.st"
    });
    let parsed = AppSettings::from_json(legacy);
    assert!(!parsed.require_mfa_for_admins);
}

#[test]
fn require_mfa_for_admins_round_trips_through_json() {
    let s = AppSettings {
        require_mfa_for_admins: true,
        ..AppSettings::default()
    };

    let json = s.to_json();
    let back = AppSettings::from_json(json);
    assert!(back.require_mfa_for_admins);
}

#[test]
fn observability_compression_defaults_to_24_hours() {
    let compression = ObservabilityCompressionSettings::default();
    assert_eq!(compression.proxy_logs_after_hours, 24);
    assert_eq!(compression.otel_spans_after_hours, 24);
}

#[test]
fn cloud_exports_default_off_for_legacy_and_new_settings() {
    let defaults = CloudSettings::default();
    assert!(!defaults.telemetry_enabled);
    assert!(!defaults.backups_enabled);
    assert!(!defaults.notifications_enabled);
    // ADR-045 §5: console access defaults off just like the other three
    // consent flags -- an instance never gets it merely by loading
    // settings. Every enrollment path preserves explicit operator consent.
    assert!(!defaults.console_access_enabled);

    let parsed = AppSettings::from_json(serde_json::json!({
        "cloud": {"backend_url": "https://cloud.example.com"}
    }));
    assert!(!parsed.cloud.telemetry_enabled);
    assert!(!parsed.cloud.backups_enabled);
    assert!(!parsed.cloud.notifications_enabled);
    assert!(!parsed.cloud.console_access_enabled);

    // A settings row written before this field existed (no `cloud.
    // console_access_enabled` key at all) must deserialize as off
    // rather than failing the whole settings read.
    let legacy = AppSettings::from_json(serde_json::json!({
        "cloud": {
            "backend_url": "https://cloud.example.com",
            "telemetry_enabled": true,
            "backups_enabled": false,
            "notifications_enabled": false
        }
    }));
    assert!(!legacy.cloud.console_access_enabled);
    assert!(legacy.cloud.telemetry_enabled);
}

#[test]
fn legacy_settings_get_24_hour_observability_compression_defaults() {
    let parsed = AppSettings::from_json(serde_json::json!({
        "external_url": "https://paas.example.com",
        "preview_domain": "localho.st"
    }));

    assert_eq!(parsed.observability_compression.proxy_logs_after_hours, 24);
    assert_eq!(parsed.observability_compression.otel_spans_after_hours, 24);
}

#[test]
fn observability_compression_round_trips_through_json() {
    let mut settings = AppSettings::default();
    settings.observability_compression.proxy_logs_after_hours = 12;
    settings.observability_compression.otel_spans_after_hours = 48;

    let parsed = AppSettings::from_json(settings.to_json());
    assert_eq!(parsed.observability_compression.proxy_logs_after_hours, 12);
    assert_eq!(parsed.observability_compression.otel_spans_after_hours, 48);
}

#[test]
fn request_timeouts_default_is_no_timeout_opt_in_only() {
    // No traffic-class default applies a timeout out of the box — an
    // existing app with a slow endpoint or long-lived connection must
    // keep working exactly as it did before this setting existed.
    // Timeouts are opt-in: an operator sets a nonzero global default
    // and/or a project/environment sets its own override. The ceiling
    // stays at a sane value because it only ever constrains a timeout
    // that's actually configured — it can't create one on its own.
    let s = RequestTimeoutSettings::default();
    assert_eq!(s.max_request_timeout_seconds, 600);
    assert_eq!(s.default_http_timeout_seconds, 0);
    assert_eq!(s.default_sse_idle_timeout_seconds, 0);
    assert_eq!(s.default_websocket_idle_timeout_seconds, 0);
}

#[test]
fn request_timeouts_ceiling_clamps_out_of_range_values() {
    let mut s = RequestTimeoutSettings {
        max_request_timeout_seconds: 0,
        ..RequestTimeoutSettings::default()
    };
    assert_eq!(s.ceiling(), RequestTimeoutSettings::MIN_CEILING_SECS);

    s.max_request_timeout_seconds = u32::MAX;
    assert_eq!(s.ceiling(), RequestTimeoutSettings::MAX_CEILING_SECS);
}

#[test]
fn request_timeouts_clamp_to_ceiling_never_exceeds_ceiling() {
    let s = RequestTimeoutSettings {
        max_request_timeout_seconds: 120,
        ..RequestTimeoutSettings::default()
    };
    assert_eq!(s.clamp_to_ceiling(30), 30, "below ceiling: pass through");
    assert_eq!(s.clamp_to_ceiling(120), 120, "at ceiling: pass through");
    assert_eq!(s.clamp_to_ceiling(9000), 120, "above ceiling: clamped");
}

#[test]
fn legacy_settings_json_without_request_timeouts_deserializes() {
    // An old `settings.data` row written before this feature shipped has
    // no `request_timeouts` key. `#[serde(default)]` must fill it in
    // with the no-timeout defaults so pre-migration rows keep loading
    // with identical (i.e. unbounded) proxy behavior.
    let legacy = serde_json::json!({
        "external_url": "https://paas.example.com",
        "preview_domain": "localho.st"
    });
    let parsed = AppSettings::from_json(legacy);
    assert_eq!(parsed.request_timeouts, RequestTimeoutSettings::default());
}

#[test]
fn request_timeouts_round_trip_through_json() {
    let mut settings = AppSettings::default();
    settings.request_timeouts.max_request_timeout_seconds = 120;
    settings.request_timeouts.default_http_timeout_seconds = 30;
    settings.request_timeouts.default_sse_idle_timeout_seconds = 90;
    settings
        .request_timeouts
        .default_websocket_idle_timeout_seconds = 90;

    let parsed = AppSettings::from_json(settings.to_json());
    assert_eq!(parsed.request_timeouts, settings.request_timeouts);
}

#[test]
fn connection_limits_default_is_unlimited_opt_in_only() {
    // Out of the box, no cap is applied — an existing app with many
    // concurrent requests must keep working without any operator action.
    // The limit is opt-in: an operator sets a nonzero global default
    // and/or a project/environment sets its own override.
    let s = ConnectionLimitSettings::default();
    assert_eq!(s.default_max_concurrent_connections, 0);
}

#[test]
fn connection_limits_round_trip_through_json() {
    let mut settings = AppSettings::default();
    settings
        .connection_limits
        .default_max_concurrent_connections = 200;

    let parsed = AppSettings::from_json(settings.to_json());
    assert_eq!(
        parsed.connection_limits.default_max_concurrent_connections,
        200
    );
}

#[test]
fn legacy_settings_json_without_connection_limits_deserializes() {
    // An old `settings.data` row written before this feature shipped has
    // no `connection_limits` key. `#[serde(default)]` must fill it in
    // with the unlimited default so pre-existing deployments aren't
    // suddenly capped on upgrade.
    let legacy = serde_json::json!({
        "external_url": "https://paas.example.com",
        "preview_domain": "localho.st"
    });
    let parsed = AppSettings::from_json(legacy);
    assert_eq!(parsed.connection_limits, ConnectionLimitSettings::default());
    assert_eq!(
        parsed.connection_limits.default_max_concurrent_connections,
        0
    );
}

/// Regression: a settings save must not delete the `admin_gate`
/// sub-document. The console writes `console_version` through
/// `update_setting_field` on every startup; with a plain `to_json()`
/// overwrite that wiped the operator's admin allowlist and reopened the
/// management surface to every host on each restart.
#[test]
fn merge_preserves_foreign_admin_gate_subdocument() {
    let existing = serde_json::json!({
        "preview_domain": "temps.kfs.es",
        "admin_gate": {
            "allowed_ips": ["10.0.0.0/8"],
            "allowed_hosts": ["app.temps.kfs.es"],
            "trust_forwarded_for": false
        }
    });

    let mut settings = AppSettings::from_json(existing.clone());
    settings.console_version = Some("v0.1.0".to_string());

    let merged = settings.to_json_merged(&existing);

    assert_eq!(
        merged.get("admin_gate"),
        existing.get("admin_gate"),
        "an unrelated settings write must not drop the admin_gate sub-document"
    );
    assert_eq!(
        merged.get("console_version").and_then(|v| v.as_str()),
        Some("v0.1.0"),
    );
    assert_eq!(
        merged.get("preview_domain").and_then(|v| v.as_str()),
        Some("temps.kfs.es"),
    );
}

/// Keys `AppSettings` owns must still be updatable — including back to
/// their default value — so the merge cannot simply prefer the stored blob.
#[test]
fn merge_lets_owned_fields_win_over_stored_values() {
    let existing = serde_json::json!({
        "preview_domain": "old.example.com",
        "insecure_tls": true,
        "admin_gate": { "allowed_hosts": ["app.example.com"] }
    });

    let mut settings = AppSettings::from_json(existing.clone());
    settings.preview_domain = "new.example.com".to_string();
    settings.insecure_tls = false;

    let merged = settings.to_json_merged(&existing);

    assert_eq!(
        merged.get("preview_domain").and_then(|v| v.as_str()),
        Some("new.example.com"),
    );
    assert_eq!(
        merged.get("insecure_tls").and_then(|v| v.as_bool()),
        Some(false),
        "a field must be settable back to its default value"
    );
    assert!(merged.get("admin_gate").is_some());
}

/// A fresh/corrupt row has nothing to preserve — the serialized settings
/// become the whole document.
#[test]
fn merge_falls_back_to_plain_serialization_for_non_object_blob() {
    let settings = AppSettings::default();
    let merged = settings.to_json_merged(&serde_json::Value::Null);
    assert_eq!(merged, settings.to_json());
}

#[test]
fn fresh_multi_node_settings_require_mtls() {
    assert!(MultiNodeSettings::default().require_mtls);
    assert!(AppSettings::default().multi_node.require_mtls);
}

#[test]
fn legacy_multi_node_settings_without_mtls_field_remain_plaintext_until_migrated() {
    let legacy = serde_json::json!({
        "multi_node": {
            "legacy_shared_token_enabled": true
        }
    });
    let parsed = AppSettings::from_json(legacy);
    assert!(!parsed.multi_node.require_mtls);
}

#[test]
fn platform_identity_is_hidden_by_default_and_exposure_can_be_enabled() {
    assert!(!AppSettings::default().expose_platform_identity());
    for json in [
        r#"{}"#,
        r#"{"expose_platform_identity":null}"#,
        r#"{"expose_platform_identity":false}"#,
    ] {
        let settings: AppSettings = serde_json::from_str(json).unwrap();
        assert!(!settings.expose_platform_identity());
    }
    let settings: AppSettings =
        serde_json::from_str(r#"{"expose_platform_identity":true}"#).unwrap();
    assert!(settings.expose_platform_identity());
}

#[test]
fn malformed_settings_preserve_platform_identity_choice() {
    for enabled in [false, true] {
        let settings = AppSettings::from_json(serde_json::json!({
            "geo": "not an object",
            "expose_platform_identity": enabled
        }));
        assert_eq!(settings.expose_platform_identity(), enabled);
    }
    for flag in [serde_json::Value::Null, serde_json::json!("invalid")] {
        let settings = AppSettings::from_json(serde_json::json!({
            "geo": "not an object",
            "expose_platform_identity": flag
        }));
        assert!(!settings.expose_platform_identity());
    }
}
