<div align="center">

<picture>
  <source media="(prefers-color-scheme: dark)" srcset="web/public/logo/temps-logo-dark.png">
  <source media="(prefers-color-scheme: light)" srcset="web/public/logo/temps-logo-light.png">
  <img alt="Temps" src="web/public/logo/temps-logo-dark.png" width="280">
</picture>

**Vercel、Sentry、PostHog、Pingdom、Resend、E2B、Datadog のオープンソース代替を、セルフホストの単一バイナリで。**

[ウェブサイト](https://temps.sh/?utm_source=github&utm_medium=repo&utm_content=header_ja) · [ドキュメント](https://temps.sh/docs?utm_source=github&utm_medium=repo&utm_content=header_ja) · [ディスカッション](https://github.com/gotempsh/temps/discussions)

</div>

---

Temps は、アプリをデプロイし、その周りのすべて（データベース、アナリティクス、エラートラッキング、トレース、ログ、稼働監視、メール、AI サンドボックス、AI ゲートウェイ）を動かす、セルフホストの単一バイナリです。オープンソースで、あなた自身のサーバーで動きます。

## VPS へのインストール

```bash
curl -fsSL https://temps.sh/deploy.sh | bash
```

新しい Ubuntu 24.04 または 22.04 サーバーで実行してください。Docker がなければインストールし、PostgreSQL を起動し、自動 HTTPS 付きのサービスとして Temps をセットアップして、コンソールの URL と管理者パスワードを表示します。まだドメインがない場合は `quick` モードを選ぶと `https://console.<サーバーIP>.sslip.io` が使えます。

[クイックスタート](https://temps.sh/docs/quickstart?utm_source=github&utm_medium=repo&utm_content=install_ja) · [サーバーの選び方](https://temps.sh/docs/choosing-a-server?utm_source=github&utm_medium=repo&utm_content=install_ja) · サーバーを運用したくない場合は [Temps Cloud](https://temps.sh/pricing?utm_source=github&utm_medium=repo&utm_content=cloud_cta_ja)

<picture>
  <source media="(prefers-color-scheme: dark)" srcset="assets/screenshots/dashboard-dark.png">
  <img alt="Temps プロジェクトダッシュボード —— プロジェクトごとにまとめたサービスと、トラフィック、ステータス、最新のデプロイ" src="assets/screenshots/dashboard-light.png">
</picture>

## 機能

<table>
  <tr>
    <td width="50%" valign="top">
      <b>git push でデプロイ</b><br>
      あらゆる言語でプレビュー URL とゼロダウンタイムのロールアウト。自動検出、または Dockerfile や Docker Compose ファイルから。 <a href="https://temps.sh/docs/deployments">ドキュメント</a><br><br>
      <picture>
        <source media="(prefers-color-scheme: dark)" srcset="assets/screenshots/deployments-dark.png">
        <img alt="Temps のデプロイ一覧 — 本番リリースとプレビューブランチのコミット、作成者、ビルド時間、そして原因付きの失敗したビルド" src="assets/screenshots/deployments-light.png">
      </picture>
    </td>
    <td width="50%" valign="top">
      <b>マネージドデータベース</b><br>
      PostgreSQL、Redis、MongoDB、MariaDB、S3 互換ストレージをバックアップ付きで。 <a href="https://temps.sh/docs/set-up-managed-services">ドキュメント</a><br><br>
      <picture>
        <source media="(prefers-color-scheme: dark)" srcset="assets/screenshots/databases-dark.png">
        <img alt="Temps のデータベース — 稼働中の PostgreSQL、Redis、MongoDB、MariaDB、S3 互換サービスとリアルタイムの CPU・メモリ" src="assets/screenshots/databases-light.png">
      </picture>
    </td>
  </tr>
  <tr>
    <td width="50%" valign="top">
      <b>ドメインと TLS</b><br>
      独自ドメインとワイルドカードドメインに Let's Encrypt 証明書を自動発行。 <a href="https://temps.sh/docs/domains-ssl">ドキュメント</a><br><br>
      <picture>
        <source media="(prefers-color-scheme: dark)" srcset="assets/screenshots/domains-dark.png">
        <img alt="Temps のドメイン — 各ドメインの HTTPS 方式と証明書の有効期限" src="assets/screenshots/domains-light.png">
      </picture>
    </td>
    <td width="50%" valign="top">
      <b>Web アナリティクス</b><br>
      訪問者、ページ、流入元、ファネル、Web パフォーマンスを自分のサーバーで収集。 <a href="https://temps.sh/docs/analytics">ドキュメント</a><br><br>
      <picture>
        <source media="(prefers-color-scheme: dark)" srcset="assets/screenshots/analytics-dark.png">
        <img alt="Temps の Web アナリティクス — 訪問者、セッション、時間ごとのトラフィック" src="assets/screenshots/analytics-light.png">
      </picture>
    </td>
  </tr>
  <tr>
    <td width="50%" valign="top">
      <b>セッションリプレイ</b><br>
      訪問をそのまま再生。フォーム入力はデフォルトでマスクされます。 <a href="https://temps.sh/docs/session-replay">ドキュメント</a><br><br>
      <picture>
        <source media="(prefers-color-scheme: dark)" srcset="assets/screenshots/session-replay-dark.png">
        <img alt="Temps のセッションリプレイ — チェックアウト中のストア訪問の録画。入力はマスクされ、イベントのタイムライン付き" src="assets/screenshots/session-replay-light.png">
      </picture>
    </td>
    <td width="50%" valign="top">
      <b>エラートラッキング</b><br>
      Sentry 互換：既存の Sentry SDK を Temps に向け、エラーをコーディングエージェントに任せられます。 <a href="https://temps.sh/docs/error-tracking">ドキュメント</a><br><br>
      <picture>
        <source media="(prefers-color-scheme: dark)" srcset="assets/screenshots/errors-dark.png">
        <img alt="Temps のエラートラッキング — 全サービスのエラーとステータス、イベント数・ユーザー数、「Fix with agent」ボタン" src="assets/screenshots/errors-light.png">
      </picture>
    </td>
  </tr>
  <tr>
    <td width="50%" valign="top">
      <b>分散トレース</b><br>
      OpenTelemetry を取り込み、1 つのリクエストをサービスをまたいで失敗した span まで追跡。 <a href="https://temps.sh/docs/opentelemetry">ドキュメント</a><br><br>
      <picture>
        <source media="(prefers-color-scheme: dark)" srcset="assets/screenshots/traces-dark.png">
        <img alt="Temps の分散トレース — 2 つのサービスにまたがるチェックアウトリクエストと、失敗した span とそのエラー" src="assets/screenshots/traces-light.png">
      </picture>
    </td>
    <td width="50%" valign="top">
      <b>ログ</b><br>
      全サービスのコンテナログとリクエストログを 1 つの検索可能なビューに。レベル別のボリューム付き。 <a href="https://temps.sh/docs/logs">ドキュメント</a><br><br>
      <picture>
        <source media="(prefers-color-scheme: dark)" srcset="assets/screenshots/logs-dark.png">
        <img alt="Temps のログ — レベル別のログ量、全サービスを横断して検索できる一覧、レベルとサービスごとの件数" src="assets/screenshots/logs-light.png">
      </picture>
    </td>
  </tr>
  <tr>
    <td width="50%" valign="top">
      <b>稼働監視とアラート</b><br>
      ヘルスチェック、ステータスページ、アラートルール。 <a href="https://temps.sh/docs/monitoring">ドキュメント</a><br><br>
      <picture>
        <source media="(prefers-color-scheme: dark)" srcset="assets/screenshots/uptime-dark.png">
        <img alt="Temps の稼働監視 — 現在のステータス、稼働率、平均応答時間、時間ごとのステータスタイムライン" src="assets/screenshots/uptime-light.png">
      </picture>
    </td>
    <td width="50%" valign="top">
      <b>トランザクションメール</b><br>
      DKIM 付きで SMTP または SES から送信し、各メッセージの配信・開封・クリックを確認。 <a href="https://temps.sh/docs/transactional-email">ドキュメント</a><br><br>
      <picture>
        <source media="(prefers-color-scheme: dark)" srcset="assets/screenshots/email-dark.png">
        <img alt="Temps のメール — 送信数と失敗数の合計、最近のメッセージとステータス・開封・クリック" src="assets/screenshots/email-light.png">
      </picture>
    </td>
  </tr>
  <tr>
    <td width="50%" valign="top">
      <b>AI サンドボックス</b><br>
      コードを実行するための隔離された Docker / Firecracker サンドボックス。ターミナルとプレビュー URL 付き。 <a href="https://temps.sh/docs/sandboxes">ドキュメント</a><br><br>
      <picture>
        <source media="(prefers-color-scheme: dark)" srcset="assets/screenshots/sandbox-detail-dark.png">
        <img alt="Temps のサンドボックス詳細 — ブラウザ内のライブターミナル、残り時間、プレビュー URL テンプレートを備えた稼働中の Docker サンドボックス" src="assets/screenshots/sandbox-detail-light.png">
      </picture>
    </td>
    <td width="50%" valign="top">
      <b>AI ゲートウェイ</b><br>
      自分のプロバイダーキーのための OpenAI 互換エンドポイント。モデルごとの使用量とコスト付き。 <a href="https://temps.sh/docs/ai-gateway">ドキュメント</a><br><br>
      <picture>
        <source media="(prefers-color-scheme: dark)" srcset="assets/screenshots/ai-usage-dark.png">
        <img alt="Temps の AI ゲートウェイ使用量 — プロバイダー横断のリクエスト数、トークン、レイテンシ、エラー率、推定コスト" src="assets/screenshots/ai-usage-light.png">
      </picture>
    </td>
  </tr>
  <tr>
    <td width="50%" valign="top">
      <b>AI チャット</b><br>
      プロジェクトについて質問すると、そのプロジェクト自身のエラー、ログ、トレースから回答します。変更を許可しない限り読み取り専用です。<br><br>
      <picture>
        <source media="(prefers-color-scheme: dark)" srcset="assets/screenshots/ai-chat-dark.png">
        <img alt="Temps の AI チャットが、プロジェクト自身のエラー、リクエストログ、トレースからチェックアウト失敗の原因を説明し、修正案を提示" src="assets/screenshots/ai-chat-light.png">
      </picture>
    </td>
    <td width="50%" valign="top">
      <b>インポート</b><br>
      アプリ、データ込みのデータベース、ドメイン、環境変数を Kubernetes、Coolify、Dokploy、CapRover、Portainer、Kamal、または Docker コンテナから、Vercel からは <code>temps migrate</code> で取り込めます。 <a href="https://temps.sh/docs/import-existing-platform">ドキュメント</a><br><br>
      <picture>
        <source media="(prefers-color-scheme: dark)" srcset="assets/screenshots/import-dark.png">
        <img alt="Temps のインポートウィザード — 移行元を選択：Kubernetes、CapRover、Portainer、Kamal、Dokploy、Coolify、Docker コンテナ" src="assets/screenshots/import-light.png">
      </picture>
    </td>
  </tr>
</table>

## AI で始める

コンソールのすべての操作は CLI コマンドとしても使えます。Temps には、コーディングエージェント（Claude Code、Codex、Cursor、OpenCode など）にその使い方を教えるスキルが付属しています。エージェントを接続して、セットアップを任せましょう：

```bash
bunx skills add gotempsh/temps --skill temps
bunx @temps-sdk/cli login https://console.your-server.example
```

次に、エージェントにこう頼みます：

> Use the Temps skill to verify my connection and list the services on this instance. Do not make any changes.

> Use the Temps skill to deploy this repository to Temps and tell me its URL when it is live.

無人で動かすエージェントには、コンソールの **Connect your coding agent**（`/setup/ai`）を開いてください。専用の API キーを作成し、エージェントの最初のリクエストが届いたかを確認します。

[スキル](https://temps.sh/docs/skills) · [CLI](https://temps.sh/docs/cli) · [MCP サーバー](https://temps.sh/docs/mcp-servers)

## ドキュメント

そのほかのすべてはドキュメントにあります： **[temps.sh/docs](https://temps.sh/docs?utm_source=github&utm_medium=repo&utm_content=docs_ja)**:

- [はじめてのデプロイ](https://temps.sh/docs/your-first-deployment)
- [ほかのプラットフォームからのインポート](https://temps.sh/docs/import-existing-platform)
- [SDK](https://temps.sh/docs/sdks) —— アナリティクス、エラートラッキング、KV、Blob ストレージ
- [本番運用チェックリスト](https://temps.sh/docs/production-checklist) と [アップグレード](https://temps.sh/docs/upgrade-temps)
- [Temps と他サービスの比較](https://temps.sh/compare?utm_source=github&utm_medium=repo&utm_content=compare_ja) （Vercel、Coolify、Dokploy など）

## テレメトリ

Temps は、セルフホスト環境が正しく動いているかをメンテナーが把握できるよう、匿名の利用イベント（ランダムなインスタンス ID、`deploy_succeeded` などのイベント名、件数）を送信します。メールアドレス、IP、リポジトリ名、ドメイン、入力内容は一切送信せず、アプリケーションのデータがサーバーの外に出ることはありません。**Settings › Telemetry** で無効にするか、ホスト全体では `TEMPS_TELEMETRY=0` を設定してください。 [詳細](https://temps.sh/docs/data-ownership-and-privacy#anonymous-telemetry)

## コミュニティと貢献

質問やアイデアは [Discussions](https://github.com/gotempsh/temps/discussions) へ、バグは [Issues](https://github.com/gotempsh/temps/issues) へ。貢献を歓迎します：[CONTRIBUTING.md](CONTRIBUTING.md) をご覧ください。

## ライセンス

[MIT](LICENSE-MIT) または [Apache 2.0](LICENSE) のデュアルライセンスです。フォークや派生物は、名称を変えたものや商用の配布も含めて歓迎します。ただし、選んだライセンスが求める著作権表示、ライセンス表示、帰属表示は残してください。[NOTICE](NOTICE) を参照してください。

---

<div align="center">

[temps.sh](https://temps.sh/?utm_source=github&utm_medium=repo&utm_content=footer_ja) | [ドキュメント](https://temps.sh/docs?utm_source=github&utm_medium=repo&utm_content=footer_ja) | [GitHub](https://github.com/gotempsh/temps)

[English](README.md) | [简体中文](README.zh-CN.md) | [Español](README.es.md) | [Français](README.fr.md) | [Deutsch](README.de.md) | 日本語 | [Português](README.pt-BR.md)

</div>
