# ADR-0002: 読み取り専用 snapshot と commit barrier による決定的並列化を採用する

## ステータス

提案

## コンテキスト

Ferritex は差分コンパイルと並列化で高速化を狙うが、TeX のマクロ展開と pass 間状態は本質的に逐次性が強い。`domain_model.md` でも `CompilationJob` と `CompilationSession` を分離し、可変状態への commit は barrier でのみ行う方針が示されている。

ここで区別すべき snapshot は 2 種類ある。

- `CompilationSnapshot`: compile pipeline の並列ステージが参照する、job/pass 状態の読み取り専用投影
- `LiveAnalysisSnapshot`: LSP が参照する、`OpenDocumentBuffer` と安定化済み compile state から生成される別契約の immutable projection

両者は「読み取り専用 projection である」という原則を共有するが、同一型・同一寿命ではない。

## 検討した選択肢

### 選択肢 A: worker 間で可変状態を直接共有する

- 利点:
  - 見かけ上は実装が単純
  - コピーや投影コストを減らせる
- 欠点:
  - 競合と非決定性が増え、pdfLaTeX 互換性と再現性を壊しやすい
  - LSP / preview と compile の整合を保ちにくい

### 選択肢 B: immutable projection + deterministic commit barrier

compile の並列ステージは `CompilationSnapshot` だけを読み、job-scope の更新は順序付き barrier でまとめて commit する。LSP は別契約の `LiveAnalysisSnapshot` を参照し、mutable な `CompilationSession` を直接保持しない。

- 利点:
  - 決定性を保ったまま並列化できる
  - `CompilationJob` / `CompilationSession` の責務が明確になる
  - compile 用と LSP 用で不変条件を共有しつつ、契約を分けられる
- 欠点:
  - snapshot 生成と merge の設計が必要
  - 粗い snapshot だとメモリ使用量が増える

### 選択肢 C: 外部 worker process に完全分散する

- 利点:
  - プロセス分離はしやすい
- 欠点:
  - 低遅延要件と単一バイナリ要件に合わない
  - mutable state と artifact provenance の受け渡しが重い

## 判断

選択肢 B を採用する。

## 根拠

- 互換性・決定性を aggressive な共有メモリ並列化より優先する
- 並列化の価値は `DocumentPartitionPlanner`、フォント処理、graphics 正規化など独立部分に限定できる
- LSP は compile の mutable state を直参照せず、別 projection によって一貫した読み取りモデルを得る

## 帰結

### ポジティブ

- 差分コンパイルと LSP の両方で projection ベースの不変条件を共有できる
- race condition を局所化できる
- perf tuning 対象が snapshot 生成 / merge / worker 実行に整理される

### ネガティブ

- snapshot の粒度設計を誤るとメモリと CPU を浪費する
- merge conflict ルールの実装が必要

### リスク

- `CompilationSnapshot` と `LiveAnalysisSnapshot` を同一型のまま実装すると不変条件が曖昧になる
- `DocumentState` 全量コピーに寄ると `REQ-NF-003` のメモリ目標を満たしにくい
