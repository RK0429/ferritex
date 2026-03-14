# ADR-0004: すべての入口で `ExecutionPolicy` と `FileAccessGate` を共通利用する

## ステータス

提案

## コンテキスト

Ferritex は `compile` / `watch` / `lsp` のすべてで同じファイルアクセス制約と外部コマンド制約を適用する必要がある。要件では `REQ-NF-005` と `REQ-NF-006` が Must であり、`domain_model.md` でも `ExecutionPolicy` と `FileAccessGate` を共通モデルとして扱っている。

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

## 帰結

### ポジティブ

- 入口差によるセキュリティバグを減らせる
- deny-case テストを共通化できる
- ログや監査項目を統一できる

### ネガティブ

- すべての I/O を gate 経由にする設計規律が必要
- 開発初期は adapter 実装量が増える

### リスク

- 近道実装で gate を bypass すると設計全体が崩れる
- Preview transport や host font lookup など境界外に見えやすい経路も policy 対象へ含める必要がある
