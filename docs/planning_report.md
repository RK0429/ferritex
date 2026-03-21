# Ferritex 完成計画レポート

## 1. 現状サマリー

### 実装済み（アーキテクチャの骨格 + 薄い実装）

| 領域 | 実装レベル | 概要 |
|---|---|---|
| CLI | Shell 完成 | compile/watch/preview/lsp の 4 サブコマンド、全 CLI オプション（REQ-FUNC-042-045） |
| Parser/Tokenizer | 基礎 | catcode テーブル、`\def`/`\gdef`/`\newcommand`/`\renewcommand`（0-2 引数）、group scoping、`\catcode`、基本条件分岐（`\iftrue`/`\iffalse`/`\ifnum`/`\ifx`/`\ifcase`）、count/dimen レジスタ |
| File Input | 部分的 | `\input`/`\include` + current-file-relative/project-root/asset-bundle fallback、`\InputIfFileExists` |
| Typesetting | 基礎 | Knuth-Plass 行分割（glue/penalty モデル）、固定寸法ページレイアウト |
| PDF | 最小限 | PDF 1.4 ヘッダ/xref/trailer、テキスト描画（Helvetica 参照のみ、埋め込みなし） |
| TFM Reader | 完成 | バイナリパース、width/height/depth/italic_correction |
| Policy/Security | フレームワーク完成 | ExecutionPolicy, FileAccessGate, PathAccessPolicy, OutputArtifactRegistry, PreviewPublicationPolicy |
| Compilation Model | 構造完成 | CompilationJob, CompilationSession, JobContext, CompilationSnapshot, DocumentState |
| Application Layer | Shell 完成 | CompileJobService, RuntimeOptions, ExecutionPolicyFactory, WorkspaceJobScheduler, RecompileScheduler, StableCompileState, PreviewSessionService, OpenDocumentStore, LiveAnalysisSnapshotFactory, LspCapabilityService |
| Infra | Shell 完成 | FsFileAccessGate, AssetBundleLoader（stub）, PollingFileWatcher, LoopbackPreviewTransport, ShellCommandGateway |
| Watch | 動作 | ポーリングベースの監視、依存パス再同期 |
| Preview | 動作 | loopback HTTP、session 管理、PDF publish |
| LSP | 基礎 | stdio JSON-RPC、initialize、open/change 時 recompile、label/citation 補完・定義ジャンプ |

### 主要ギャップ（要件→実装の対応不足）

| ID | 要件領域 | ギャップ深刻度 | 主な不足 |
|---|---|---|---|
| A | Parser/Macro (REQ-FUNC-001-006) | 大 | `\edef`, `\expandafter`/`\noexpand`, `\if`/`\ifcat`/`\ifdim`, e-TeX 拡張レジスタ(32768), skip/muskip/toks/box レジスタ, `\begingroup`/`\endgroup`, 3+ 引数マクロ, 再帰深度検出, エラー回復 |
| B | Typesetting (REQ-FUNC-007-012) | 極大 | 実 box モデル(hbox/vbox), ハイフネーション, ページ分割アルゴリズム, フロート配置, 数式組版, 相互参照解決(multi-pass .aux), TOC/索引生成, 脚注 |
| C | PDF (REQ-FUNC-013-016) | 極大 | フォント埋め込み/サブセット化, ToUnicode CMap, ハイパーリンク/しおり, 画像埋め込み(PNG/JPEG/PDF), カラー, proper content stream |
| D | Font (REQ-FUNC-017-019) | 大 | OpenType 読み込み, OverlaySet 経由のフォント解決, フォントマップ, Host Font Catalog |
| E | Package Compat (REQ-FUNC-020-026) | 極大 | LaTeX カーネル互換, document class 読み込み, amsmath, hyperref, tikz/pgf, .bbl 読み込み/Citation Table, fontspec, .sty 汎用読み込み |
| F | Incremental (REQ-FUNC-027-030) | 極大 | 依存グラフ構築, 変更検知, キャッシュ管理, 部分再コンパイル（enum stub のみ） |
| G | Parallelization (REQ-FUNC-031-033) | 極大 | パイプライン並列化, パーティション並列化, フォント処理並列化（未着手） |
| H | LSP (REQ-FUNC-034-037) | 中 | codeAction（修正候補）, hover, definition jump の改善（現状は stub compile 依存） |
| I | Preview/Watch (REQ-FUNC-038-041) | 中 | OS ネイティブ watcher, SyncTeX, view state 保持/復元 |
| J | Asset Bundle (REQ-FUNC-046) | 大 | 実 bundle 形式, manifest 検証, mmap 読み込み, OverlaySet 合成 |

## 2. 完全完成の実現可能性

**単一ラン（1 セッション）での完全完成は不可能。**

