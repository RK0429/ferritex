# ADR-0003: 差分コンパイル状態は独立ストア群に分けて永続化する

## ステータス

提案

## コンテキスト

Ferritex は `DependencyGraph` と `CompilationCache` を永続化して `REQ-NF-002` の差分コンパイル速度を支える必要がある。一方、trusted artifact 判定に使う `OutputArtifactRegistry` は active job に結び付くため、cache と同じ寿命で永続化すると `REQ-NF-006` の same-job 制約を曖昧にする。trusted readback の一致判定キーは主入力と jobname であり、current pass number は current job の運用属性、`producedPass` は artifact provenance の監査属性として扱う。trusted readback 判定の authority は active job の in-memory `OutputArtifactRegistry` だけとし、append-only manifest は監査専用に限定する必要がある。

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

`DependencyGraphStore`、`CacheMetadataStore`、`BlobCacheStore` を分離し、必要に応じて同じ SQLite 技術を使っても論理的に独立させる。`OutputArtifactRegistry` は active job の in-memory state とし、監査用にのみ append-only manifest を残す。registry record には normalized path、primary input、artifact kind、jobname、`producedPass`、artifact producer kind、produced path、content hash を保持するが、trusted readback の same-job 判定は primary input と jobname だけで行う。current pass number は registry record ではなく active `JobContext` 側の運用属性として扱う。

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
- trusted artifact は active job にのみ有効とし、same-job 判定を主入力 + jobname に固定したうえで job 完了または process restart で無効化するのが制約に合う
- append-only manifest は監査には有用だが、trusted 判定の根拠にすると same-job 制約と authority が混線する
- 単一ユーザー・単一ノード前提では SQLite 系ストアの運用コストが低い

## 要件トレーサビリティ

| 要件 | この ADR で固定する点 |
| --- | --- |
| `REQ-FUNC-027` | `DependencyGraphStore` が依存グラフを独立永続化し、プロセス再起動後も復元可能にする。cache metadata / blob が壊れても依存情報まで巻き込んで失わない |
| `REQ-FUNC-029` | `CacheMetadataStore` / `BlobCacheStore` がコンパイル中間結果の保存・無効化・整合性検証・サイズ管理を担い、cache 破損検出時は `DependencyGraphStore` を温存したままフルコンパイルへフォールバックできる |
| `REQ-NF-002` | 永続化された依存グラフとキャッシュにより、差分コンパイルの再起動コストを抑え速度目標を支える |
| `REQ-FUNC-048` / `REQ-NF-006` | trusted artifact 判定の authority を active job の in-memory `OutputArtifactRegistry` に限定し、append-only manifest は監査専用に留める。same-job 判定キーは主入力 + jobname に固定し、`producedPass` は監査属性として扱う |

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
