<div align="center">

<picture>
  <source media="(prefers-color-scheme: dark)" srcset="web/public/logo/temps-logo-dark.png">
  <source media="(prefers-color-scheme: light)" srcset="web/public/logo/temps-logo-light.png">
  <img alt="Temps" src="web/public/logo/temps-logo-dark.png" width="280">
</picture>

**Vercel + Sentry + PostHog + Pingdom + Resend + E2B のオープンソース代替。**
デプロイ、アナリティクス、セッションリプレイ、エラートラッキング、アップタイム監視、トランザクションメール、AI サンドボックス —— すべてをセルフホストの単一バイナリで。

**AI ネイティブ:** 440 以上の CLI 操作と、Claude Code・Codex・OpenCode にそのまま入るスキル。

[![License](https://img.shields.io/badge/license-MIT%20OR%20Apache--2.0-blue.svg)](LICENSE)
[![GitHub Release](https://img.shields.io/github/v/release/gotempsh/temps)](https://github.com/gotempsh/temps/releases)
[![Rust](https://img.shields.io/badge/rust-1.70%2B-orange.svg)](https://www.rust-lang.org/)
[![GitHub Stars](https://img.shields.io/github/stars/gotempsh/temps?style=social)](https://github.com/gotempsh/temps)

[ウェブサイト](https://temps.sh/?utm_source=github&utm_medium=repo&utm_content=header_ja) · [ドキュメント](https://temps.sh/docs?utm_source=github&utm_medium=repo&utm_content=header_ja) · [クイックスタート](https://temps.sh/docs/introduction?utm_source=github&utm_medium=repo&utm_content=header_ja) · [ディスカッション](https://github.com/gotempsh/temps/discussions) · [Contributing](CONTRIBUTING.md)

[English](README.md) | [简体中文](README.zh-CN.md) | [Español](README.es.md) | [Français](README.fr.md) | [Deutsch](README.de.md) | 日本語 | [Português](README.pt-BR.md)

</div>

---

```bash
curl -fsSL https://temps.sh/deploy.sh | bash
```

<picture>
  <source media="(prefers-color-scheme: dark)" srcset="assets/screenshots/create-dark.png">
  <img alt="Temps での公開リポジトリのインポート —— デプロイ前にフレームワークプリセットを自動検出" src="assets/screenshots/create-light.png">
</picture>


7 種類の SaaS ツールに料金を払うのはもう終わりです。Temps はデプロイプラットフォーム、アナリティクス、エラートラッキング、セッションリプレイ、稼働監視、トランザクションメール、AI コード実行サンドボックスをまとめて置き換えます —— すべてセルフホストで、すべて1つのバイナリに。

---

## 機能

### AI ネイティブ —— エージェントが実行できる 440 以上の操作

ダッシュボードでできる操作はすべて CLI コマンドにもなっています —— **69 グループ、440 以上** —— しかも Temps には、その使い方をエージェントに教える [skills](skills/) が同梱されています。**Claude Code**、**Codex**、**OpenCode**、あるいは `.claude/skills/` を読むあらゆるハーネスに入れれば、エージェントはデプロイ、トレースの調査、マイグレーションの実行、ドメインの追加まで、接着コードなしでこなせます。

```bash
bunx @temps-sdk/cli projects list
bunx @temps-sdk/cli deploy my-app --environment production
bunx @temps-sdk/cli analytics ai-agents -p my-app --period 7d
```

Temps はそれらのエージェントの実行環境も提供します。ワークフローサンドボックスがあなたのリポジトリに対して Claude Code、Codex、OpenCode を実行し、プラットフォーム全体のスキルと MCP サーバーが自動的に注入されます。

### AI チャット —— 自分のテレメトリに基づく回答

プロジェクトについて尋ねれば、答えは汎用モデルの推測ではなく、あなた自身のデータ —— トレース、メトリクス、アラーム、デプロイ、収益 —— から返ってきます。**デフォルトは読み取り専用**で、書き込み操作はオプトイン。有効にしても、アシスタントは変更を提案してあなたの確認を待ちます。

<picture>
  <source media="(prefers-color-scheme: dark)" srcset="assets/screenshots/ai-chat-dark.png">
  <img alt="Temps の AI チャットが、プロジェクト自身のトレース、メトリクス、収益データからチェックアウトのレイテンシ急増を診断" src="assets/screenshots/ai-chat-light.png">
</picture>

### AI ゲートウェイ —— 1 つのエンドポイント、自分の API キー

自分のプロバイダーキー（OpenAI、Anthropic、xAI、Google Gemini）を持ち込み、すべてを 1 つの OpenAI 互換エンドポイント経由で呼び出せます —— base URL を差し替えるだけで、今使っている SDK はそのまま。キーは自分のサーバー上で暗号化して保管され、リクエストごとにトークン数、レイテンシ、エラー率、モデル別の推定コストが記録されます。

<picture>
  <source media="(prefers-color-scheme: dark)" srcset="assets/screenshots/ai-gateway-dark.png">
  <img alt="Temps AI ゲートウェイ —— OpenAI 互換エンドポイントの背後に自分のプロバイダーキー（BYOK）" src="assets/screenshots/ai-gateway-light.png">
</picture>

<picture>
  <source media="(prefers-color-scheme: dark)" srcset="assets/screenshots/ai-usage-dark.png">
  <img alt="Temps AI ゲートウェイの使用状況分析 —— リクエスト数、トークン、レイテンシ、エラー率、推定コスト" src="assets/screenshots/ai-usage-light.png">
</picture>

### ウェブアナリティクス & セッションリプレイ

ファネル分析、訪問者トラッキング、セッションリプレイ（rrweb）を標準搭載したウェブアナリティクス —— 外部サービスは不要で、データがサーバーの外に出ることもありません。これは他のセルフホスト型 PaaS にはない機能です。

<picture>
  <source media="(prefers-color-scheme: dark)" srcset="assets/screenshots/analytics-dark.png">
  <img alt="Temps ウェブアナリティクス —— 訪問者、セッション、ページ、ファネル" src="assets/screenshots/analytics-light.png">
</picture>

### 稼働監視 & アラート

ステータスタイムライン付きの稼働モニターに加え、デプロイ失敗、ランタイムのクラッシュ、証明書の期限切れ、バックアップの健全性に対するアラートを提供。問題がユーザーに届く前に通知を受け取れます。

<picture>
  <source media="(prefers-color-scheme: dark)" srcset="assets/screenshots/uptime-dark.png">
  <img alt="Temps 稼働監視 —— ステータスタイムライン、稼働率、レスポンスタイム" src="assets/screenshots/uptime-light.png">
</picture>

### エラートラッキング —— Sentry 互換

Sentry のドロップイン代替: 公式の Sentry SDK を Temps の DSN に向けるだけで、エラーグループ、ソースコンテキスト付きのスタックトレース、アラートが手に入ります。イベント単位の課金はありません。

<picture>
  <source media="(prefers-color-scheme: dark)" srcset="assets/screenshots/errors-dark.png">
  <img alt="Temps エラートラッキング —— イベントとタイムライン付きのエラーグループ" src="assets/screenshots/errors-light.png">
</picture>

### リクエストログ & プロキシの可視化

すべての HTTP リクエストを、メソッド、パス、ステータス、レスポンスタイム、ルーティングメタデータとともに記録 —— AI クローラー別のトラフィック（OpenAI、Anthropic、Perplexity、Google…）も含まれます。Cloudflare の Pingora エンジン上で動作し、Let's Encrypt による自動 TLS（HTTP-01 & DNS-01）に対応します。

<picture>
  <source media="(prefers-color-scheme: dark)" srcset="assets/screenshots/request-logs-dark.png">
  <img alt="AI エージェントのフィルタリングに対応した Temps プロキシリクエストログ" src="assets/screenshots/request-logs-light.png">
</picture>

### トランザクションメール

UI から DKIM レコード付きの送信ドメインを追加し、`@temps-sdk/node-sdk` で送信 —— あるいは AWS SES、Scaleway、任意の SMTP リレーを接続することもできます。

<picture>
  <source media="(prefers-color-scheme: dark)" srcset="assets/screenshots/email-dark.png">
  <img alt="Temps メールプロバイダー —— SMTP、Scaleway、AWS SES" src="assets/screenshots/email-light.png">
</picture>

### OpenTelemetry —— トレース、メトリクス、ログ、アラート

任意の OTLP エクスポーターを Temps に向けるだけで、分散トレース、メトリクス、構造化ログが他のデータと同じ場所に集まります。トレースはサービスをまたいだスパンごとのレイテンシとエラーを表示し、メトリクスはゴールデンシグナルを記録し、アラートはそのメトリクスから発火して1つのキューに集約され、確認や解決を行えます。Grafana、Prometheus、Jaeger、Loki を運用する必要はありません。

<picture>
  <source media="(prefers-color-scheme: dark)" srcset="assets/screenshots/traces-dark.png">
  <img alt="Temps の分散トレース —— リクエストごとのレイテンシ、スパン数、サービス間のエラー" src="assets/screenshots/traces-light.png">
</picture>

<picture>
  <source media="(prefers-color-scheme: dark)" srcset="assets/screenshots/metrics-dark.png">
  <img alt="Temps の OpenTelemetry メトリクス —— リクエストレート、レイテンシ、データベースとキャッシュのシグナル" src="assets/screenshots/metrics-light.png">
</picture>

<picture>
  <source media="(prefers-color-scheme: dark)" srcset="assets/screenshots/otel-logs-dark.png">
  <img alt="Temps の構造化ログ —— 重大度、サービス、メッセージをトレースと相関" src="assets/screenshots/otel-logs-light.png">
</picture>

<picture>
  <source media="(prefers-color-scheme: dark)" srcset="assets/screenshots/alerts-dark.png">
  <img alt="Temps のアラート —— メトリクス、コンテナ、稼働監視、データベースにまたがる発火中・確認済み・解決済みのアラーム" src="assets/screenshots/alerts-light.png">
</picture>

### AI サンドボックス —— Firecracker マイクロ VM、セルフホスト

**コンテナだけではない、本物のハードウェアレベルの隔離。** サンドボックスは **Firecracker マイクロ VM**（AWS Lambda を支えるのと同じ技術）上で動き、既定のバックエンドは **Docker** です。`temps firecracker setup` を実行すれば、Temps は自動でサンドボックスをマイクロ VM に振り分けます。各サンドボックスが独自のカーネルを持つため、エージェントが生成した信頼できないコードがホストとカーネルを共有することはありません。

**そのまま差し替えられる SDK。** `@temps-sdk/sandbox` は `@vercel/sandbox` と互換の形をしています —— import とベース URL を変えるだけでプロバイダを切り替えられます:

```ts
import { Sandbox } from '@temps-sdk/sandbox'

const sandbox = await Sandbox.create({
  source: { type: 'git', url: 'https://github.com/example/repo.git', revision: 'main' },
})

const { stdout } = await sandbox.exec(['npm', 'test'])
const url = sandbox.domain(3000) // VM 内の開発サーバーのライブプレビュー
```

**パスワード保護されたプレビュー。** サンドボックスの各ポートは公開プレビュー URL として公開でき、生成されたパスワードでロックできます:

```bash
bunx @temps-sdk/cli sandbox password sbx_abc123 --rotate --length 32
bunx @temps-sdk/cli sandbox password sbx_abc123 --clear   # 再び公開する
```

動作中のブランチを、世界中に公開することなく共有できます。

CLI と REST API からも利用できます。E2B や Daytona、Vercel Sandbox に支払っていた分がこれで不要になります。

<picture>
  <source media="(prefers-color-scheme: dark)" srcset="assets/screenshots/sandboxes-dark.png">
  <img alt="Temps のサンドボックス —— 実行中のサンドボックスと、そのままコピーできる CLI・REST・SDK のスニペット" src="assets/screenshots/sandboxes-light.png">
</picture>

各サンドボックスにはシェル、バインドした任意のポート用のプレビュー URL テンプレート、そして起きたことすべてのタイムラインが付いてきます:

<picture>
  <source media="(prefers-color-scheme: dark)" srcset="assets/screenshots/sandbox-detail-dark.png">
  <img alt="Temps サンドボックス詳細 —— Docker/Firecracker バックエンド、ブラウザ内コマンド実行、プレビュー URL テンプレート、パスワード保護されたプレビュー" src="assets/screenshots/sandbox-detail-light.png">
</picture>

### すべてを1つのダッシュボードに

訪問者、エラー、デプロイ状況、監視の健全性をプロジェクトごとに一元表示 —— ブラウザのタブを6つ開く代わりに、この1画面で完結します。

<picture>
  <source media="(prefers-color-scheme: dark)" srcset="assets/screenshots/dashboard-dark.png">
  <img alt="Temps プロジェクトダッシュボード —— 全プロジェクトの訪問者とステータス" src="assets/screenshots/dashboard-light.png">
</picture>

### Git プッシュでデプロイ & マネージドサービス

Git にプッシュすれば、Temps がビルドとデプロイを行い、ゼロダウンタイムのロールアウトでプレビュー URL を作成します —— あらゆる言語を自動検出。Postgres、Redis、S3（MinIO）、MongoDB をアプリと並べてプロビジョニングでき、作成、バックアップ、削除は Temps が処理します。

### あなたのスタックでそのまま使える

<p align="center">
<a href="https://nextjs.org"><img src="https://img.shields.io/badge/Next.js-000?logo=nextdotjs&logoColor=fff&style=for-the-badge" alt="Next.js" /></a>
<a href="https://vitejs.dev"><img src="https://img.shields.io/badge/Vite-646CFF?logo=vite&logoColor=fff&style=for-the-badge" alt="Vite" /></a>
<a href="https://go.dev"><img src="https://img.shields.io/badge/Go-00ADD8?logo=go&logoColor=fff&style=for-the-badge" alt="Go" /></a>
<a href="https://python.org"><img src="https://img.shields.io/badge/Python-3776AB?logo=python&logoColor=fff&style=for-the-badge" alt="Python" /></a>
<a href="https://rust-lang.org"><img src="https://img.shields.io/badge/Rust-000?logo=rust&logoColor=fff&style=for-the-badge" alt="Rust" /></a>
<a href="https://java.com"><img src="https://img.shields.io/badge/Java-ED8B00?logo=openjdk&logoColor=fff&style=for-the-badge" alt="Java" /></a>
<a href="https://dotnet.microsoft.com"><img src="https://img.shields.io/badge/.NET-512BD4?logo=dotnet&logoColor=fff&style=for-the-badge" alt=".NET" /></a>
<a href="https://nestjs.com"><img src="https://img.shields.io/badge/NestJS-E0234E?logo=nestjs&logoColor=fff&style=for-the-badge" alt="NestJS" /></a>
<a href="https://docker.com"><img src="https://img.shields.io/badge/Dockerfile-2496ED?logo=docker&logoColor=fff&style=for-the-badge" alt="Docker" /></a>
</p>

<p align="center"><em>あらゆる言語、あらゆるフレームワークに対応。自動検出、または独自の Dockerfile を持ち込めます。</em></p>

---

## すでに他のプラットフォームで運用中ですか?

Temps は既存のセットアップを再構築させるのではなく、そのままインポートします。ウィザードで今使っているプラットフォームを指定するだけで、アプリ、*データを含む*データベース、ドメイン、環境変数までまるごと移行されます。

**セルフホスト型プラットフォーム**

<p align="center">
<a href="https://coolify.io"><img src="https://img.shields.io/badge/Coolify-8B5CF6?logo=coolify&logoColor=fff&style=for-the-badge" alt="Coolify" /></a>
<a href="https://dokploy.com"><img src="https://img.shields.io/badge/Dokploy-0B0B0B?style=for-the-badge" alt="Dokploy" /></a>
<a href="https://caprover.com"><img src="https://img.shields.io/badge/CapRover-2196F3?logo=caprover&logoColor=fff&style=for-the-badge" alt="CapRover" /></a>
<a href="https://portainer.io"><img src="https://img.shields.io/badge/Portainer-13BEF9?logo=portainer&logoColor=fff&style=for-the-badge" alt="Portainer" /></a>
<a href="https://kamal-deploy.org"><img src="https://img.shields.io/badge/Kamal-1B1B1B?style=for-the-badge" alt="Kamal" /></a>
<a href="https://kubernetes.io"><img src="https://img.shields.io/badge/Kubernetes-326CE5?logo=kubernetes&logoColor=fff&style=for-the-badge" alt="Kubernetes" /></a>
<a href="https://docker.com"><img src="https://img.shields.io/badge/Docker-2496ED?logo=docker&logoColor=fff&style=for-the-badge" alt="Docker" /></a>
</p>

**ホスティング型プラットフォーム**

<p align="center">
<a href="https://vercel.com"><img src="https://img.shields.io/badge/Vercel-000?logo=vercel&logoColor=fff&style=for-the-badge" alt="Vercel" /></a>
<a href="https://netlify.com"><img src="https://img.shields.io/badge/Netlify-00C7B7?logo=netlify&logoColor=fff&style=for-the-badge" alt="Netlify" /></a>
<a href="https://railway.app"><img src="https://img.shields.io/badge/Railway-0B0D0E?logo=railway&logoColor=fff&style=for-the-badge" alt="Railway" /></a>
<a href="https://render.com"><img src="https://img.shields.io/badge/Render-000?logo=render&logoColor=fff&style=for-the-badge" alt="Render" /></a>
<a href="https://fly.io"><img src="https://img.shields.io/badge/Fly.io-24175B?logo=flydotio&logoColor=fff&style=for-the-badge" alt="Fly.io" /></a>
</p>

<p align="center"><em>インポートはダッシュボードから実行します — プラットフォームのアイコンはプロジェクトページのヘッダーにあります。</em></p>

---

## クイックスタート

```bash
curl -fsSL https://temps.sh/deploy.sh | bash
```

**動作確認済み:** Ubuntu 24.04 / 22.04 &nbsp;|&nbsp; macOS でも動作します

サーバーの管理はしたくない？ [Temps Cloud](https://temps.sh/pricing?utm_source=github&utm_medium=repo&utm_content=cloud_cta_ja) なら、マネージドインフラ上で Temps をあなたの代わりに運用します。

---

## Temps が置き換えるもの

| 手に入るもの | 支払わずに済むもの |
|---|---|
| Git デプロイ + プレビュー URL | Vercel / Netlify / Railway（月額 $20〜） |
| ウェブアナリティクス + ファネル分析 | PostHog / Plausible（月額 $0〜450） |
| セッションリプレイ | PostHog / FullStory（月額 $0〜2000） |
| エラートラッキング | Sentry（月額 $26〜） |
| トレース、メトリクス、ログ（OpenTelemetry） | Grafana Cloud / Datadog（月額 $0〜500+） |
| 稼働監視 | Better Uptime / Pingdom（月額 $20〜） |
| マネージド Postgres/Redis/S3 | AWS RDS / ElastiCache（月額 $50〜） |
| トランザクションメール + DKIM | Resend / SendGrid（月額 $20〜100） |
| AI コード実行サンドボックス | E2B / Daytona / Vercel Sandbox（$150+/月 + 従量課金） |
| AI ゲートウェイ + 使用量/コスト追跡 | OpenRouter / Helicone / LangSmith（月額 $0〜200+） |
| リクエストログ + プロキシ | Cloudflare（月額 $0〜200） |
| **Temps での合計** | **$0（セルフホスト）** |

---

## Temps と代替ツールの比較

| 機能 | Temps | Coolify | Dokploy | Kamal | Railway | Render | Vercel |
|---|:---:|:---:|:---:|:---:|:---:|:---:|:---:|
| セルフホスト & オープンソース | あり | あり | あり | あり | なし | なし | なし |
| 単一バイナリでインストール | あり | なし | なし | CLI ツール | -- | -- | -- |
| Git プッシュでデプロイ | あり | あり | あり | なし | あり | あり | あり |
| プレビューデプロイ | あり | あり | あり | なし | あり | あり | あり |
| 自動 TLS（HTTP-01 + DNS-01） | あり | あり | あり | あり | あり | あり | あり |
| Docker Compose 対応 | あり | あり | あり | なし | -- | -- | -- |
| ワンクリックテンプレートライブラリ | なし | 280以上 | あり | なし | あり | あり | あり |
| ウェブアナリティクス | あり | なし | なし | なし | なし | なし | 有料アドオン |
| セッションリプレイ | あり | なし | なし | なし | なし | なし | なし |
| エラートラッキング（Sentry 互換） | あり | なし | なし | なし | なし | なし | なし |
| OpenTelemetry トレース + メトリクス + ログ | あり | なし | なし | なし | なし | なし | トレース（有料） |
| 稼働監視 | あり | なし | なし | なし | なし | なし | なし |
| トランザクションメール + DKIM | あり | なし | なし | なし | なし | なし | なし |
| コード実行サンドボックス（API） | あり | なし | なし | なし | なし | なし | Sandbox（従量課金） |
| AI ゲートウェイ（BYOK）+ アシスタント | あり | なし | なし | なし | なし | なし | AI Gateway（有料） |
| マネージド Postgres / Redis | あり | あり | あり | なし | あり | あり | パートナーアドオン |
| S3 互換ストレージ | あり | なし | なし | なし | なし | なし | Blob（有料） |
| マルチノード / クラスタリング | あり | あり | Swarm | あり | マネージド | マネージド | マネージド |
| エッジ関数 / グローバルエッジネットワーク | なし | なし | なし | なし | なし | なし | あり |
| シートごとの課金 | なし | なし | なし | なし | $20/ユーザー（Pro） | ユーザー単位 | $20/シート（Pro） |

**代替ツールが勝る点。** Coolify と Dokploy には、Temps がまだ持っていないワンクリックテンプレートライブラリ（Coolify は280以上のアプリ）があり、コミュニティの規模もはるかに大きい —— Coolify だけで GitHub スター56k超を誇る一方、Temps はこのリストで最も新しいプロジェクトです。CLI からのゼロダウンタイム Docker デプロイだけが必要なら、Kamal のほうがシンプルな選択肢です。Vercel をはじめとするマネージドプラットフォームは、単一の VPS では太刀打ちできないグローバルエッジネットワーク、エッジ関数、DDoS 吸収能力を提供し、しかもインフラの運用まで代行してくれます —— サーバーのことを一切考えたくないなら、それは確かな価値です。

詳細で定期的に更新される比較はこちら: [temps.sh/compare](https://temps.sh/compare?utm_source=github&utm_medium=repo&utm_content=compare_ja)

---

## 技術スタック

- **バックエンド:** Rust, Axum, Sea-ORM, Pingora（Cloudflare のプロキシエンジン）, Bollard（Docker API）
- **フロントエンド:** React 19, TypeScript, Tailwind CSS, shadcn/ui
- **データベース:** PostgreSQL + TimescaleDB
- **アーキテクチャ:** 30以上のワークスペースクレート、三層サービスアーキテクチャ

---

## SDK

| パッケージ | 説明 |
|---|---|
| [`@temps-sdk/node-sdk`](https://www.npmjs.com/package/@temps-sdk/node-sdk) | プラットフォーム API クライアント + Sentry 互換エラートラッキング |
| [`@temps-sdk/react-analytics`](https://www.npmjs.com/package/@temps-sdk/react-analytics) | React アナリティクス、セッションリプレイ、Web Vitals、エンゲージメント計測 |
| [`@temps-sdk/kv`](https://www.npmjs.com/package/@temps-sdk/kv) | サーバーレスのキーバリューストア |
| [`@temps-sdk/blob`](https://www.npmjs.com/package/@temps-sdk/blob) | ファイルストレージ（S3 互換） |
| [`@temps-sdk/cli`](https://www.npmjs.com/package/@temps-sdk/cli) | コマンドラインインターフェース |

<details>
<summary><strong>クイックサンプル</strong></summary>

**アナリティクス** —— React アプリをラップするだけで、あとはすべて自動:

```tsx
import { TempsAnalyticsProvider } from '@temps-sdk/react-analytics';

export default function App({ children }) {
  return <TempsAnalyticsProvider>{children}</TempsAnalyticsProvider>;
}
```

**エラートラッキング** —— Sentry 互換のドロップイン代替:

```typescript
import { ErrorTracking } from '@temps-sdk/node-sdk';

ErrorTracking.init({ dsn: 'https://key@your-instance.temps.dev/1' });

try {
  riskyOperation();
} catch (error) {
  ErrorTracking.captureException(error);
}
```

**KV ストア** —— Redis ライクな API、設定不要:

```typescript
import { kv } from '@temps-sdk/kv';

await kv.set('user:123', { name: 'Alice', plan: 'pro' }, { ex: 3600 });
const user = await kv.get('user:123');
```

**Blob ストレージ** —— ファイルのアップロードと配信:

```typescript
import { blob } from '@temps-sdk/blob';

const { url } = await blob.put('avatars/user-123.png', fileBuffer);
const files = await blob.list({ prefix: 'avatars/' });
```

</details>

---

## コミュニティ

- [GitHub Discussions](https://github.com/gotempsh/temps/discussions) — 質問、アイデア、成果の共有
- [GitHub Issues](https://github.com/gotempsh/temps/issues) — バグ報告と機能リクエスト

Temps のおかげで SaaS の請求が減ったなら、[スター](https://github.com/gotempsh/temps)を付けてもらえると、他の人が見つけやすくなります。

---

## スター履歴

<a href="https://www.star-history.com/#gotempsh/temps&Date">
  <picture>
    <source media="(prefers-color-scheme: dark)" srcset="https://api.star-history.com/svg?repos=gotempsh/temps&type=Date&theme=dark" />
    <source media="(prefers-color-scheme: light)" srcset="https://api.star-history.com/svg?repos=gotempsh/temps&type=Date" />
    <img alt="スター履歴チャート" src="https://api.star-history.com/svg?repos=gotempsh/temps&type=Date" />
  </picture>
</a>

---

## コントリビューション

コントリビューションを歓迎します。ガイドラインは [CONTRIBUTING.md](CONTRIBUTING.md) をご覧ください。

```bash
git clone https://github.com/gotempsh/temps.git
cd temps
cargo build --release
```

---

## ライセンス

[MIT](LICENSE-MIT) または [Apache 2.0](LICENSE) のデュアルライセンスです。

---

<div align="center">

[temps.sh](https://temps.sh/?utm_source=github&utm_medium=repo&utm_content=footer_ja) | [ドキュメント](https://temps.sh/docs?utm_source=github&utm_medium=repo&utm_content=footer_ja) | [GitHub](https://github.com/gotempsh/temps)

</div>
