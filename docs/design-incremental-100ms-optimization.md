# REQ-NF-002 差分コンパイル 100ms 最適化設計

## Meta

| Item | Value |
|---|---|
| Version | 0.1.0 |
| Date | 2026-04-04 |
| Status | Draft |
| Scope | REQ-NF-002（差分コンパイル中央値 100ms 未満） |
| Input | requirements.md, planning_report.md, 調査レポート（researcher session d18ad3a3）, 現行コードベース |

## 1. 現状と未達理由

### 1.1 REQ-NF-002 の定義

> `FTX-BENCH-001` を一度フルコンパイルしてキャッシュと依存グラフを構築した状態で、本文 1 段落だけを変更した差分コンパイル完了時間の中央値が 100ms 未満である

— requirements.md REQ-NF-002（Must、ユーザー確認済み 2026-03-16）

### 1.2 Wave 1 の到達点

Wave 1（Incremental Performance Evidence）では `FTX-BENCH-001` 1000-section staged input に対し、warm incremental compile 15.550s vs full `--no-cache` 28.614s（1.84× speedup）を point-in-time 計測した。これは incremental compile **機構**の有効性を実証したものであり、100ms 目標とは 2 桁以上の乖離がある。

### 1.3 ボトルネック分析

現行パイプラインで 100ms を超過する原因を、pipeline stage ごとに整理する。Step 0 の計装（`StageTiming`）により stage 別の定量計測基盤は整備済みだが、`FTX-BENCH-001` 5-run median による実測プロファイリングは未実施である。以下はコード構造からの定性分析であり、Step 1 着手前に定量データで検証する。

| Stage | 現状の問題 | 根拠 |
|---|---|---|
| **依存検出** | `CompileCache::detect_changes()` が依存グラフ上の全 node を走査し content hash を比較する。変更パスの caller 伝播がない | `compile_cache.rs:328` |
| **Parse** | source subtree reuse は I/O 削減のみ。expanded 全文を毎回 parser に渡し直す | `compile_job_service.rs:2112` |
| **Typeset** | `TypesetterReusePlan` の再利用単位が partition（chapter/section）粒度。1 段落変更でも section 全体を rebuild する。monolithic file は full fallback | `typesetting/api.rs:447`, `compile_job_service.rs:2285` |
| **Cross-ref 収束** | `\pageref` 存在時は partial typeset 無効化。2〜3 pass 回り得る | `compile_job_service.rs:1026`, `compile_job_service.rs:1114` |
| **PDF 出力** | 全 page を再 render し全 PDF bytes を再生成する | `pdf/api.rs:232`, `compile_job_service.rs:1380` |
| **Cache 保存** | monolithic JSON serialize/deserialize が O(full document) | `compile_cache.rs:272` |

**構造的制約**: 上記の各 stage が O(full document) で動作するため、個別 stage の最適化では 100ms に到達できない。複数 stage を同時に sub-document 粒度に縮小する必要がある。

## 2. 設計オプション比較

### Option A: Fixed-cost 削減のみ

依存検出と cache I/O の固定費を削減する。

- **内容**: `changed_paths` を caller（watch/scheduler）から渡す stat-based fast path、cache を index + per-partition blob に分割
- **対象**: `compile_cache.rs`, `runtime_options.rs`, `RecompileScheduler`
- **効果**: 依存検出と cache I/O のオーバーヘッドを O(changed files) に削減
- **限界**: parse/typeset/render が全文のままなので、単独では 100ms に届かない
- **リスク**: 低。既存 API の拡張で実現可能
- **評価**: **必要条件だが十分条件ではない**

### Option B: Block checkpoint reuse（partition 内 block 粒度再利用）

partition 内の block（paragraph/display/list/float）単位で checkpoint を取り、変更 block 以降の suffix のみ rebuild する。

- **内容**: `RecompilationScope` を affected partitions + affected source spans を持つ構造体に拡張。`segment_source_span` ベースで block を識別し、`build_vlist_for_partition_continuing()` + `paginate_vlist_continuing_detailed()` で影響 block 以降の suffix だけ rebuild + repaginate
- **対象**: `incremental/api.rs`, `typesetting/api.rs`, `compile_job_service.rs`, `compile_cache.rs`
- **効果**: 1 段落変更で typeset する範囲を section 全体から「変更 block + suffix」に縮小
- **限界**: parse と PDF render は全文のまま（他 option との組み合わせが必要）
- **リスク**: 中。footnote/float の継続状態の正確な保存・復元が最大の技術リスク。`compile_job_service.rs:2650` 周辺の footnote merge が要注意
- **評価**: **本命。既存の continuation API を活用でき、最も費用対効果が高い**

