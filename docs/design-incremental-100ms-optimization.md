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

現行パイプラインで 100ms を超過する原因を、pipeline stage ごとに整理する。Step 0 の計装（`StageTiming`）により stage 別の定量計測基盤は整備済みであり、以下は Step 3 完了後のコード構造と WU-5 再 profiling 結果に基づく分析である。`FTX-BENCH-001` 等で定量データを継続確認し、追加最適化の優先順位を判断する。

| Stage | 現状の問題 | 根拠 |
|---|---|---|
| **依存検出** | Step 1 で watcher/scheduler からの `changed_paths` fast path、split cache、watcher backend 抽象化は導入済み。ただし empty hint 時は依存グラフ上の全 node 走査に fallback する | `compile_cache.rs:328` |
| **Parse** | source subtree reuse は I/O 削減のみ。expanded 全文を毎回 parser に渡し直す | `compile_job_service.rs:2112` |
| **Typeset** | Step 2 の block checkpoint reuse は benchmark 条件で有効化されており、WU-5 再 profiling では 1000-section staged input の 1 段落変更時に `cached=999 partition`, `suffix_rebuild=1 partition`, `full_rebuild=0 partition` を記録した。変更対象 partition は `reuse=SuffixRebuild, suffix=2/4, fallback=None` で処理され、以前の `SuffixValidationFailed` 起因の full rebuild fallback は解消された。`StageTiming.pass_count` 計装後の with-ref benchmark では typeset median は 57ms（9.7%）で、主要律速ではないことが確認された | `compile_job_service.rs:120`, `compile_job_service.rs:3580`, `compile_job_service.rs:10882` |
| **Cross-ref 収束** | `\pageref` 存在時は partial typeset 無効化。2〜3 pass 回り得る | `compile_job_service.rs:1026`, `compile_job_service.rs:1114` |
| **PDF 出力** | Step 3 で per-page payload reuse は実装済み。ただし再利用対象は `Cached` / `BlockReuse` partition のページに限られ、fallback partition を含む文書や XObject-backed page は safety-first で再 render する | `compile_job_service.rs:1561`, `compile_job_service.rs:4710`, `pdf/api.rs:990` |
| **Cache 保存** | split cache（現在の format は v7）で monolithic JSON は解消したが、`index.json` と変更 partition blob の serialize/deserialize は依然として発生する | `compile_cache.rs:272` |

**構造的制約**: typeset と render は sub-document 粒度まで縮小できているが、parse と一部 fixed-cost stage は依然として全文・全 partition 相当の処理を含む。`StageTiming.pass_count` 計装と 1000-section staged input の 5-run benchmark により、incremental compile の baseline は no-ref 520ms / with-ref 588ms、pass_count はいずれも 1 と確認された。Round 1（delta write）で with-ref median は 505ms、Round 2（dirty-tracking store + warm-cache lookup）で 369ms まで低下した。旧来の 66.8s 計測値は `--no-cache` 初期 compile を同一 run に含む debug 条件のため hot incremental path の比較基準としては不適切である点は変わらない。現在の frontier は cache I/O 252ms（68.3%）の削減であり、dominant stage は cache_load 151ms（40.9%）である。

### 1.4 収束ループ分析（2026-04-05）

`StageTiming.pass_count` 計装により、`\ref` + `\tableofcontents` を含む 1000-section staged input に対する incremental compile の収束パス数を計測した。

**結果**: incremental compile の `pass_count` は 5 run すべてで **1**（no-ref / with-ref 両方）。`cached_cross_reference_seed` がラベルを事前注入し、pass 1 で全 `\ref` が解決される。multi-pass は発生しない。

**Stage 別内訳（with-ref、5-run median）**:

| Stage | Median | 割合 |
|---|---|---|
| cache_store | 289ms | 49.2% |
| cache_load | 171ms | 29.1% |
| typeset | 57ms | 9.7% |
| source_tree_load | 24ms | 4.1% |
| parse | 23ms | 3.9% |
| pdf_render | 12ms | 2.0% |
| **total** | **588ms** | **100%** |

