-- ClickHouse counterpart of backfill-ai-agent-bot-names.sql.
--
-- WHY A SEPARATE SCRIPT
-- ----------------------
-- `proxy_logs` has two storage backends behind `ProxyLogStorage`
-- (crates/temps-proxy/src/storage/): TimescaleDB (Postgres) and ClickHouse,
-- selected at startup by whether TEMPS_CLICKHOUSE_* is set
-- (ServerConfig::is_clickhouse_enabled()). Deployments on the ClickHouse path
-- have their own `proxy_logs` table (migrations/clickhouse/0001_proxy_logs.sql)
-- with different SQL syntax, a different retention mechanism (native TTL, not
-- a Timescale hypertable), and no shared row store with the Postgres path — so
-- the Postgres backfill script's UPDATE statement never runs against this data.
-- This script is that same fix, in ClickHouse's dialect.
--
-- WHY THIS IS A MANUAL SCRIPT, NOT CODE
-- --------------------------------------
-- Same reasoning as the Postgres version: this is a one-off historical-data
-- fix for pre-existing rows so the AI Agents analytics page has history. The
-- going-forward code fix (ai_agent_detector::detect, called before storage in
-- both proxy_log_batch_writer.rs and proxy_log_service.rs) already classifies
-- all NEW rows correctly on both backends.
--
-- USAGE
-- -----
--   clickhouse-client \
--     --host "$TEMPS_CLICKHOUSE_HOST" \
--     --user "$TEMPS_CLICKHOUSE_USER" \
--     --password "$TEMPS_CLICKHOUSE_PASSWORD" \
--     --database "$TEMPS_CLICKHOUSE_DATABASE" \
--     --multiquery < scripts/backfill-ai-agent-bot-names-clickhouse.sql
--
-- MUTATION, NOT MERGE
-- --------------------
-- `ALTER TABLE ... UPDATE` is a ClickHouse "mutation": it rewrites affected
-- parts in the background (asynchronous — track progress in
-- `system.mutations WHERE table = 'proxy_logs'`), not a synchronous row-level
-- UPDATE. This does NOT touch `_version` or the ReplacingMergeTree dedup key
-- (project_id, timestamp, request_id) — it modifies bot_name/is_bot in place
-- on existing parts, so there's no risk of it interacting with merge-time
-- row replacement. It IS idempotent (safe to re-run): the WHERE clause only
-- matches rows whose bot_name doesn't already equal the freshly computed
-- value, mirroring the Postgres script's `IS DISTINCT FROM` guard.
--
-- The `timestamp >= now() - INTERVAL 7 DAY` scope is not a compression
-- workaround like the Postgres version's chunk-exclusion reasoning (ClickHouse
-- has no equivalent to Timescale's compressed-chunk immutability) — it exists
-- purely to bound mutation cost. Widen it (up to the table's 30-day TTL; rows
-- older than that are already gone, see 0001_proxy_logs.sql) if you want a
-- full-table pass.
--
-- REGEX SYNTAX
-- ------------
-- ClickHouse `match()` uses RE2, the same engine Rust's `regex` crate uses, so
-- the patterns below are copied verbatim from
-- temps_proxy::ai_agent_detector::AGENT_PATTERNS (33 agents / 21 providers,
-- same specificity order — e.g. Applebot-Extended before Applebot,
-- OAI-SearchBot before openai/) with only the string-literal escaping changed
-- for ClickHouse's single-quoted strings (`\\b` here is the same `\b` Rust
-- writes as `\b` inside a raw string).
--
-- VERIFIED against a live ClickHouse 26.2.5 instance (the version pinned in
-- clickhouse.rs's own header comment): applied the real
-- migrations/clickhouse/0001_proxy_logs.sql schema in a scratch database,
-- inserted rows covering a real Googlebot UA, GPTBot, GoogleOther, a plain
-- browser, and Googlebot-Image, then ran STEP 1 and STEP 2 against them.
-- Confirmed: correct classification per row (including GoogleOther not
-- being misdetected as Googlebot or vice versa), the plain browser row left
-- untouched, `\bGooglebot\b` also catching Googlebot-Image (same word-boundary
-- behavior as the Rust regex — expected, not a bug), and re-running STEP 2 a
-- second time changed nothing (idempotent). Not run against production data —
-- this was a throwaway scratch database, dropped after verification.


-- ----------------------------------------------------------------------------
-- STEP 1 (optional, read-only): preview what WOULD change. Run this first.
-- ----------------------------------------------------------------------------
-- SELECT detected, count() AS would_change
-- FROM (
--     SELECT
--         bot_name,
--         multiIf(
--             match(user_agent, '(?i)\\bGPTBot\\b'),                       'GPTBot',
--             match(user_agent, '(?i)\\bOAI-SearchBot\\b'),                'OAI-SearchBot',
--             match(user_agent, '(?i)\\bChatGPT-User\\b'),                 'ChatGPT-User',
--             match(user_agent, 'openai/'),                                'OpenAI',
--             match(user_agent, '(?i)\\bClaudeBot\\b'),                    'ClaudeBot',
--             match(user_agent, '(?i)\\bClaude-SearchBot\\b'),             'Claude-SearchBot',
--             match(user_agent, '(?i)\\bClaude-User\\b'),                  'Claude-User',
--             match(user_agent, '(?i)\\banthropic-ai\\b'),                 'anthropic-ai',
--             match(user_agent, '(?i)\\bPerplexityBot\\b'),                'PerplexityBot',
--             match(user_agent, '(?i)\\bPerplexity-User\\b'),              'Perplexity-User',
--             match(user_agent, '(?i)\\bGoogleOther\\b'),                  'GoogleOther',
--             match(user_agent, '(?i)\\bGooglebot\\b'),                    'Googlebot',
--             match(user_agent, '(?i)\\bApplebot-Extended\\b'),            'Applebot-Extended',
--             match(user_agent, '(?i)\\bApplebot\\b'),                     'Applebot',
--             match(user_agent, '(?i)\\bMeta-ExternalAgent\\b'),           'Meta-ExternalAgent',
--             match(user_agent, '(?i)\\bMeta-ExternalFetcher\\b'),         'Meta-ExternalFetcher',
--             match(user_agent, '(?i)\\bAmazonbot\\b'),                    'Amazonbot',
--             match(user_agent, '(?i)\\bBytespider\\b'),                   'Bytespider',
--             match(user_agent, '(?i)\\bCCBot\\b'),                        'CCBot',
--             match(user_agent, '(?i)\\bcohere-training-data-crawler\\b'), 'cohere-training-data-crawler',
--             match(user_agent, '(?i)\\bcohere-ai\\b'),                    'cohere-ai',
--             match(user_agent, '(?i)\\bDiffbot\\b'),                      'Diffbot',
--             match(user_agent, '(?i)\\bYouBot\\b'),                       'YouBot',
--             match(user_agent, '(?i)\\bDuckAssistBot\\b'),                'DuckAssistBot',
--             match(user_agent, '(?i)\\bBravebot\\b'),                     'Bravebot',
--             match(user_agent, '(?i)\\bAndibot\\b'),                      'Andibot',
--             match(user_agent, '(?i)\\bOmgilibot\\b'),                    'Omgilibot',
--             match(user_agent, '(?i)\\bomgili\\b'),                       'Omgili',
--             match(user_agent, '(?i)\\bImagesiftBot\\b'),                 'ImagesiftBot',
--             match(user_agent, '(?i)\\bTimpibot\\b'),                     'Timpibot',
--             match(user_agent, '(?i)\\bKangaroo Bot\\b'),                 'Kangaroo Bot',
--             match(user_agent, '(?i)\\bMistralAI-User\\b'),               'MistralAI-User',
--             match(user_agent, '(?i)\\bGrokBot\\b'),                      'GrokBot',
--             NULL
--         ) AS detected
--     FROM proxy_logs
--     WHERE timestamp >= now() - INTERVAL 7 DAY AND user_agent != ''
-- )
-- WHERE detected IS NOT NULL AND bot_name != detected
-- GROUP BY detected ORDER BY would_change DESC;


-- ----------------------------------------------------------------------------
-- STEP 2: the actual backfill.
-- ----------------------------------------------------------------------------
ALTER TABLE proxy_logs
UPDATE
    bot_name = multiIf(
        match(user_agent, '(?i)\\bGPTBot\\b'),                       'GPTBot',
        match(user_agent, '(?i)\\bOAI-SearchBot\\b'),                'OAI-SearchBot',
        match(user_agent, '(?i)\\bChatGPT-User\\b'),                 'ChatGPT-User',
        match(user_agent, 'openai/'),                                'OpenAI',
        match(user_agent, '(?i)\\bClaudeBot\\b'),                    'ClaudeBot',
        match(user_agent, '(?i)\\bClaude-SearchBot\\b'),             'Claude-SearchBot',
        match(user_agent, '(?i)\\bClaude-User\\b'),                  'Claude-User',
        match(user_agent, '(?i)\\banthropic-ai\\b'),                 'anthropic-ai',
        match(user_agent, '(?i)\\bPerplexityBot\\b'),                'PerplexityBot',
        match(user_agent, '(?i)\\bPerplexity-User\\b'),              'Perplexity-User',
        match(user_agent, '(?i)\\bGoogleOther\\b'),                  'GoogleOther',
        match(user_agent, '(?i)\\bGooglebot\\b'),                    'Googlebot',
        match(user_agent, '(?i)\\bApplebot-Extended\\b'),            'Applebot-Extended',
        match(user_agent, '(?i)\\bApplebot\\b'),                     'Applebot',
        match(user_agent, '(?i)\\bMeta-ExternalAgent\\b'),           'Meta-ExternalAgent',
        match(user_agent, '(?i)\\bMeta-ExternalFetcher\\b'),         'Meta-ExternalFetcher',
        match(user_agent, '(?i)\\bAmazonbot\\b'),                    'Amazonbot',
        match(user_agent, '(?i)\\bBytespider\\b'),                   'Bytespider',
        match(user_agent, '(?i)\\bCCBot\\b'),                        'CCBot',
        match(user_agent, '(?i)\\bcohere-training-data-crawler\\b'), 'cohere-training-data-crawler',
        match(user_agent, '(?i)\\bcohere-ai\\b'),                    'cohere-ai',
        match(user_agent, '(?i)\\bDiffbot\\b'),                      'Diffbot',
        match(user_agent, '(?i)\\bYouBot\\b'),                       'YouBot',
        match(user_agent, '(?i)\\bDuckAssistBot\\b'),                'DuckAssistBot',
        match(user_agent, '(?i)\\bBravebot\\b'),                     'Bravebot',
        match(user_agent, '(?i)\\bAndibot\\b'),                      'Andibot',
        match(user_agent, '(?i)\\bOmgilibot\\b'),                    'Omgilibot',
        match(user_agent, '(?i)\\bomgili\\b'),                       'Omgili',
        match(user_agent, '(?i)\\bImagesiftBot\\b'),                 'ImagesiftBot',
        match(user_agent, '(?i)\\bTimpibot\\b'),                     'Timpibot',
        match(user_agent, '(?i)\\bKangaroo Bot\\b'),                 'Kangaroo Bot',
        match(user_agent, '(?i)\\bMistralAI-User\\b'),               'MistralAI-User',
        match(user_agent, '(?i)\\bGrokBot\\b'),                      'GrokBot',
        bot_name
    ),
    is_bot = 1
WHERE timestamp >= now() - INTERVAL 7 DAY
  AND user_agent != ''
  AND multiMatchAny(user_agent, [
        '(?i)\\bGPTBot\\b', '(?i)\\bOAI-SearchBot\\b', '(?i)\\bChatGPT-User\\b', 'openai/',
        '(?i)\\bClaudeBot\\b', '(?i)\\bClaude-SearchBot\\b', '(?i)\\bClaude-User\\b', '(?i)\\banthropic-ai\\b',
        '(?i)\\bPerplexityBot\\b', '(?i)\\bPerplexity-User\\b',
        '(?i)\\bGoogleOther\\b', '(?i)\\bGooglebot\\b',
        '(?i)\\bApplebot-Extended\\b', '(?i)\\bApplebot\\b',
        '(?i)\\bMeta-ExternalAgent\\b', '(?i)\\bMeta-ExternalFetcher\\b',
        '(?i)\\bAmazonbot\\b', '(?i)\\bBytespider\\b', '(?i)\\bCCBot\\b',
        '(?i)\\bcohere-training-data-crawler\\b', '(?i)\\bcohere-ai\\b',
        '(?i)\\bDiffbot\\b', '(?i)\\bYouBot\\b', '(?i)\\bDuckAssistBot\\b', '(?i)\\bBravebot\\b',
        '(?i)\\bAndibot\\b', '(?i)\\bOmgilibot\\b', '(?i)\\bomgili\\b', '(?i)\\bImagesiftBot\\b',
        '(?i)\\bTimpibot\\b', '(?i)\\bKangaroo Bot\\b', '(?i)\\bMistralAI-User\\b', '(?i)\\bGrokBot\\b'
      ]) = 1
  AND bot_name != multiIf(
        match(user_agent, '(?i)\\bGPTBot\\b'),                       'GPTBot',
        match(user_agent, '(?i)\\bOAI-SearchBot\\b'),                'OAI-SearchBot',
        match(user_agent, '(?i)\\bChatGPT-User\\b'),                 'ChatGPT-User',
        match(user_agent, 'openai/'),                                'OpenAI',
        match(user_agent, '(?i)\\bClaudeBot\\b'),                    'ClaudeBot',
        match(user_agent, '(?i)\\bClaude-SearchBot\\b'),             'Claude-SearchBot',
        match(user_agent, '(?i)\\bClaude-User\\b'),                  'Claude-User',
        match(user_agent, '(?i)\\banthropic-ai\\b'),                 'anthropic-ai',
        match(user_agent, '(?i)\\bPerplexityBot\\b'),                'PerplexityBot',
        match(user_agent, '(?i)\\bPerplexity-User\\b'),              'Perplexity-User',
        match(user_agent, '(?i)\\bGoogleOther\\b'),                  'GoogleOther',
        match(user_agent, '(?i)\\bGooglebot\\b'),                    'Googlebot',
        match(user_agent, '(?i)\\bApplebot-Extended\\b'),            'Applebot-Extended',
        match(user_agent, '(?i)\\bApplebot\\b'),                     'Applebot',
        match(user_agent, '(?i)\\bMeta-ExternalAgent\\b'),           'Meta-ExternalAgent',
        match(user_agent, '(?i)\\bMeta-ExternalFetcher\\b'),         'Meta-ExternalFetcher',
        match(user_agent, '(?i)\\bAmazonbot\\b'),                    'Amazonbot',
        match(user_agent, '(?i)\\bBytespider\\b'),                   'Bytespider',
        match(user_agent, '(?i)\\bCCBot\\b'),                        'CCBot',
        match(user_agent, '(?i)\\bcohere-training-data-crawler\\b'), 'cohere-training-data-crawler',
        match(user_agent, '(?i)\\bcohere-ai\\b'),                    'cohere-ai',
        match(user_agent, '(?i)\\bDiffbot\\b'),                      'Diffbot',
        match(user_agent, '(?i)\\bYouBot\\b'),                       'YouBot',
        match(user_agent, '(?i)\\bDuckAssistBot\\b'),                'DuckAssistBot',
        match(user_agent, '(?i)\\bBravebot\\b'),                     'Bravebot',
        match(user_agent, '(?i)\\bAndibot\\b'),                      'Andibot',
        match(user_agent, '(?i)\\bOmgilibot\\b'),                    'Omgilibot',
        match(user_agent, '(?i)\\bomgili\\b'),                       'Omgili',
        match(user_agent, '(?i)\\bImagesiftBot\\b'),                 'ImagesiftBot',
        match(user_agent, '(?i)\\bTimpibot\\b'),                     'Timpibot',
        match(user_agent, '(?i)\\bKangaroo Bot\\b'),                 'Kangaroo Bot',
        match(user_agent, '(?i)\\bMistralAI-User\\b'),               'MistralAI-User',
        match(user_agent, '(?i)\\bGrokBot\\b'),                      'GrokBot',
        bot_name
      );

-- ----------------------------------------------------------------------------
-- STEP 3 (optional): confirm. Should now show canonical names, including
-- Googlebot. Note FINAL is needed for an exact count on a ReplacingMergeTree
-- if a merge hasn't run yet, but bot_name/is_bot are updated in-place by the
-- mutation regardless, so plain SELECT already reflects it.
-- ----------------------------------------------------------------------------
-- SELECT bot_name, count()
-- FROM proxy_logs
-- WHERE timestamp >= now() - INTERVAL 7 DAY
--   AND bot_name IN ('ClaudeBot','GPTBot','OAI-SearchBot','PerplexityBot','CCBot',
--                    'Applebot','Applebot-Extended','Amazonbot','Bytespider',
--                    'Meta-ExternalAgent','Googlebot')
-- GROUP BY bot_name ORDER BY 2 DESC;