### Option C: Per-page payload reuse（deterministic full rewrite）

未変更ページの rendered content stream を cache し再利用するが、PDF 自体は毎回 deterministic に full rewrite する。

- **内容**: per-page の rendered PDF content stream を hash 付きで cache する。変更が及ばないページの content stream はキャッシュから復元し、変更ページのみ再 render する。最終的な PDF は catalog/xref/trailer を含む完全な単一ファイルとして毎回生成する（incremental PDF update は採用しない）
- **対象**: `pdf/api.rs`, `compile_job_service.rs`
- **効果**: PDF render を O(changed pages) に削減しつつ、出力 PDF は fresh full compile と byte-identical を保証
- **限界**: typeset/parse の改善には寄与しない
- **リスク**: 中。page 境界でのリソース参照（font/image XObject）の一貫性保証が必要
- **評価**: **100ms 達成にはほぼ必須。Option B と組み合わせる**

### Option D: Full incremental parse/IR

unchanged partition の parsed body も cache し、全文 parse を省略する。

- **内容**: partition 単位で parsed IR を cache し、変更 partition のみ再 parse
- **対象**: `parser/api.rs`, `compile_job_service.rs`
- **効果**: parse を O(changed partitions) に削減
- **限界**: macro 展開の副作用（global def/counter）の正確な分離が困難
- **リスク**: 高。現行 parser API からの乖離が最も大きい。preamble 変更時の invalidation 範囲の定義が難しい
- **評価**: **効果は大きいが最も侵襲的。最終段階で検討する**

### オプション比較サマリー

| Option | 効果 | リスク | 実装コスト | 単独で 100ms 達成 | 推奨順序 |
|---|---|---|---|---|---|
| A: Fixed-cost 削減 | 低〜中 | 低 | 低 | 不可 | Step 1 |
| B: Block checkpoint | 高 | 中 | 中 | 不可（他と組合せ） | Step 2 |
| C: Page/PDF reuse | 中〜高 | 中 | 中 | 不可（他と組合せ） | Step 3 |
| D: Incremental parse | 高 | 高 | 高 | 不可（他と組合せ） | Step 4（条件付き） |

## 3. 推奨方針

**A → B → C の順に段階的に実装する。** D は A〜C で 100ms に到達しない場合のみ着手する。

### 根拠

1. **既存 API の活用**: Option B は `segment_source_span`、`build_vlist_for_partition_continuing()`、`paginate_vlist_continuing_detailed()` など既存の continuation API を直接活用できる。全面的な parser rewrite を先送りしつつ、typeset の支配的コストを大幅に削減できる
2. **段階的な効果検証**: A で固定費を削った後、B で typeset、C で render を sub-document 化する。各段階で stage timing を計測し、次段階の必要性を判断できる
3. **リスクの局所化**: A は低リスク、B と C は中リスクだが影響範囲が異なる module に閉じる。D の高リスクを後回しにすることで、早期に効果を得られる
4. **`CachedTypesetFragment` の進化方向**: 次の粒度は「paragraph 単体 cache」より「block checkpoint + page suffix」が現実的。footnote/float/page-shift の継続状態を保持できるため

### 前提条件

- **計装の先行実装（Step 0 完了済み）**: `StageTiming` により stage 別 timing の取得基盤が整った。Step 1 着手前に `FTX-BENCH-001` 5-run median で実測プロファイリングを行い、各 stage のコスト分布を定量的に確認すること
- **`FTX-BENCH-001` の固定構成**: ベンチマークは 1000-section staged input を使用する。monolithic single-file ではなく partition entry file 単位への staged 変換が前提（Wave 1 の設計判断を踏襲）
- **preamble 変更は full fallback を許容**: preamble 変更での 100ms 達成は scope 外。本文 1 段落変更が対象

## 4. 段階的実装ステップ

### Step 0: Pipeline 計装（前提作業） — 完了

`CompileJobService` に `StageTiming` 構造体を追加し、6 stage の個別計測を実装した。

| 計測対象 stage | 対象コード | 実装状態 |
|---|---|---|
| cache_load | `compile_job_service.rs` cache lookup + 依存検出 | **完了** |
| source_tree_load | `compile_job_service.rs` source tree construction | **完了** |
| parse | `compile_job_service.rs` `parse_document_with_cross_references` 全体から typeset 時間を除外 | **完了** |
| typeset | typeset callback 内の累積計測時間 | **完了** |
| pdf_render | `pdf/api.rs` | **完了** |
| cache_store | `compile_cache.rs` serialize/deserialize | **完了** |