注: 上表は delta write 最適化前の baseline 計測を保持する。更新値は §7「Cache I/O delta write optimization (2026-04-05)」を参照。

**結論**: baseline では cache I/O（cache_store + cache_load = 460ms、78%）が最優先ターゲットだった。Round 1（delta write）で with-ref 5-run median は 505ms、cache I/O は 389ms（77.0%）まで低下した。Round 2（dirty-tracking store + warm-cache lookup）で cache_store は 245ms → 101ms（-58.8%）に劇的改善し、total は 369ms、cache I/O は 252ms（68.3%）まで低下した。dominant stage は cache_store から cache_load 151ms（40.9%）にシフトした。cache I/O が引き続き最優先 frontier であり、cache_load の根本的改善（lazy partition load、binary serialization format）が次の候補である。Step 4（incremental parse）は parse が 22ms（6.0%）に留まるため、cache I/O 改善後の次の候補となる。

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

**A → B → C は実装済みで、cache I/O Round 1（delta write）+ Round 2（dirty-tracking store + warm-cache lookup）が完了した。** suffix rebuild 改善と収束ループ分析に加え、Round 1 では compact JSON serialization・`partition_hashes` 追跡・未変更 blob write skip を導入し、Round 2 では clean partition の serialization スキップ（dirty-tracking）と前回 lookup の blob 再利用（warm-cache lookup）を導入した。with-ref 5-run median は 588ms → 505ms → 369ms、cache I/O は 460ms（78%）→ 389ms（77.0%）→ 252ms（68.3%）と段階的に低下した。cache_store は 289ms → 101ms（-65.1%）に劇的改善したが、cache_load は 171ms → 151ms（-11.7%）に留まり dominant stage にシフトした。次の主計画は cache_load 151ms（40.9%）を中心とした lazy partition load と binary serialization format の導入である。D（incremental parse）は parse が 22ms（6.0%）に留まるため、cache I/O 改善後の候補とする。

### 根拠

1. **既存 API の活用**: Option B は `segment_source_span`、`build_vlist_for_partition_continuing()`、`paginate_vlist_continuing_detailed()` など既存の continuation API を直接活用できる。全面的な parser rewrite を先送りしつつ、typeset の支配的コストを大幅に削減できる
2. **段階的な効果検証**: Step 0 + 収束ループ分析で `pass_count=1` / cache I/O 78% を確認し、Round 1（delta write）で 505ms / 389ms、Round 2（dirty-tracking + warm-cache）で 369ms / 252ms まで低下した。cache_store は 101ms まで改善し dominant stage が cache_load 151ms にシフトした。次段階は cache_load を中心に lazy partition load / binary format で cache I/O を先に削り、その後に parse 最適化の必要性を再判定できる
3. **リスクの局所化**: A は低リスク、B と C は中リスクだが影響範囲が異なる module に閉じる。D の高リスクを後回しにすることで、早期に効果を得られる
4. **`CachedTypesetFragment` の進化方向**: 次の粒度は「paragraph 単体 cache」より「block checkpoint + page suffix」が現実的。footnote/float/page-shift の継続状態を保持できるため

### 前提条件

- **計装の先行実装（Step 0 + 収束ループ分析完了済み）**: `StageTiming` と `pass_count` により stage 別 timing と収束回数の取得基盤が整った。baseline では with-ref benchmark の total 588ms・cache I/O 78% を確認し、Round 1（delta write）後は total 505ms・cache I/O 389ms（77.0%）、Round 2（dirty-tracking + warm-cache）後は total 369ms・cache I/O 252ms（68.3%）・`pass_count=1` を確認済み
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

1. **依存検出の最適化**: **完了（Step 1 Slice 1）**。`CompileCache::detect_changes()` に `changed_paths: &[PathBuf]` パラメータを追加し、watch/scheduler から変更ファイルパスを直接渡す。全 node 走査をスキップする fast path を実装
2. **Cache 分割**: **完了**。v5 split cache 形式（`{cache_key}/index.json` + `partitions/*.json`）を実装し、v4 fallback 互換、partition 個別破損の graceful degrade、directory-based eviction を導入
3. **inotify/kqueue 連携**: **完了**。`FileWatcher` trait を導入し、`PollingFileWatcher` に path canonicalize 内包と debounce を実装。inotify/kqueue バックエンドが同じ trait を実装できる基盤を整備

