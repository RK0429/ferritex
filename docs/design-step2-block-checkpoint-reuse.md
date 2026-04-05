# REQ-NF-002 Step 2: Block Checkpoint Reuse — 詳細設計

## Meta

| Item | Value |
|---|---|
| Version | 0.1.0 |
| Date | 2026-04-04 |
| Status | Implemented |
| Parent | design-incremental-100ms-optimization.md §4.2 |
| Scope | partition 内 block 粒度 checkpoint による typeset suffix rebuild |
| Input | Step 1 完了コードベース、profiling データ（typeset 91.1%）|

## 1. 背景と目的

### 1.1 Step 1 完了後のボトルネック

Step 0 profiling により、`FTX-STRESS-2000` stress benchmark（1000-section / 2000-cycle staged input）の warm incremental compile で typeset が全体の 91.1%（14.678s / 16.106s）を占めることが判明している。現在の `TypesetterReusePlan` は **partition 粒度**で reuse/rebuild を判定するため、1 段落変更でもその partition（section）全体を rebuild する。

Step 2 の目的は、partition 内の **block 粒度**で checkpoint を取り、変更 block 以降の suffix のみ rebuild することで、typeset stage のコストを partition サイズではなく「変更 block + suffix」のサイズに比例させることである。

### 1.2 受入基準（親設計文書より）

> 1 段落変更時の typeset stage timing が Step 1 比で 80% 以上削減。full fallback が必要なケースが正しく判定される。

### 1.3 既存 API のアセスメント

Step 2 が活用する既存 API:

| API | 現在の役割 | Step 2 での活用 |
|---|---|---|
| `segment_source_span_for_nodes()` | VList item と source location の対応を算出 | block 境界と source span の対応付け |
| `build_vlist_for_partition_continuing()` | partition 内 body nodes を VList に変換。`continues_from_previous_block` パラメータで前ブロックからの継続制御 | 変更 block 以降の body nodes だけを渡して suffix VList を構築 |
| `paginate_vlist_continuing_detailed()` | `initial_content_used` を受けて途中ページから pagination 継続 | checkpoint の layout state から suffix ページを生成 |
| `extract_footnotes_from_nodes()` | body nodes から footnote を分離して `FootnoteEntry` リストを生成 | suffix rebuild 時に suffix nodes の footnote のみを抽出 |

## 2. データ構造設計

### 2.1 `RecompilationScope` の拡張

**現在の定義** (`incremental/api.rs:12`):

```rust
pub enum RecompilationScope {
    FullDocument,
    LocalRegion,
}
```

**変更後**:

```rust
/// ファイル: ferritex-core/src/incremental/api.rs

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum RecompilationScope {
    FullDocument,
    LocalRegion,
    /// Step 2: block-level reuse が可能。affected_partitions が特定済み
    BlockLevel {
        affected_partitions: Vec<String>,
        references_affected: bool,
        pagination_affected: bool,
    },
}
```

**判定ロジック**: `CacheLookupResult` の構築時、以下の条件で `BlockLevel` を返す:

1. `LocalRegion` の条件を満たす（preamble 変更なし）
2. `has_pageref_markers() == false`（`\pageref` なし）
3. 対応する partition の `BlockCheckpointData` がキャッシュに存在する
4. LOF/LOT/index エントリの追加・削除・変更がない

条件を満たさない場合は既存の `LocalRegion` または `FullDocument` にフォールバック。

### 2.2 `BlockCheckpointData` — ブロック境界の checkpoint

```rust
/// ファイル: ferritex-application/src/compile_cache.rs（新規構造体）

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct BlockCheckpoint {
    /// この block の先頭に対応する body_nodes 内のインデックス
    pub node_index: usize,
    /// block 先頭の source span（block 特定用）
    pub source_span: Option<SourceSpan>,
    /// この block の直前時点での layout state
    pub layout_state: BlockLayoutState,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct BlockLayoutState {
    /// ページ上で既に使われた高さ（pagination の initial_content_used に対応）
    pub content_used: DimensionValue,
    /// checkpoint 時点までに確定済みのページ数
    pub completed_page_count: usize,
    /// 未配置 float queue の snapshot
    pub pending_floats: Vec<PendingFloat>,
    /// footnote counter（partition 開始からの累積）
    pub footnote_count: usize,
    /// float counter snapshot（figure/table 番号の継続用）
    pub figure_count: u32,
    pub table_count: u32,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PendingFloat {
    pub spec: PlacementSpec,
    pub content: FloatContent,
    pub defer_count: u32,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct BlockCheckpointData {
    /// partition 内の block 境界 checkpoint 一覧（node_index 昇順）
    pub checkpoints: Vec<BlockCheckpoint>,
    /// checkpoint 生成時の partition source hash
    pub source_hash: u64,
}
```