**実装詳細**:
- `StageTiming` 構造体（6 フィールド、各 `Option<Duration>`）を `CompileResult` に追加
- parse と typeset は `parse_document_with_cross_references` 内の typeset callback で累積計測した時間を `typeset`、全体からの差分を `parse` として分離
- `tracing::info` ログにマイクロ秒単位で出力。`CompileResult.stage_timing` からプログラマティックにアクセス可能
- `no_cache=true` の場合、cache_load / cache_store は None
- unit test 3 件 + CLI smoke test 1 件 pass

**既知の制約**: 初回 typeset callback で font selection 時間が `typeset` に含まれる。Step 0 として許容だが、Step 1 以降で分離が望ましい。

**成果物**: stage 別 timing の取得基盤。以降の Step の優先順位を定量的に判断する根拠となる。

### Step 1: Fixed-cost 削減

1. **依存検出の最適化**: `CompileCache::detect_changes()` に `changed_paths: &[PathBuf]` パラメータを追加し、watch/scheduler から変更ファイルパスを直接渡す。全 node 走査をスキップする fast path を実装
2. **Cache 分割**: monolithic JSON を `index.json`（metadata のみ）+ per-partition blob ファイルに分割。変更 partition の blob のみ deserialize/serialize する
3. **inotify/kqueue 連携**: `PollingFileWatcher` からのイベントに含まれる変更パスを `changed_paths` として `CompileCache` に渡すパイプラインを接続

**受入基準**: cache load/store の stage timing が Step 0 比で 50% 以上削減。依存検出が O(changed files) で完了する。

### Step 2: Block checkpoint reuse

1. **`RecompilationScope` の拡張**: `FullDocument | LocalRegion` の 2 値から、以下のフィールドを持つ構造体に拡張する:
   - `affected_partitions: Vec<PartitionId>`
   - `affected_source_spans: Vec<SourceSpan>`
   - `references_affected: bool`
   - `pagination_affected: bool`
2. **Block checkpoint の導入**: `CachedTypesetFragment` を拡張し、partition 内の block 境界ごとに layout state（current y position, footnote queue, float queue, page number）を保存する。`segment_source_span` ベースで paragraph/display/list/float block を識別
3. **Suffix rebuild**: `build_vlist_for_partition_continuing()` と `paginate_vlist_continuing_detailed()` を使い、最初の affected block から suffix だけ rebuild + repaginate する。未変更 prefix の VList items はキャッシュから復元
4. **Fallback 条件の明確化**: 以下の場合は full typeset に fallback する:
   - preamble 変更
   - `\pageref` によるページ番号ずれの伝播
   - float reorder が partition boundary を超える場合
   - LOF（List of Figures）/ LOT（List of Tables）/ index エントリの追加・削除・変更（multi-pass 収束が必要なため）

**受入基準**: 1 段落変更時の typeset stage timing が Step 1 比で 80% 以上削減。full fallback が必要なケースが正しく判定される。

### Step 3: Per-page payload reuse（deterministic full rewrite）

1. **Per-page content stream cache**: 各ページの rendered PDF content stream を hash 付きで cache する。hash はページの typeset 結果（テキスト行、画像配置、graphics scene）から算出
2. **Dirty page 判定**: Step 2 の suffix rebuild 結果から変更ページ集合を特定。未変更ページの content stream はキャッシュから復元し再 render をスキップ
3. **Font/Image XObject 再利用**: 未変更ページが参照する font subset と image XObject をキャッシュから復元
4. **Deterministic full rewrite**: 全ページの content stream（キャッシュ復元 + 再 render）を集約し、catalog/xref/trailer を含む完全な PDF を毎回生成する。incremental PDF update は採用しない。これにより fresh full compile との byte-identical 比較が常に有効となる

**受入基準**: PDF render stage timing が Step 2 比で 70% 以上削減。`FTX-BENCH-001` での 1 段落変更 median が 100ms 未満。出力 PDF が fresh full compile と byte-identical であること。

### Step 4: Incremental parse（条件付き）

Step 0〜3 で 100ms 未達の場合のみ着手する。