**受入基準**: cache load/store の stage timing が Step 0 比で 50% 以上削減。依存検出が O(changed files) で完了する。

### Step 2: Block checkpoint reuse — 完了

詳細設計文書: [design-step2-block-checkpoint-reuse.md](design-step2-block-checkpoint-reuse.md)

1. **`RecompilationScope::BlockLevel` 拡張**: `FullDocument | LocalRegion` に加えて `BlockLevel { affected_partitions, references_affected, pagination_affected }` を導入し、block-level reuse が可能な partition を明示できるようにした
2. **Block checkpoint の導入**: `BlockCheckpoint`, `BlockLayoutState`, `PendingFloat`, `BlockCheckpointData` を追加し、`CachedTypesetFragment.block_checkpoints: Option<BlockCheckpointData>` を `#[serde(default)]` 付きで保持することで Step 1 cache との後方互換を維持した
3. **Checkpoint 生成 + suffix rebuild**: `document_nodes_to_vlist_with_state()` で block 境界 checkpoint を収集し、`find_affected_block_index()` と `suffix_rebuild()` により変更 block 以降だけを rebuild する path を実装した。pagination は `paginate_vlist_continuing_with_state()` で初期 float queue を外部注入して継続できる
4. **Fallback 条件の実装**: preamble 変更、`\pageref`、`typeset_callback_count > 1`、checkpoint 不在、block 構造変化、float / footnote 不整合で partition または full document fallback する。先頭 block 変更（`affected_block_index == 0`）は suffix rebuild 経路で処理し、ページ数変化も `suffix_rebuild()` 側で許容する

**実装結果**: 変更 block 以降のみ再 typeset する block-level reuse path と、横断参照収束パスで block reuse を無効化する guard が導入された。`block_checkpoint_single_paragraph_edit_parity`、`block_checkpoint_heading_addition_fallback`、suffix rebuild の footnote/float 系 test などで parity/fallback を検証済み。

**有効化状態**: benchmark 条件で `BlockLevel` scope が生成・適用されることを確認済み。`compile_cache.rs` が block checkpoint を持つ partition で `LocalRegion` → `BlockLevel` に昇格し、`compile_job_service.rs` の `partial_typeset_available` が `BlockLevel` を受理し、`TypesetterReusePlan::create()` が `primary_input_changed` ガードをバイパスする。WU-5 再 profiling では staged `FTX-BENCH-001` の変更 partition が `SuffixRebuild` として処理され、従来の `SuffixValidationFailed` fallback は再現しなかった。baseline の with-ref 5-run median 588ms / `pass_count=1` から、Round 1 の 505ms、Round 2 の 369ms / `pass_count=1` まで低下した。Step 2 の改善後も主要 frontier は cache I/O であり、multi-pass guard は不要と判明した。次の作業は split cache の read/write I/O 最適化である（§7 参照）。

### Step 3: Per-page payload reuse（deterministic full rewrite） — 完了

1. **Cache v7 と page payload 永続化**: `PageRenderPayload.stream_hash` と `CachedPagePayload` を導入し、page content stream・annotation・opacity graphics state を split cache に保存する。`CACHE_VERSION` は 6 → 7 に更新した
2. **Pre-rendered payload 注入経路**: `reusable_page_payloads_for_render()` が `PartitionTypesetDetail` から `Cached` / `BlockReuse` partition を抽出し、対応ページの cached payload を `PdfRenderer::render_with_partition_plan()` に注入する
3. **Deterministic full rewrite**: `PageRenderPayload::try_from_cached()` による hash 検証を通過した payload のみ再利用しつつ、catalog/xref/trailer を含む PDF 全体は毎回 fresh に再生成する。粒度は「dirty page 厳密判定」ではなく「reuse 可能 partition 以外のページ再 render」である
4. **Guard 1 + Guard 2**: Guard 1 として `compile_job_service.rs` 側で fallback partition を含む文書、先頭 partition が複数ページにまたがる frontmatter/TOC 系構成、reindexed XObject page を reuse 対象から除外する。Guard 2 として `pdf/api.rs` 側で XObject resource を持つページ、または stream hash 不一致 payload を必ず再 render する