**保存粒度**: `BlockCheckpointData` は partition ごとに 1 つ。内部の `checkpoints` ベクタが各 block 境界を保持する。初期実装では `prefix_vlist_items` / `prefix_pages` / `prefix_footnotes` は定義せず、`CachedTypesetFragment.fragment.pages[..completed_page_count]` の動的切り出しだけで prefix を復元する。

### 2.3 `CachedTypesetFragment` の拡張

```rust
/// ファイル: ferritex-application/src/compile_cache.rs

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct CachedTypesetFragment {
    pub fragment: DocumentLayoutFragment,
    pub source_hash: u64,
    /// Step 2: partition 内の block checkpoint data（None = Step 1 以前のキャッシュ）
    #[serde(default)]
    pub block_checkpoints: Option<BlockCheckpointData>,
}
```

`#[serde(default)]` により、Step 1 以前のキャッシュとの後方互換を維持する。`block_checkpoints` が `None` の場合は partition 全体 rebuild にフォールバック。

### 2.4 `PartitionBlob` の拡張（v5 split cache 互換）

`PartitionBlob` (`compile_cache.rs:98`) への変更は不要。`CachedTypesetFragment` の内部拡張のみで、既存の serialize/deserialize パスがそのまま動作する。

**必要な derive 追加**:

| 構造体 | 現在の derive | 追加必要 |
|---|---|---|
| `VListItem` (`typesetting/api.rs:952`) | `—`（enum、derive なし） | `Serialize, Deserialize` |
| `TeXBox` (`typesetting/api.rs:870`) | `Debug, Clone, PartialEq, Eq` | `Serialize, Deserialize` |
| `PlacementSpec` (`typesetting/api.rs:363`) | `Debug, Clone, PartialEq, Eq` | `Serialize, Deserialize` |
| `HListItem` (`typesetting/api.rs:923`) | `Debug, Clone, PartialEq, Eq` | `Serialize, Deserialize`（VListItem::Box の content 復元には不要だが、VList 全体 cache 時に必要） |
| `FootnoteEntry` (`typesetting/api.rs:396`) | `Debug, Clone, PartialEq, Eq` | `Serialize, Deserialize`（field `text`, `source_span` を pub 化） |
| `FloatQueue` / `FloatItem` | `Debug, Clone, PartialEq, Eq` | 直接 serialize しない。`PendingFloat` として再定義 |

**既に Serialize/Deserialize を持つ型**（変更不要）: `TextLine`, `TextLineLink`, `FloatContent`, `FloatRegion`, `FloatPlacement`, `TypesetPage`, `TypesetImage`, `GraphicsBox`, `IndexRawEntry`, `SourceSpan`, `DimensionValue`, `PageBox`, `DocumentLayoutFragment`

## 3. アルゴリズム設計

### 3.1 Block 境界の定義

`document_nodes_to_vlist_with_state()` (`typesetting/api.rs:1863`) の処理フローに基づき、以下の `DocumentNode` が block 境界を形成する:

| Block 境界 | 対応する DocumentNode | SegmentBlockKind |
|---|---|---|
| 段落区切り | `ParBreak` | `Paragraph` |
| ページ区切り | `PageBreak`, `ClearPage`, `ClearDoublePage` | — |
| 見出し | heading 系ノード | `Heading`, `ChapterTitle` |
| Display math | `DisplayMath`, `EquationEnv` | — |
| Float | `Float` | — |
| 画像 | `IncludeGraphics`, `TikzPicture` | — |
| リスト項目 | list/description 系 | `ListItem`, `DescriptionItem` |

**checkpoint 生成タイミング**: `ParBreak` の処理後（VList に `Glue { parskip }` を追加した直後）が主要な checkpoint 位置。Heading、DisplayMath、Float、IncludeGraphics の処理後にも checkpoint を生成する。

### 3.2 Checkpoint 生成パス（full typeset 時）

Full typeset のパイプラインを拡張し、partition ごとの typeset 結果から `BlockCheckpointData` を同時に生成する。