1. **Partition 単位の parsed IR cache**: 変更のない partition の parsed body をキャッシュし再 parse を省略
2. **Invalidation scope**: preamble 変更・macro 定義変更は全 partition を invalidate。本文変更は affected partition のみ
3. **Global state isolation**: `\gdef` / counter / length register の partition 間副作用の追跡機構

**受入基準**: parse stage timing が O(changed partitions) に削減。global state の副作用が正しく伝播する。

## 5. 主要リスク

| リスク | 影響度 | 発生可能性 | 緩和策 |
|---|---|---|---|
| **Footnote/float 継続状態の不整合** | 高 | 中 | `compile_job_service.rs:2650` 周辺の footnote merge を集中テスト。checkpoint に footnote queue 状態を含める |
| **Cross-reference 収束の non-termination** | 高 | 低 | `\pageref` 含む文書で suffix rebuild 後の収束を byte-identical で検証（既存 `incremental_xref_convergence_after_page_shift` の拡張） |
| **Stage timing 計測なしでの最適化着手** | 緩和済み | — | Step 0 完了により `StageTiming` 計装が稼働中。残タスクは `FTX-BENCH-001` 5-run median の実測プロファイリングのみ |
| **Cache 分割による I/O パターン変化** | 中 | 中 | SSD/HDD 両環境でのベンチマーク。blob 数が過大にならないよう partition 粒度を維持 |
| **Per-page cache の hash 不整合** | 中 | 低 | page content stream の hash 算出にページの全構成要素（テキスト行、画像、graphics scene、font 参照）を含める。hash mismatch 時は再 render に fallback |
| **Monolithic file の full fallback 頻度** | 低 | 高 | `DocumentPartitionPlanner` が monolithic file でも section 境界で仮想 partition を生成する拡張を検討（ただし本設計の scope 外） |

## 6. 検証計画

### 6.1 定量検証

| 計測項目 | 方法 | 基準 |
|---|---|---|
| **1 段落変更 median** | `FTX-BENCH-001` をフルコンパイル後、本文 1 段落変更を 5 回計測し median を採用 | < 100ms |
| **Stage 別 timing** | Step 0 の計装で各 stage の median を採取 | 各 Step の受入基準を満たすこと |
| **Full vs incremental parity** | 1 段落変更の incremental compile PDF と fresh full compile PDF の byte 比較 | byte-identical（cross-ref 収束後） |

### 6.2 正確性検証

| テストケース | 検証内容 |
|---|---|
| **単一段落変更** | 変更 block + suffix のみ rebuild されることを stage counter で確認 |
| **Preamble 変更** | full fallback が正しく発動することを確認 |
| **`\pageref` を含む変更** | ページ番号ずれ後の cross-reference 収束を byte-identical で確認 |
| **Footnote を含む段落変更** | footnote の番号・配置が正しいことを visual parity で確認 |
| **Float を含む段落変更** | float の配置順序が維持されることを確認 |
| **TOC を含む文書** | TOC エントリの更新が正しく反映されることを確認 |
| **LOF/LOT を含む文書** | figure/table の追加・削除時に LOF/LOT が正しく更新され、full fallback が発動することを確認 |
| **Index を含む文書** | `\index` エントリの変更時に索引が正しく再構築されることを確認 |
| **Multi-file (`\include`) 文書** | include されたファイルの変更が正しく検出・反映されることを確認 |

### 6.3 回帰テスト

- 既存の `full_bench_warm_incremental_evidence` が引き続き pass すること
- 既存の `incremental_xref_convergence_after_page_shift` が引き続き pass すること
- REQ-NF-007 の parity 5 カテゴリが全 pass を維持すること

## 7. REQ-NF-002 達成の見通し

Step 0〜3 の組み合わせにより、1 段落変更時のパイプラインは以下のように変化する:

```
現状:  O(全 node hash) → O(全文 parse) → O(section typeset) → O(全 page render) → O(全文 cache serialize)
目標:  O(changed files) → O(全文 parse) → O(suffix typeset) → O(changed pages render + full rewrite) → O(changed partition cache)
         Step 1            (Step 4で改善)     Step 2              Step 3                   Step 1
```

parse が全文のまま残るが、1000-section 文書でも parse 自体は typeset/render と比較して軽量である可能性が高い（Step 0 の `StageTiming` 計装で確認可能。実測プロファイリングは未実施）。parse が支配的と判明した場合のみ Step 4 に進む。

Step 4 なしで 100ms を達成できるかは Step 0 の実測プロファイリング結果に依存するが、typeset と render の sub-document 化（Step 2 + 3）で大半の固定費を除去できる見込みは高い。