本リポジトリは TeX コンパイラの全体設計ドキュメント（要件定義 48 要件 + 非機能要件 10 項目）に対し、アーキテクチャの骨格と CLI/policy/preview/LSP の shell を整備した段階にある。コア処理（TeX パーサー、タイプセッティング、PDF 生成、フォント管理）はプロトタイプレベルであり、パッケージ互換レイヤー・差分コンパイル・並列化は未着手。

推定規模: Must 要件の完全実装に **数十万行規模の Rust コード**が必要。現状の実装量（約 8,000 行）は最終形の 5-10% 程度。

## 3. タスク分解（即座実行可能なスライス）

以下は依存関係を考慮した優先順位順。各タスクは 1 エージェントセッションで完了可能な粒度に分割済み。

### Tier 1: コアパイプライン強化（最優先 — 他の全てが依存）

| # | タスク | 受入基準 | 依存 | 優先度 | ロール |
|---|---|---|---|---|---|
| 1 | **Parser: マクロエンジン拡張** — `\edef` 展開、3+ 引数マクロ、`\expandafter`/`\noexpand`、再帰深度検出（上限 1000）、`\begingroup`/`\endgroup` | `\def\foo#1#2#3{#3#2#1}` → `CBA`、`\edef` が内部マクロを即時展開、再帰超過でエラー＋スタックトレース | なし | 高 | implementer |
| 2 | **Parser: 条件分岐拡張** — `\if`, `\ifcat`, `\ifdim`, LaTeX `\ifthenelse`、ネスト skip の完全対応 | `\if` 系全プリミティブの真偽分岐がネスト含め正しく処理される | #1 | 高 | implementer |
| 3 | **Parser: レジスタ拡張** — e-TeX 拡張（32768 個）、skip/muskip/toks レジスタ、`\newcount`/`\countdef`、グローバル/ローカル区別 | レジスタ 256-32767 番にアクセス可能、group 離脱時に local 値がロールバック | #1 | 高 | implementer |
| 4 | **Parser: エラー回復** — パニックモードリカバリ（同期点までスキップ）、複数エラー報告、ファイル名:行:列の特定 | 複数エラーを含む文書で後続エラーも報告される | #1 | 中 | implementer |
| 5 | **Typesetting: 実 box モデル** — hbox/vbox 構築、width/height/depth プロパティ、ネスト box、glue set 計算 | `\hbox{text}` が正しい寸法の box を生成し、typeset 結果に反映される | #1 | 高 | implementer |
| 6 | **Typesetting: ページ分割** — TeX ページ分割アルゴリズム、`\pagebreak`/`\newpage`/`\clearpage`、脚注スペース考慮 | 100 ページ文書でページ分割位置が TeX アルゴリズムに準拠 | #5 | 高 | implementer |
| 7 | **Typesetting: ハイフネーション** — TeX パターンベースハイフネーション、言語パターン読み込み | 英語テキストのハイフネーション位置が pdfLaTeX と一致 | #5 | 中 | implementer |
| 8 | **PDF: フォント埋め込み基盤** — Type1/TrueType フォント埋め込み、サブセット化、ToUnicode CMap 生成 | PDF にフォントがサブセット埋め込みされ、テキスト検索が機能 | TFM reader 既存 | 高 | implementer |
| 9 | **Font: OpenType 基本読み込み** — `cmap`, `head`, `hhea`, `hmtx` テーブルパース、glyph ID マッピング | OTF/TTF ファイルからメトリクスとグリフ ID が取得可能 | なし | 高 | implementer |

### Tier 2: LaTeX 互換レイヤー（Tier 1 完了後）

| # | タスク | 受入基準 | 依存 | 優先度 | ロール |
|---|---|---|---|---|---|
| 10 | **LaTeX カーネル: 基本構造** — `\documentclass` によるクラス読み込み（article/report/book/letter）、`\usepackage` 機構、環境定義、セクショニング | `\documentclass{article}` + 基本セクショニングの文書が正しく処理される | #1-#6, #8 | 高 | implementer |
| 11 | **相互参照: multi-pass .aux** — `\label`/`\ref`/`\pageref` の解決、.aux 書き出し/読み込み、最大 3 パス | 3 パスで全参照が解決、未定義 `\ref` は `??` + 警告 | #10 | 高 | implementer |
| 12 | **TOC/索引生成** — `\tableofcontents`, `\listoffigures`, `\listoftables`, `\makeindex`/`\printindex` | 目次のセクション番号・タイトル・ページ番号が正しく生成 | #11 | 中 | implementer |
| 13 | **Bibliography: .bbl 読み込み** — BblSnapshot, CitationTable 構築、`\cite` 解決、stale 検出 | 事前生成 `.bbl` から参考文献リストが正しく組版、`\cite` が解決される | #10 | 中 | implementer |
| 14 | **数式組版: 基礎** — インライン/ディスプレイ数式、数式リスト構築、上付き/下付き/分数/根号 | `$x^2$` と `\[...\]` が正しく組版される | #5 | 高 | implementer |
| 15 | **汎用パッケージ読み込み** — .sty パース、`\RequirePackage`、パッケージオプション処理 | Asset Bundle 内の標準パッケージがエラーなく読み込まれる | #10 | 高 | implementer |