**実装結果**: `per_page_payload_reuse_matches_full_and_reduces_pdf_render_stage` で 40 chapter report の 1 chapter edit に対して `reused_pages=39` / `rendered_pages=1` と fresh full compile との byte-identical を確認した。加えて TOC 文書回帰、XObject guard、invalid hash guard を含む core 3 件 + application 3 件 + 回帰 2 件の関連テストが pass している。

**未完 / 制限**: page payload reuse は `Cached` / `BlockReuse` partition のページだけが対象で、`SuffixRebuild` / `FullRebuild` partition は全ページ再 render する。external image / embedded PDF graphic を含むページと fallback partition 文書では safety-first で reuse を無効化する。したがって Step 3 完了は render 側 frontier を閉じたことを意味するが、`REQ-NF-002` の 100ms 達成そのものは未完である。

### Step 4: Incremental parse（未着手 / 条件付き）

Step 3 完了後も parse は全文のまま残る。WU-5 再 profiling では typeset 側の `SuffixValidationFailed` fallback は再現せず、変更 partition は suffix rebuild 経路で処理された。Round 2 後の with-ref benchmark でも parse は 22ms（6.0%）に留まるため、Step 4 は cache I/O 最適化後に再評価する。

1. **Partition 単位の parsed IR cache**: 変更のない partition の parsed body をキャッシュし再 parse を省略
2. **Invalidation scope**: preamble 変更・macro 定義変更は全 partition を invalidate。本文変更は affected partition のみ
3. **Global state isolation**: `\gdef` / counter / length register の partition 間副作用の追跡機構

**受入基準**: parse stage timing が O(changed partitions) に削減。global state の副作用が正しく伝播する。

## 5. 主要リスク

| リスク | 影響度 | 発生可能性 | 緩和策 |
|---|---|---|---|
| **Footnote/float 継続状態の不整合** | 高 | 中 | `compile_job_service.rs:2650` 周辺の footnote merge を集中テスト。checkpoint に footnote queue 状態を含める |
| **Cross-reference 収束の non-termination** | 高 | 低 | `\pageref` 含む文書で suffix rebuild 後の収束を byte-identical で検証（既存 `incremental_xref_convergence_after_page_shift` の拡張） |
| **Stage timing 計測なしでの最適化着手** | 緩和済み | — | Step 0 + 収束ループ分析完了により `StageTiming` / `pass_count` 計装が稼働中。baseline で cache I/O 78%・`pass_count=1` を確認し、Round 2 後は cache I/O 252ms（68.3%）が支配的と確認済み。残タスクは cache_load 151ms を中心とする cache I/O 最適化 |
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

Step 0〜3 の設計方針により、1 段落変更時のパイプラインは以下のように変化する:

```
現時点: O(changed files) → O(全文 parse) → O(validated suffix typeset) → O(reuse 不能 partition の page render + full rewrite) → O(changed partition cache)
次段階候補: O(changed files) → O(changed partitions parse) → O(validated suffix typeset) → O(changed pages render + full rewrite) → O(changed partition cache)
            Step 1            (Step 4が必要なら改善)        Step 2 改善済み                 Step 3                   Step 1/3
```

現時点で Step 1〜3 は実装済みであり、依存検出は O(changed files)、render は supported path で O(reuse 不能 partition のページ) まで縮小された。Step 3 では `PageRenderPayload.stream_hash` / `CachedPagePayload` / cache v7 を導入し、`PartitionTypesetDetail` が `Cached` / `BlockReuse` と判定した partition のページだけを full rewrite に再注入する。XObject-backed page と fallback partition 文書では safety-first で reuse を無効化する。

