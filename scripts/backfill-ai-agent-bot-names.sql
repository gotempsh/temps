-- Backfill proxy_logs.bot_name with canonical AI-agent names.
--
-- This is the TimescaleDB (Postgres) path only. Deployments running the
-- ClickHouse ProxyLogStorage backend (TEMPS_CLICKHOUSE_* set) have a
-- separate proxy_logs table with different SQL syntax — use
-- backfill-ai-agent-bot-names-clickhouse.sql for those.
--
-- WHY THIS IS A MANUAL SCRIPT, NOT A MIGRATION
-- --------------------------------------------
-- Sea-ORM migrations run inside `establish_connection` (Migrator::up) which
-- executes BEFORE the Pingora proxy binds its listeners. A full-table UPDATE
-- there would block proxy startup -- the exact thing the fast-LB-bind work
-- avoids. So this backfill is intentionally a one-off you run by hand against
-- the database, decoupled from server startup.
--
-- The going-forward code fix (ProxyLogBatchWriter now runs ai_agent_detector)
-- classifies all NEW rows correctly; this script only fixes pre-existing rows
-- so the AI Agents analytics page has history.
--
-- USAGE
-- -----
--   psql "$TEMPS_DATABASE_URL" -f scripts/backfill-ai-agent-bot-names.sql
--
-- It is idempotent (safe to re-run) and read-mostly: only rows matching an AI
-- agent whose bot_name is wrong get written.
--
-- TIMESCALEDB NOTES
-- -----------------
-- proxy_logs is a hypertable: chunks older than 7 days are COMPRESSED (and
-- cannot be modified without decompression) and chunks older than 30 days are
-- dropped by retention. So:
--   * We scope to `timestamp >= now() - interval '7 days'` so chunk exclusion
--     skips compressed chunks entirely.
--   * We use a PLAIN single-table UPDATE with the AI regex in the WHERE clause.
--     Do NOT rewrite this as `UPDATE ... FROM (SELECT ... FROM proxy_logs ...)`:
--     the self-join form materialises a scan across chunk boundaries and forces
--     decompression, which aborts with "tuple decompression limit exceeded" on
--     a compressed hypertable.
--
-- The CASE mirrors temps_proxy::ai_agent_detector::AGENT_PATTERNS exactly
-- (33 agents / 21 providers) in the same specificity order (e.g.
-- Applebot-Extended before Applebot, OAI-SearchBot before openai/). Postgres
-- `~*` is case-insensitive POSIX regex; `\y` is the word boundary (Rust `\b`).


-- ----------------------------------------------------------------------------
-- STEP 1 (optional, read-only): preview what WOULD change. Run this first.
-- ----------------------------------------------------------------------------
-- SELECT detected, count(*) AS would_change,
--        array_agg(DISTINCT bot_name) AS current_values
-- FROM (
--   SELECT bot_name, (
--     CASE
--         WHEN user_agent ~* '\yGPTBot\y'                       THEN 'GPTBot'
--         WHEN user_agent ~* '\yOAI-SearchBot\y'                THEN 'OAI-SearchBot'
--         WHEN user_agent ~* '\yChatGPT-User\y'                 THEN 'ChatGPT-User'
--         WHEN user_agent ~* 'openai/'                          THEN 'OpenAI'
--         WHEN user_agent ~* '\yClaudeBot\y'                    THEN 'ClaudeBot'
--         WHEN user_agent ~* '\yClaude-SearchBot\y'             THEN 'Claude-SearchBot'
--         WHEN user_agent ~* '\yClaude-User\y'                  THEN 'Claude-User'
--         WHEN user_agent ~* '\yanthropic-ai\y'                 THEN 'anthropic-ai'
--         WHEN user_agent ~* '\yPerplexityBot\y'                THEN 'PerplexityBot'
--         WHEN user_agent ~* '\yPerplexity-User\y'              THEN 'Perplexity-User'
--         WHEN user_agent ~* '\yGoogleOther\y'                  THEN 'GoogleOther'
        WHEN user_agent ~* '\yGooglebot\y'                    THEN 'Googlebot'
--         WHEN user_agent ~* '\yApplebot-Extended\y'            THEN 'Applebot-Extended'
--         WHEN user_agent ~* '\yApplebot\y'                     THEN 'Applebot'
--         WHEN user_agent ~* '\yMeta-ExternalAgent\y'           THEN 'Meta-ExternalAgent'
--         WHEN user_agent ~* '\yMeta-ExternalFetcher\y'         THEN 'Meta-ExternalFetcher'
--         WHEN user_agent ~* '\yAmazonbot\y'                    THEN 'Amazonbot'
--         WHEN user_agent ~* '\yBytespider\y'                   THEN 'Bytespider'
--         WHEN user_agent ~* '\yCCBot\y'                        THEN 'CCBot'
--         WHEN user_agent ~* '\ycohere-training-data-crawler\y' THEN 'cohere-training-data-crawler'
--         WHEN user_agent ~* '\ycohere-ai\y'                    THEN 'cohere-ai'
--         WHEN user_agent ~* '\yDiffbot\y'                      THEN 'Diffbot'
--         WHEN user_agent ~* '\yYouBot\y'                       THEN 'YouBot'
--         WHEN user_agent ~* '\yDuckAssistBot\y'                THEN 'DuckAssistBot'
--         WHEN user_agent ~* '\yBravebot\y'                     THEN 'Bravebot'
--         WHEN user_agent ~* '\yAndibot\y'                      THEN 'Andibot'
--         WHEN user_agent ~* '\yOmgilibot\y'                    THEN 'Omgilibot'
--         WHEN user_agent ~* '\yomgili\y'                       THEN 'Omgili'
--         WHEN user_agent ~* '\yImagesiftBot\y'                 THEN 'ImagesiftBot'
--         WHEN user_agent ~* '\yTimpibot\y'                     THEN 'Timpibot'
--         WHEN user_agent ~* '\yKangaroo Bot\y'                 THEN 'Kangaroo Bot'
--         WHEN user_agent ~* '\yMistralAI-User\y'               THEN 'MistralAI-User'
--         WHEN user_agent ~* '\yGrokBot\y'                      THEN 'GrokBot'
--         ELSE NULL
--     END) AS detected
--   FROM proxy_logs
--   WHERE timestamp >= now() - interval '7 days' AND user_agent IS NOT NULL
-- ) s
-- WHERE detected IS NOT NULL AND bot_name IS DISTINCT FROM detected
-- GROUP BY detected ORDER BY would_change DESC;