```
[既存パス]
body_nodes → document_nodes_to_vlist_with_state() → VList → paginate_vlist_continuing_detailed() → pages

[追加パス — checkpoint 収集]
document_nodes_to_vlist_with_state() 内で ParBreak/Heading/Float 等の処理時に
checkpoint_collector に (node_index, source_span, current_vlist_snapshot) を push

paginate_vlist_continuing_detailed() 内で各 checkpoint の node_index に対応する
VList 位置での layout_state (content_used, completed_page_count, float_queue) を記録
```

**実装方針**: `document_nodes_to_vlist_with_state()` に `checkpoint_collector: Option<&mut Vec<RawBlockCheckpoint>>` パラメータを追加する。`None` の場合は既存パスと完全に等価（zero-cost）。

### 3.3 変更 Block の特定

1. `CacheLookupResult` で `RecompilationScope::BlockLevel` が返された partition について処理する
2. 現在の source text から body nodes を取得する
3. キャッシュされた `BlockCheckpointData.checkpoints` の `source_span` と、現在の body nodes の source span を照合する
4. **最初の不一致 checkpoint** を `affected_block_index` として特定する

```rust
fn find_affected_block_index(
    checkpoints: &[BlockCheckpoint],
    current_nodes: &[DocumentNode],
    current_source: &str,
) -> Option<usize> {
    // 各 checkpoint の source_span が示すソース範囲のテキストを比較
    // 最初に不一致が見つかった checkpoint index を返す
    // 全一致 → None（変更なし、reuse 可能）
}
```

### 3.4 Suffix Rebuild パス

変更 block が特定された後の処理フロー:

```
1. affected_block_index の checkpoint から layout_state を復元
2. checkpoint 以前の prefix_pages をキャッシュから復元
3. affected_block_index 以降の body_nodes（suffix nodes）を切り出す
4. suffix nodes の footnotes を extract_footnotes_from_nodes() で分離
5. suffix nodes を build_vlist_for_partition_continuing() に渡す
   - continues_from_previous_block = true
6. suffix VList を paginate_vlist_continuing_detailed() で pagination
   - initial_content_used = checkpoint.layout_state.content_used
7. prefix_pages + suffix_pages を結合して DocumentLayoutFragment を構築
8. prefix_footnotes + suffix_footnotes を結合して append_footnotes_to_pages()
```

**float queue 復元**: `BlockLayoutState.pending_floats` から `FloatQueue` を再構築し、suffix の `paginate_vlist_continuing_detailed()` に渡す。現在 `paginate_vlist_continuing_detailed()` は float queue を内部で `FloatQueue::new()` する設計のため、**初期 float queue を外部から注入可能にする拡張**が必要。

```rust
/// ファイル: ferritex-core/src/typesetting/api.rs — 新規関数

pub fn paginate_vlist_continuing_with_state(
    vlist: &[VListItem],
    page_box: &PageBox,
    layout: ClassLayout,
    initial_content_used: DimensionValue,
    initial_float_queue: FloatQueue,
) -> PaginatedVListContinuation {
    // paginate_vlist_continuing_detailed() と同一ロジックだが
    // float_queue の初期値を外部から受け取る
}
```

### 3.5 Integration Point: `try_partial_typeset_document()` の拡張

`compile_job_service.rs:2461` の `try_partial_typeset_document()` を拡張し、`BlockLevel` スコープ時に block-level suffix rebuild を適用する。

```
[現在のパス]
try_partial_typeset_document():
  for each rebuild_partition:
    partition_document → build_vlist_for_partition_continuing() → 全 body nodes typeset
    extract_rebuilt_fragment()

[Step 2 追加パス]
try_partial_typeset_document():
  for each rebuild_partition:
    if block_checkpoints available:
      find_affected_block_index()
      if found:
        suffix_rebuild()  ← 新規パス
      else:
        reuse from cache (no change in this partition)
    else:
      [既存パス — partition 全体 rebuild]
```

## 4. Fallback 条件

以下の場合は block-level reuse を無効化し、partition 全体 rebuild（または full document rebuild）にフォールバックする:

### 4.1 Full Document Fallback

| 条件 | 理由 | 判定箇所 |
|---|---|---|
| Preamble 変更 | レイアウトパラメータが全面的に変わり得る | `TypesetterReusePlan::create()` |
| `\pageref` 存在 | ページ番号ずれの伝播が partition をまたぐ | `document.has_pageref_markers()` |
| LOF/LOT エントリの追加・削除・変更 | multi-pass 収束が必要 | cross-reference seed 差分比較 |
| Index エントリの追加・削除・変更 | multi-pass 収束が必要 | cross-reference seed 差分比較 |
| `typeset_callback_count > 1` | cross-reference 収束パスでは block reuse が不正確 | 既存判定（`compile_job_service.rs:1179`） |