**profiling 現況**: WU-5 で `cargo test -p ferritex-application typeset_dominance_diagnostic -- --ignored --nocapture` を再実行し、`StageTiming.typeset_partition_details` の診断出力を確認した。1000-section staged input の 1 段落変更では `cached=999 partition`, `block_reuse=0`, `suffix_rebuild=1`, `full_rebuild=0` であり、変更対象 partition は `reuse=SuffixRebuild, suffix=2/4, fallback=None` を記録した。以前の `SuffixValidationFailed` fallback は再現せず、suffix rebuild 経路が正常化したことが確認できる。

**incremental benchmark 実測（2026-04-05）**: `incremental_stage_timing_5run_median`（no-ref）と `incremental_stage_timing_with_refs_5run`（`\ref` + `\tableofcontents`）で、1000-section staged input の 1 段落変更を 5-run 計測した。

| Variant | cache_load | source_tree_load | parse | typeset | pdf_render | cache_store | total | pass_count |
|---|---:|---:|---:|---:|---:|---:|---:|---|
| no-ref | 144ms | 23ms | 17ms | 57ms | 9ms | 253ms | 520ms | 1, 1, 1, 1, 1 |
| with-ref | 171ms | 24ms | 23ms | 57ms | 12ms | 289ms | 588ms | 1, 1, 1, 1, 1 |

with-ref path でも `pass_count` は **1** であり、`cached_cross_reference_seed` により pass 1 で全 `\ref` が解決される。multi-pass は発生しない。したがって、cross-reference convergence loop そのものは現在の hot path の律速ではない。

**旧計測値との関係**: 以前の 66.782s は `--no-cache` 初期 compile を同一 run に含む debug 条件の wall-clock であり、hot incremental path の比較基準としては不適切だった。現在の test-profile benchmark の比較対象は上表の 520ms / 588ms である。

### Stage breakdown evidence (2026-04-05)

with-ref 5-run median の stage 構成は以下の通り。

| Stage | Median | 割合 |
|---|---:|---:|
| `cache_store` | 289ms | 49.2% |
| `cache_load` | 171ms | 29.1% |
| `typeset` | 57ms | 9.7% |
| `source_tree_load` | 24ms | 4.1% |
| `parse` | 23ms | 3.9% |
| `pdf_render` | 12ms | 2.0% |
| `total` | 588ms | 100.0% |

注: この表は delta write 最適化前の baseline（with-ref 588ms）を保持する。更新値は次節の「Cache I/O delta write optimization (2026-04-05)」を参照。

baseline 時点の主要律速は `cache_store` + `cache_load` の cache I/O であり、合計 **460ms（78%）** を占める。実計算（parse + typeset + pdf_render）は **92ms（15.6%）** に留まるため、当時の最優先ターゲットは split cache の serialize/deserialize 高速化、lazy load、差分/非同期書き込みなどの cache I/O 最適化だった。Step 4（incremental parse）は cache I/O 改善後に再評価する方針だった。

### Cache I/O delta write optimization (2026-04-05)

`incremental_stage_timing_with_refs_5run` を、split cache write path の delta write 最適化後に再計測した。実施内容は (1) index / partition blob の compact JSON serialization、(2) `partition_hashes` による前回 blob hash の追跡、(3) hash 一致かつファイル存在時の未変更 blob 書き込み skip である。

| Stage | Median | % |
|---|---:|---:|
| `cache_store` | 245ms | 48.5% |
| `cache_load` | 144ms | 28.5% |
| `typeset` | 55ms | 10.9% |
| `source_tree_load` | 23ms | 4.6% |
| `parse` | 22ms | 4.4% |
| `pdf_render` | 12ms | 2.4% |
| `total` | 505ms | 100.0% |
| `cache I/O` | 389ms | 77.0% |
| `computation` | 89ms | 17.6% |

| Stage | Before | After | Delta | Delta % |
|---|---:|---:|---:|---:|
| `cache_store` | 289ms | 245ms | -44ms | -15.2% |
| `cache_load` | 171ms | 144ms | -27ms | -15.8% |
| `cache I/O total` | 460ms | 389ms | -71ms | -15.4% |
| `total` | 588ms | 505ms | -83ms | -14.1% |