### Tier 3: 高度な機能（Tier 2 完了後）

| # | タスク | 受入基準 | 依存 | 優先度 | ロール |
|---|---|---|---|---|---|
| 16 | **amsmath サポート** — align/gather/multline 等の複数行数式環境 | `align` 環境の揃え位置が正しく整列 | #14 | 高 | implementer |
| 17 | **hyperref サポート** — リンク生成、PDF メタデータ、NavigationState、Link Annotation Plan | `\href` がクリック可能なリンクを生成、PDF プロパティに pdftitle 反映 | #11, #8 | 中 | implementer |
| 18 | **画像埋め込み** — PNG/JPEG/PDF 読み込み、GraphicsScene、XObject 配置 | `\includegraphics` で画像が PDF に埋め込まれる | #8 | 中 | implementer |
| 19 | **フロート配置** — figure/table 環境、配置アルゴリズム（htbp）、フロートキュー | フロート指定子に従い配置される | #6 | 中 | implementer |
| 20 | **Asset Bundle: 実装** — bundle 形式定義、manifest 検証、mmap 読み込み、OverlaySet 合成 | `--asset-bundle <path>` で TeX Live なしでコンパイル可能 | #9, #15 | 高 | implementer |

### Tier 4: 差分コンパイル・並列化・開発者ツール強化

| # | タスク | 受入基準 | 依存 | 優先度 | ロール |
|---|---|---|---|---|---|
| 21 | **依存グラフ構築** — ファイル/マクロ/参照間の依存記録、永続化 | 10 ファイル文書の全依存関係が正しく記録・復元される | #11 | 中 | implementer |
| 22 | **変更検知 + キャッシュ管理** — ハッシュベース変更検知、中間結果キャッシュ、LRU 管理 | キャッシュ再利用でフルコンパイルの 10% 以下の時間 | #21 | 中 | implementer |
| 23 | **部分再コンパイル** — 差分コンパイル実行、キャッシュマージ、参照再計算 | 1 段落変更の差分コンパイルがフルと同一 PDF を生成 | #22 | 中 | implementer |
| 24 | **パイプライン並列化** — Snapshot/CommitBarrier、ステージ間バッファリング、スレッドプール | `--jobs=4` が `--jobs=1` より高速、出力同一 | #23 | 中 | implementer |
| 25 | **tikz/pgf サポート** — 描画コマンドパース、GraphicsScene 生成、PDF 変換 | 基本図形の幾何関係が pdfLaTeX と一致 | #18 | 低 | implementer |
| 26 | **LSP 強化** — codeAction（修正候補）、hover 情報、completion/definition の改善 | `\begin{equation}` 未閉じでの修正候補提示 | #11 | 低 | implementer |
| 27 | **SyncTeX** — Source Span 記録、fragment-based trace 生成、forward/inverse search | ソース行から PDF 座標、PDF 座標からソース範囲が取得可能 | #6, #8 | 低 | implementer |
| 28 | **fontspec サポート** — OpenType フィーチャ、フォント fallback チェーン | `\setmainfont{Noto Serif}` で正しく組版される | #9, #20 | 低 | implementer |

## 4. 実行戦略

### 並行実行可能グループ

- **Group A**（並行可能）: #1, #9 — パーサー拡張とOpenType読み込みは独立
- **Group B**（#1 完了後、並行可能）: #2, #3, #4, #5 — 条件分岐/レジスタ/エラー回復/box モデルは概ね独立
- **Group C**（#5 完了後、並行可能）: #6, #7, #14 — ページ分割/ハイフネーション/数式は独立
- **Group D**（#8 は #9 とは独立）: #8 — フォント埋め込みは TFM reader 上に構築可能

### クリティカルパス

```
#1 (macro) → #5 (box model) → #6 (page split) → #10 (LaTeX kernel) → #11 (cross-ref) → #21 (dep graph) → #22 (cache) → #23 (incremental) → #24 (parallel)
```

### 現在ランで実行推奨

現在ランで **Tier 1 の #1-#9**（9 タスク）を並行実行することを推奨。これにより:
- コアパイプライン（parse → typeset → PDF）が実用レベルに近づく
- Tier 2 以降の全タスクのブロッカーが解消される
- 推定: 各タスク 1 エージェント × 9 並行 ≈ 1 セッション

Tier 2 以降は次セッション以降に段階的に実施。

## 5. 妥当性判定

- **結果**: 承認（条件付き）
- **条件**: Tier 1 完了後に再計画を実施し、Tier 2 のスコープと優先順位を確認する
- **リスク**: TeX の互換性は long tail。Must 要件の完全充足には数十セッションの反復が必要と推定