### 4.2 Partition-level Fallback（block reuse 無効化）

| 条件 | 理由 | 判定箇所 |
|---|---|---|
| `BlockCheckpointData` がキャッシュに存在しない | Step 1 以前のキャッシュ、または初回 compile | `CachedTypesetFragment.block_checkpoints == None` |
| Checkpoint の source_span 照合で block 境界自体が変わった | section 追加・削除等で block 構造が変化 | `find_affected_block_index()` |
| Float reorder が発生し affected block 以降の float counter が不整合 | float 番号のずれが partition 境界を超え得る | suffix rebuild 後の float count 検証 |
| Affected block が partition の最初の block | prefix が空なので partition 全体 rebuild と等価 | `affected_block_index == 0` |

### 4.3 Fallback 検証（suffix rebuild 後）

Suffix rebuild の結果を検証し、以下の場合は partition 全体 rebuild にフォールバック:

1. **ページ数の変化**: suffix rebuild でページ数が変わった場合、後続 partition の page offset がずれる。pagination_affected フラグを立て、後続 partition も rebuild する
2. **Float 配置の不整合**: suffix の float 配置が prefix の float count と整合しない場合
3. **Footnote 番号の不整合**: suffix の footnote count が prefix の footnote_count と連続しない場合

## 5. 対象ファイルと変更スコープ

### 5.1 主要変更ファイル

| ファイル | 変更内容 | 推定行数 |
|---|---|---|
| `ferritex-core/src/incremental/api.rs` | `RecompilationScope` を enum 拡張 | ~30 行 |
| `ferritex-core/src/typesetting/api.rs` | (1) `VListItem`, `FootnoteEntry` に `Serialize`/`Deserialize` 追加 (2) `paginate_vlist_continuing_with_state()` 追加 (3) `document_nodes_to_vlist_with_state()` に checkpoint collector 追加 | ~120 行 |
| `ferritex-application/src/compile_cache.rs` | (1) `BlockCheckpoint`, `BlockLayoutState`, `BlockCheckpointData` 構造体追加 (2) `CachedTypesetFragment` 拡張 (3) checkpoint data の serialize/deserialize | ~100 行 |
| `ferritex-application/src/compile_job_service.rs` | (1) `try_partial_typeset_document()` 内の block-level suffix rebuild パス (2) checkpoint 生成パスの組み込み (3) fallback 判定ロジック | ~200 行 |

### 5.2 副次的変更ファイル

| ファイル | 変更内容 | 推定行数 |
|---|---|---|
| `ferritex-core/src/typesetting/api.rs` | `PlacementSpec`, `FloatContent` への Serialize/Deserialize derive 確認・追加 | ~10 行 |
| `ferritex-core/src/kernel/source_span.rs` | 変更なし（Serialize/Deserialize 既存） | 0 行 |
| `ferritex-core/src/compilation/partition.rs` | 変更なし | 0 行 |

**推定合計**: ~460 行

## 6. テスト計画

### 6.1 Unit Tests（ferritex-core）

| テスト | 対象 | 検証内容 |
|---|---|---|
| `block_checkpoint_collected_at_par_break` | `document_nodes_to_vlist_with_state()` | ParBreak ごとに checkpoint が生成されることを確認 |
| `block_checkpoint_collected_at_heading` | 同上 | Heading ノードで checkpoint が生成されることを確認 |
| `block_checkpoint_collected_at_float` | 同上 | Float ノードで checkpoint が生成されることを確認 |
| `paginate_continuing_with_initial_floats` | `paginate_vlist_continuing_with_state()` | 初期 float queue を渡して pagination が正しく動作 |
| `suffix_vlist_continues_from_checkpoint` | `build_vlist_for_partition_continuing()` | suffix nodes の VList が checkpoint 状態から正しく継続 |
| `checkpoint_serialization_roundtrip` | `BlockCheckpointData` | serialize → deserialize で等価性を確認 |

### 6.2 Unit Tests（ferritex-application）

