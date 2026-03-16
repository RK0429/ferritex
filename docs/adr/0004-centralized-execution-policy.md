# ADR-0004: すべての入口で `ExecutionPolicy` と `FileAccessGate` を共通利用する

| 項目 | 値 |
| --- | --- |
| ステータス | 提案 |
| 日付 | 2026-03-15 |
| 著者 | ferritex team |
| 関連ADR | ADR-0001, ADR-0002, ADR-0003 |

## コンテキスト

Ferritex は `compile` / `watch` / `lsp` / preview publish path のすべてで同じファイルアクセス制約と外部コマンド制約を適用する必要がある。要件では `REQ-NF-005` と `REQ-NF-006` が Must であり、`domain_model.md` でも `ExecutionPolicy` と `FileAccessGate` を共通モデルとして扱っている。preview については `ExecutionPolicy` に内包される `previewPublication` が loopback bind、active-job 限定 publish、session target 一致、target 変更または process restart 時の session 再発行規約を表現する。ただし `FileAccessGate` が扱うのは filesystem の read / write / readback だけであり、preview の socket accept/connect や `PreviewSession` の in-memory state は対象外とする。

## 検討した選択肢

### 選択肢 A: 入口ごとに個別実装する

- 利点:
  - 入口の事情に合わせた最適化はしやすい
- 欠点:
  - `compile` と `lsp` で挙動差が出やすい
  - セキュリティ回帰の検証範囲が広がる

### 選択肢 B: 共通 `ExecutionPolicy` / `FileAccessGate` を使う

- 利点:
  - deny / allow の意味が全入口で一致する
  - trusted artifact、shell escape、overlay root の扱いを統一できる
  - テストと監査が容易
- 欠点:
  - 入口固有の例外を port 設計で吸収する必要がある

### 選択肢 C: OS sandbox のみを頼る

- 利点:
  - 一部プラットフォームでは強い隔離が可能
- 欠点:
  - クロスプラットフォームで一貫しない
  - artifact provenance や same-job readback は OS sandbox だけでは表現できない

## 判断

選択肢 B を採用する。

## 根拠

- セキュリティ境界は製品全体で一貫している必要がある
- `watch` / `lsp` でも compile と同等のファイル解決や readback が起こるため、共通モデルが必要
- same-job / same-primary-input の provenance 判定はアプリケーション内でしか表現できない
- preview でも bind 先、公開可否、配信対象 artifact の選別、session 再発行規約は policy で統一し、`PreviewSessionService` が `ExecutionPolicy.previewPublication` を評価して active job が生成した PDF だけを publish 対象にする必要がある

## 要件トレーサビリティ

| 要件 | この ADR で固定する点 |
| --- | --- |
| `REQ-FUNC-043` | 入口ごとの差を `RuntimeOptions(jobname を含む)` と `ExecutionPolicyFactory` で吸収し、共通 policy を構築する |
| `REQ-FUNC-047` | `ShellCommandGateway` は default deny、trusted external artifact 登録、上限制御を共通 policy で扱う |
| `REQ-FUNC-048` | `FileAccessGate` が filesystem の read / write / readback を一元判定する |
| `REQ-FUNC-040` | `PreviewSessionService` が preview の loopback bind と active job artifact の publish 可否、target 変更 / process restart 時の session 再発行を `ExecutionPolicy.previewPublication` で制御し、`PreviewTransport` は許可済み publish だけを配信する |
| `REQ-NF-005` / `REQ-NF-006` | 外部コマンド実行とファイルアクセスの deny / allow semantics を入口横断で一致させる |

## 帰結

### ポジティブ

- 入口差によるセキュリティバグを減らせる
- deny-case テストを共通化できる
- ログや監査項目を統一できる
- preview publish path も同じ policy で制御できる

### ネガティブ

- すべての I/O を gate 経由にする設計規律が必要
- 開発初期は adapter 実装量が増える

### リスク

- 近道実装で gate を bypass すると設計全体が崩れる
- Preview transport や host font lookup など境界外に見えやすい経路も policy 対象と gate 対象の線引きを誤ると設計が崩れる