-- ----------------------------------------------------------------------------
-- STEP 2: the actual backfill.
-- ----------------------------------------------------------------------------
UPDATE proxy_logs
SET bot_name = (
    CASE
        WHEN user_agent ~* '\yGPTBot\y'                       THEN 'GPTBot'
        WHEN user_agent ~* '\yOAI-SearchBot\y'                THEN 'OAI-SearchBot'
        WHEN user_agent ~* '\yChatGPT-User\y'                 THEN 'ChatGPT-User'
        WHEN user_agent ~* 'openai/'                          THEN 'OpenAI'
        WHEN user_agent ~* '\yClaudeBot\y'                    THEN 'ClaudeBot'
        WHEN user_agent ~* '\yClaude-SearchBot\y'             THEN 'Claude-SearchBot'
        WHEN user_agent ~* '\yClaude-User\y'                  THEN 'Claude-User'
        WHEN user_agent ~* '\yanthropic-ai\y'                 THEN 'anthropic-ai'
        WHEN user_agent ~* '\yPerplexityBot\y'                THEN 'PerplexityBot'
        WHEN user_agent ~* '\yPerplexity-User\y'              THEN 'Perplexity-User'
        WHEN user_agent ~* '\yGoogleOther\y'                  THEN 'GoogleOther'
        WHEN user_agent ~* '\yGooglebot\y'                    THEN 'Googlebot'
        WHEN user_agent ~* '\yApplebot-Extended\y'            THEN 'Applebot-Extended'
        WHEN user_agent ~* '\yApplebot\y'                     THEN 'Applebot'
        WHEN user_agent ~* '\yMeta-ExternalAgent\y'           THEN 'Meta-ExternalAgent'
        WHEN user_agent ~* '\yMeta-ExternalFetcher\y'         THEN 'Meta-ExternalFetcher'
        WHEN user_agent ~* '\yAmazonbot\y'                    THEN 'Amazonbot'
        WHEN user_agent ~* '\yBytespider\y'                   THEN 'Bytespider'
        WHEN user_agent ~* '\yCCBot\y'                        THEN 'CCBot'
        WHEN user_agent ~* '\ycohere-training-data-crawler\y' THEN 'cohere-training-data-crawler'
        WHEN user_agent ~* '\ycohere-ai\y'                    THEN 'cohere-ai'
        WHEN user_agent ~* '\yDiffbot\y'                      THEN 'Diffbot'
        WHEN user_agent ~* '\yYouBot\y'                       THEN 'YouBot'
        WHEN user_agent ~* '\yDuckAssistBot\y'                THEN 'DuckAssistBot'
        WHEN user_agent ~* '\yBravebot\y'                     THEN 'Bravebot'
        WHEN user_agent ~* '\yAndibot\y'                      THEN 'Andibot'
        WHEN user_agent ~* '\yOmgilibot\y'                    THEN 'Omgilibot'
        WHEN user_agent ~* '\yomgili\y'                       THEN 'Omgili'
        WHEN user_agent ~* '\yImagesiftBot\y'                 THEN 'ImagesiftBot'
        WHEN user_agent ~* '\yTimpibot\y'                     THEN 'Timpibot'
        WHEN user_agent ~* '\yKangaroo Bot\y'                 THEN 'Kangaroo Bot'
        WHEN user_agent ~* '\yMistralAI-User\y'               THEN 'MistralAI-User'
        WHEN user_agent ~* '\yGrokBot\y'                      THEN 'GrokBot'
        ELSE NULL
    END
    ),
    is_bot = true