| テスト | 対象 | 検証内容 |
|---|---|---|
| `find_affected_block_single_paragraph_change` | `find_affected_block_index()` | 1 段落変更で正しい block index が返る |
| `find_affected_block_no_change_returns_none` | 同上 | 変更なしで None が返る |
| `find_affected_block_heading_added` | 同上 | Heading 追加で block 構造変化を検出し fallback |
| `suffix_rebuild_produces_correct_pages` | suffix rebuild パス | prefix_pages + suffix_pages が partition 全体 rebuild と同一出力 |
| `suffix_rebuild_with_footnotes` | 同上 | footnote 含む段落変更で footnote 番号・配置が正確 |
| `suffix_rebuild_with_pending_floats` | 同上 | checkpoint の float queue が正しく復元され配置される |
| `block_checkpoint_missing_falls_back_to_partition` | fallback 判定 | `block_checkpoints == None` で partition 全体 rebuild |
| `block_level_scope_with_pageref_falls_back` | fallback 判定 | `\pageref` 存在で `FullDocument` にフォールバック |
| `cached_fragment_v1_without_checkpoints_compatible` | 後方互換 | Step 1 キャッシュの deserialize が成功し fallback 動作 |

### 6.3 Integration Tests（e2e_compile）

| テスト | 検証内容 |
|---|---|
| `block_checkpoint_single_paragraph_edit_parity` | 1 段落変更の block reuse compile と fresh full compile が byte-identical |
| `block_checkpoint_footnote_paragraph_edit_parity` | footnote を含む段落変更で出力 PDF が正確 |
| `block_checkpoint_float_paragraph_edit_parity` | float を含む段落変更で float 配置順序が維持 |
| `block_checkpoint_heading_addition_fallback` | heading 追加時に partition-level fallback が発動 |
| `block_checkpoint_pageref_falls_back_to_full` | `\pageref` 含む文書で full fallback |
| `block_checkpoint_toc_update_after_edit` | TOC を含む文書での段落変更後に TOC が正しく更新 |
| `block_checkpoint_multi_file_include` | `\include` された file の変更が正しく検出・rebuild |

### 6.4 Benchmark Validation

| 計測 | 方法 | 基準 |
|---|---|---|
| Typeset stage timing | `FTX-STRESS-2000` stress benchmark の 1 段落変更、5-run median | Step 1 比 80%+ 削減 |
| Full vs block reuse parity | byte-identical PDF 比較 | 一致 |
| Checkpoint generation overhead | full compile with/without checkpoint collection | < 5% overhead |

### 6.5 回帰テスト

- `full_bench_warm_incremental_evidence` が引き続き pass
- `incremental_xref_convergence_after_page_shift` が引き続き pass
- REQ-NF-007 parity 5 カテゴリ全 pass 維持
- Step 1 の全テスト（`fast_path_*`, `lookup_reads_legacy_v4_*`, `corrupted_partition_blob_*` 等）pass 維持

## 7. 実装順序

段階的な slice 分割で、各 slice が独立してテスト可能：

### Slice 1: データ構造と Serialize 基盤（~100 行）

1. `VListItem`, `FootnoteEntry`, `PlacementSpec` に `Serialize`/`Deserialize` を追加
2. `FootnoteEntry` の fields を `pub` 化
3. `BlockCheckpoint`, `BlockLayoutState`, `PendingFloat`, `BlockCheckpointData` を定義
4. `CachedTypesetFragment` に `block_checkpoints: Option<BlockCheckpointData>` を追加
5. serialization roundtrip テスト

### Slice 2: Checkpoint 生成（~120 行）

1. `document_nodes_to_vlist_with_state()` に `checkpoint_collector` パラメータ追加
2. `ParBreak`, `Heading`, `Float` 等の処理後に checkpoint を push
3. Full typeset 時に checkpoint 生成を組み込み、`CachedTypesetFragment` に保存
4. checkpoint 生成の unit テスト

### Slice 3: Suffix Rebuild パス（~150 行）

1. `paginate_vlist_continuing_with_state()` の実装
2. `find_affected_block_index()` の実装
3. `try_partial_typeset_document()` 内の block-level suffix rebuild パス
4. Fallback 判定ロジック
5. `RecompilationScope::BlockLevel` の判定と伝播
6. suffix rebuild + fallback の unit テスト

### Slice 4: Integration テストと検証（~90 行）

1. e2e integration テスト一式
2. benchmark validation
3. 回帰テスト確認

## 8. 設計判断（ADR）

### ADR-STEP2-001: Checkpoint 粒度を ParBreak 単位にする

- **Status**: Accepted
- **Context**: Block 境界の粒度として、(a) 全 DocumentNode 単位、(b) ParBreak + 主要構造ノード単位、(c) 固定バイトオフセット単位 が候補
- **Decision**: (b) ParBreak + Heading + Float + DisplayMath を checkpoint 境界とする
- **Consequences**: 1 段落変更のユースケースに最適化される。ParBreak が最頻出の block 境界であり、checkpoint 数が partition 内の段落数と概ね一致する（過多にならない）。構造変更（heading 追加等）は block 構造変化として検出され fallback する