cache I/O は依然として **389ms（77.0%）** で支配的だが、baseline の 460ms からは明確に低下した。単一 stage としては `cache_store` 245ms（48.5%）が依然最大だが、write 側は今回の delta write で部分対応済みである。したがって、cache I/O 内の次の未着手機会は `cache_load` 144ms（28.5%）を中心とする read-side 最適化であり、次 frontier は lazy partition load、async / deferred write、binary serialization format の順で検討する。Step 4（incremental parse）は parse 22ms（4.4%）のため、その後に再評価する。

### Cache I/O Round 2: Dirty-tracking store + warm-cache lookup (2026-04-05)

Round 1（delta write）に続き、(1) dirty-tracking store（clean partition blob の serialization 自体をスキップ）と (2) warm-cache lookup（前回 lookup の partition blob を `WarmPartitionCache` として保持し、unchanged partition の read + deserialize をスキップ）を導入した。

| Stage | Median | % |
|---|---:|---:|
| `cache_load` | 151ms | 40.9% |
| `source_tree_load` | 24ms | 6.5% |
| `parse` | 22ms | 6.0% |
| `typeset` | 56ms | 15.2% |
| `pdf_render` | 12ms | 3.3% |
| `cache_store` | 101ms | 27.4% |
| `total` | 369ms | 100.0% |
| `cache I/O` | 252ms | 68.3% |
| `computation` | 114ms | 30.9% |

5-run 詳細:
- cache_load: 162ms, 151ms, 151ms, 152ms, 151ms
- cache_store: 102ms, 101ms, 100ms, 101ms, 101ms
- total: 382ms, 369ms, 369ms, 370ms, 369ms
- pass_count: 1, 1, 1, 1, 1

| Stage | Baseline | Round 1 | Round 2 | R1→R2 Delta | R1→R2 % | Baseline→R2 Delta | Baseline→R2 % |
|---|---:|---:|---:|---:|---:|---:|---:|
| `cache_store` | 289ms | 245ms | 101ms | -144ms | -58.8% | -188ms | -65.1% |
| `cache_load` | 171ms | 144ms | 151ms | +7ms | +4.9% | -20ms | -11.7% |
| `cache I/O total` | 460ms | 389ms | 252ms | -137ms | -35.2% | -208ms | -45.2% |
| `total` | 588ms | 505ms | 369ms | -136ms | -26.9% | -219ms | -37.2% |

**分析**:

- **cache_store が劇的に改善**（245ms → 101ms、-58.8%）。dirty-tracking により clean partition の `serde_json::to_vec` + hash 計算をスキップした効果が顕著
- **cache_load は横ばい**（144ms → 151ms、+4.9%）。warm-cache lookup は `WarmPartitionCache` を導入したが、partition_hashes の比較と blob の条件付きスキップのオーバーヘッドにより、計測ノイズ範囲の微増。read-side の根本的改善には lazy partition load（必要な partition のみ read）または binary format が必要
- **dominant stage がシフト**: `cache_store`（48.5%）→ `cache_load`（40.9%）。write 側の最適化が一段落し、read 側が次の主要 frontier に確定
- **computation は 114ms（30.9%）** に留まり、cache I/O 252ms（68.3%）との間に 2.2 倍の差がある。100ms 目標に対しては computation 単独でも超過（114ms）しているが、source_tree_load 24ms は cache I/O 改善後に副次的に対処可能な範囲

**次 frontier**: cache I/O 252ms（68.3%）が引き続き支配的。cache_load 151ms（40.9%）が最大の単一 stage であり、(1) lazy partition load（必要な partition blob のみ read + deserialize）、(2) binary serialization format（bincode 等による encode/decode 高速化）、(3) index 分離（partition_hashes のみ先行 read し、blob 本体は遅延 load）が候補。cache_store 101ms（27.4%）は dirty-tracking で大幅改善済みだが、async / background write で残りをさらに削れる可能性がある。Step 4（incremental parse）は parse 22ms（6.0%）のため、cache I/O 改善後に再評価する。