WHERE timestamp >= now() - interval '7 days'
  AND user_agent IS NOT NULL
  AND user_agent ~* '\y(GPTBot|OAI-SearchBot|ChatGPT-User|ClaudeBot|Claude-SearchBot|Claude-User|anthropic-ai|PerplexityBot|Perplexity-User|GoogleOther|Googlebot|Applebot-Extended|Applebot|Meta-ExternalAgent|Meta-ExternalFetcher|Amazonbot|Bytespider|CCBot|cohere-training-data-crawler|cohere-ai|Diffbot|YouBot|DuckAssistBot|Bravebot|Andibot|Omgilibot|omgili|ImagesiftBot|Timpibot|Kangaroo Bot|MistralAI-User|GrokBot)\y|openai/'
  AND bot_name IS DISTINCT FROM (
    CASE
        WHEN user_agent ~* '\yGPTBot\y'                       THEN 'GPTBot'
        WHEN user_agent ~* '\yOAI-SearchBot\y'                THEN 'OAI-SearchBot'
        WHEN user_agent ~* '\yChatGPT-User\y'                 THEN 'ChatGPT-User'
        WHEN user_agent ~* 'openai/'                          THEN 'OpenAI'
        WHEN user_agent ~* '\yClaudeBot\y'                    THEN 'ClaudeBot'
        WHEN user_agent ~* '\yClaude-SearchBot\y'             THEN 'Claude-SearchBot'
        WHEN user_agent ~* '\yClaude-User\y'                  THEN 'Claude-User'
        WHEN user_agent ~* '\yanthropic-ai\y'                 THEN 'anthropic-ai'
        WHEN user_agent ~* '\yPerplexityBot\y'                THEN 'PerplexityBot'
        WHEN user_agent ~* '\yPerplexity-User\y'              THEN 'Perplexity-User'
        WHEN user_agent ~* '\yGoogleOther\y'                  THEN 'GoogleOther'
        WHEN user_agent ~* '\yGooglebot\y'                    THEN 'Googlebot'
        WHEN user_agent ~* '\yApplebot-Extended\y'            THEN 'Applebot-Extended'
        WHEN user_agent ~* '\yApplebot\y'                     THEN 'Applebot'
        WHEN user_agent ~* '\yMeta-ExternalAgent\y'           THEN 'Meta-ExternalAgent'
        WHEN user_agent ~* '\yMeta-ExternalFetcher\y'         THEN 'Meta-ExternalFetcher'
        WHEN user_agent ~* '\yAmazonbot\y'                    THEN 'Amazonbot'
        WHEN user_agent ~* '\yBytespider\y'                   THEN 'Bytespider'
        WHEN user_agent ~* '\yCCBot\y'                        THEN 'CCBot'
        WHEN user_agent ~* '\ycohere-training-data-crawler\y' THEN 'cohere-training-data-crawler'
        WHEN user_agent ~* '\ycohere-ai\y'                    THEN 'cohere-ai'
        WHEN user_agent ~* '\yDiffbot\y'                      THEN 'Diffbot'
        WHEN user_agent ~* '\yYouBot\y'                       THEN 'YouBot'
        WHEN user_agent ~* '\yDuckAssistBot\y'                THEN 'DuckAssistBot'
        WHEN user_agent ~* '\yBravebot\y'                     THEN 'Bravebot'
        WHEN user_agent ~* '\yAndibot\y'                      THEN 'Andibot'
        WHEN user_agent ~* '\yOmgilibot\y'                    THEN 'Omgilibot'
        WHEN user_agent ~* '\yomgili\y'                       THEN 'Omgili'
        WHEN user_agent ~* '\yImagesiftBot\y'                 THEN 'ImagesiftBot'
        WHEN user_agent ~* '\yTimpibot\y'                     THEN 'Timpibot'
        WHEN user_agent ~* '\yKangaroo Bot\y'                 THEN 'Kangaroo Bot'
        WHEN user_agent ~* '\yMistralAI-User\y'               THEN 'MistralAI-User'
        WHEN user_agent ~* '\yGrokBot\y'                      THEN 'GrokBot'
        ELSE NULL
    END
    );

-- ----------------------------------------------------------------------------
-- STEP 3 (optional): confirm. Should now show canonical names.
-- ----------------------------------------------------------------------------
-- SELECT bot_name, count(*)
-- FROM proxy_logs
-- WHERE timestamp >= now() - interval '7 days'
--   AND bot_name IN ('ClaudeBot','GPTBot','OAI-SearchBot','PerplexityBot','CCBot',
--                    'Applebot','Applebot-Extended','Amazonbot','Bytespider',
--                    'Meta-ExternalAgent')
-- GROUP BY bot_name ORDER BY 2 DESC;