### ADR-STEP2-002: Float queue を checkpoint に含める

- **Status**: Accepted
- **Context**: `paginate_vlist_continuing_detailed()` の内部 `FloatQueue` は関数スコープ内で管理される。suffix rebuild で正確な float 配置を復元するには、checkpoint 時点の float queue 状態が必要
- **Decision**: `BlockLayoutState` に `pending_floats: Vec<PendingFloat>` を含め、`paginate_vlist_continuing_with_state()` で外部注入する
- **Consequences**: `PendingFloat` は `FloatItem` の serializable 版として新規定義。`FloatQueue` 自体への Serialize derive は不要（内部実装を変えない）

### ADR-STEP2-003: Prefix pages をキャッシュに含める

- **Status**: Accepted
- **Context**: Suffix rebuild で prefix ページを復元する方法として、(a) VList を先頭から再 pagination する、(b) 確定済みページをキャッシュする、(c) partition の `DocumentLayoutFragment.pages` から切り出す が候補
- **Decision**: (c) 既存の `CachedTypesetFragment.fragment.pages` から `checkpoint.completed_page_count` で切り出す。`prefix_vlist_items` / `prefix_pages` / `prefix_footnotes` フィールドは初期実装では定義せず、`fragment.pages[..completed_page_count]` からの動的切り出しのみを採用する
- **Consequences**: キャッシュサイズの増加を抑える。prefix payload 専用フィールドを持たないため schema は最小で済む。将来 prefix ページの独立キャッシュが必要になった場合は別 ADR で拡張する

### ADR-STEP2-004: Cross-reference 収束パスでは block reuse を無効化

- **Status**: Accepted
- **Context**: `parse_document_with_cross_references()` は最大 3 pass の parse-typeset ループを回す。2 pass 目以降の typeset で block reuse を適用すると、ページ番号変動による label 解決の不整合リスクがある
- **Decision**: `typeset_callback_count > 1` の場合は block reuse を適用しない（既存の partition-level reuse と同じ条件）。初回 pass のみ block reuse を許可する
- **Consequences**: `\pageref` を含まない文書の 1 段落変更（最も頻出のケース）で効果を発揮する。`\pageref` 含む文書は既存の full typeset パスを維持する

## 9. リスクと緩和策

| リスク | 影響度 | 発生可能性 | 緩和策 |
|---|---|---|---|
| **Footnote 番号ずれ** | 高 | 中 | checkpoint に `footnote_count` を含め、suffix rebuild 後に連続性を検証。不整合時は partition fallback |
| **Float 配置不整合** | 高 | 中 | `PendingFloat` queue を checkpoint に含め、`paginate_vlist_continuing_with_state()` で正確に復元。suffix 後の float count を検証 |
| **Checkpoint 生成 overhead** | 低 | 低 | checkpoint collector は `Option` で条件付き有効化。full compile 時の overhead 目標は < 5% |
| **キャッシュサイズ増加** | 低 | 高 | prefix_pages を動的切り出しに簡略化（ADR-STEP2-003）。VList items の serialize は checkpoint 位置のみで prefix 全体は保存しない |
| **後方互換性** | 低 | 低 | `#[serde(default)]` により Step 1 キャッシュからの deserialize は `block_checkpoints = None` → partition fallback |

## 10. 親設計文書との差分

本文書の設計は `design-incremental-100ms-optimization.md` §4.2 の Step 2 記述と概ね整合するが、以下の点で具体化・修正がある:

1. **`RecompilationScope` の拡張形式**: 親文書では struct への変更を提案。本設計では既存の enum に `BlockLevel` variant を追加する形式を選択（既存の `FullDocument` / `LocalRegion` のパターンマッチを壊さない）
2. **`affected_source_spans` の省略**: 親文書で提案された `affected_source_spans: Vec<SourceSpan>` は、block checkpoint の `source_span` 照合で代替する。独立フィールドとして持つ必要がない
3. **Float queue の明示的な checkpoint 包含**: 親文書では暗黙的だった float 継続状態を、`PendingFloat` 構造体として明示的に serialize 対象に含める
4. **Prefix pages の動的切り出し**: 親文書の「未変更 prefix の VList items はキャッシュから復元」は、初期実装では VList ではなく確定済み pages を `fragment.pages` から切り出す方式に簡略化
