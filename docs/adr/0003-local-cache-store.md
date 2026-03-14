# ADR-0003: 差分コンパイル状態は独立ストア群に分けて永続化する

## ステータス

提案

## コンテキスト

Ferritex は `DependencyGraph` と `CompilationCache` を永続化して `REQ-NF-002` の差分コンパイル速度を支える必要がある。一方、trusted artifact 判定に使う `OutputArtifactRegistry` は current `JobContext` に結び付くため、cache と同じ寿命で永続化すると `REQ-NF-006` の same-job 制約を曖昧にする。

## 検討した選択肢

### 選択肢 A: in-memory のみ

- 利点:
  - 実装は最小
  - I/O オーバーヘッドがない
- 欠点:
  - process 再起動で差分コンパイルが失われる
  - watch / lsp / compile 間で状態再利用しにくい

### 選択肢 B: 平文ファイル / JSON の寄せ集め

- 利点:
  - 人間が直接読める
  - 実装の学習コストが低い
- 欠点:
  - 整合性、更新原子性、GC が弱い
  - artifact registry と dependency graph の参照整合性を保ちにくい

### 選択肢 C: 独立ストア群

`DependencyGraphStore`、`CacheMetadataStore`、`BlobCacheStore` を分離し、必要に応じて同じ SQLite 技術を使っても論理的に独立させる。`OutputArtifactRegistry` は active job の in-memory state とし、監査用にのみ append-only manifest を残す。

- 利点:
  - dependency graph と cache の破損を障害分離できる
  - metadata と大きな binary を分離できる
  - trusted artifact の寿命を job 単位で明確にできる
- 欠点:
  - schema migration と blob GC が必要
  - 単一 DB より構成管理が増える

## 判断

選択肢 C を採用する。

## 根拠

- `DependencyGraph` は `CompilationCache` と独立永続化した方が、cache 破損時のフォールバック要件を満たしやすい
- cache metadata と大きな compiled artifact は分離した方が再利用効率と GC 制御に優れる
- trusted artifact は active `JobContext` にのみ有効とし、job 完了または process restart で無効化するのが same-job 制約に合う
- 単一ユーザー・単一ノード前提では SQLite 系ストアの運用コストが低い

## 帰結

### ポジティブ

- 差分コンパイルに必要な metadata を高速に参照できる
- cache corruption と dependency graph 破損を切り分けやすい
- trusted artifact 判定を job-scope で厳格に実装できる

### ネガティブ

- schema version 管理が必要
- cache directory の肥大化対策を別途持つ必要がある
- 監査用 manifest と trusted 判定ロジックを混同しない規律が必要

### リスク

- blob GC 不備でディスク肥大化する
- hot path で SQL アクセスが多すぎると性能目標を阻害する
- active job 終了時の registry 無効化漏れがあると readback 制約が崩れる
