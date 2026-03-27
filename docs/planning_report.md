# Ferritex 完成計画レポート

## 1. 現状サマリー

### 実装済み（docs と実装が概ね揃っている領域）

| 領域 | 実装レベル | 概要 |
|---|---|---|
| CLI / Runtime Options | 実用 | compile/watch/preview/lsp の 4 サブコマンド、`--jobs` / `--asset-bundle` / `--reproducible` / `--synctex` / `--trace-font-tasks` などの共通 runtime option 正規化 |
| Parser / Macro | 中〜高 | `\def` / `\gdef` / `\edef`、` \expandafter`、`\noexpand`、`\csname`、`\newcommand`、`\newenvironment`、`\begingroup` / `\endgroup`、`\if` / `\ifx` / `\ifcat` / `\ifnum` / `\ifdim` / `\ifcase`、`\numexpr` / `\dimexpr`、32768 register family、recoverable parse diagnostics |
| File Input / Package Loading | 中 | `\input` / `\include` / `\InputIfFileExists`、current-file/project/overlay/bundle fallback、`.sty` 読み込み、`\RequirePackage` 再帰、class/package registry |
| Typesetting | 中 | Knuth-Plass line breaking、hyphenation、hbox/vbox、page breaking、float queue、inline/display math、equation/align 系、TOC/LOF/LOT/index の multi-pass 解決 |
| PDF / Graphics | 中 | PDF 1.4 出力、TrueType subset embedding + ToUnicode、hyperref link annotation / named destination / metadata、PNG/JPEG `\includegraphics` 埋め込み |
| Font | 中 | TFM / OpenType 読み込み、fontspec named-font resolution、project/overlay/bundle/host catalog fallback、`--reproducible` で host fallback 無効化 |
| Incremental / Cache | 初期 | source expansion から dependency graph を収集し、output dir 配下の persistent cache metadata へ保存。入力と PDF hash が不変なら warm compile を再利用し、cache metadata / artifact 破損時は full compile fallback |
| Bibliography | 中 | `.bbl` 読み込み、citation 解決、stale `.bbl` warning、reference list 組版 |
| Preview / LSP / Watch | 中 | loopback preview transport、watch の依存パス再同期、LSP diagnostics/completion/definition/hover/codeAction |
| SyncTeX | 初期 | `--synctex` で `.synctex` sidecar を生成し、line-based trace で forward / inverse search を提供 |

### 主要な残ギャップ

| ID | 要件領域 | 深刻度 | 主な不足 |
|---|---|---|---|
| A | Incremental compilation (REQ-FUNC-027-030) | 高 | dependency graph / persistent cache / cache corruption fallback の最小縦配線は実装済み。変更範囲に応じた selective invalidation、partial recompile merge、性能適合度は未実装 |
| B | Parallel pipeline (REQ-FUNC-031-033) | 高 | `CommitBarrier`、document partition planning、deterministic merge、partition benchmark を満たす本体並列化が未実装 |
| C | tikz/pgf (REQ-FUNC-023) | 高 | `tikzpicture` / `\draw` / style inheritance / transform / clip / arrow を扱う graphics scene が未実装 |
| D | Asset bundle runtime (REQ-FUNC-046) | 中 | manifest validation はあるが、versioned bundle の本実装・mmap 読み込み・built-in bundle 配布契約は未完 |
| E | SyncTeX fidelity (REQ-FUNC-041) | 中 | 現在は expanded source line ベースの trace で、`PlacedNode.sourceSpan` に基づく fragment 精度までは未到達 |
| F | Full LaTeX compatibility | 中 | long-tail package behavior、TikZ 周辺、より厳密な layout parity は継続課題 |

## 2. 完全完成の実現可能性

Must 要件のかなりの部分は既に動作するが、docs 全体の「完成」を名乗るには Incremental / Parallel / TikZ / high-fidelity SyncTeX がまだ不足している。したがって今の到達点は「shell ではなく、主要な論文系ワークロードを処理できる prototype」。以後の計画は “全体骨格の整備” ではなく “残る高難度領域の収束” に切り替えるべき段階に入っている。

## 3. タスク分解（即座実行可能なスライス）

残タスクは「未着手の基盤」と「精度向上」に分ける。

### Wave 1: Incremental / Cache

| # | タスク | 受入基準 |
|---|---|---|
| 1 | Dependency graph / change detection | 最小実装済み。source expansion から依存ファイル集合と include edge を記録し、変更ファイル集合から `RecompilationScope` を判定できる |
| 2 | Persistent cache / integrity check | 最小実装済み。warm compile で再利用が走り、cache metadata / cached PDF の破損時は full compile fallback できる |
| 3 | Partial recompile merge | 小変更で full compile と同一 PDF を保ったまま処理時間を短縮できる |

### Wave 2: Deterministic Parallelization

| # | タスク | 受入基準 |
|---|---|---|
| 4 | `CommitBarrier` / stage payload | parallel stage の commit が deterministic order で適用される |
| 5 | Document partition planner | chapter / section 単位の stable `partitionId` を発行できる |
| 6 | Partition merge benchmark | `FTX-PARTITION-BENCH-001` の `--jobs=4` が `--jobs=1` を上回る |

### Wave 3: Graphics / Trace Fidelity

| # | タスク | 受入基準 |
|---|---|---|
| 7 | tikz/pgf core scene | `FTX-CORPUS-TIKZ-001/basic-shapes` を通せる |
| 8 | Precise SyncTeX | `PlacedNode.sourceSpan` ベースの fragment trace へ置き換える |
| 9 | Bundle runtime hardening | built-in bundle / mmap / manifest versioning を docs 契約どおりに固める |

## 4. 実行戦略

- まず Wave 1 を優先する。理由は `REQ-FUNC-027-033` が watch / LSP / benchmark の全てに波及する基盤だから
- Wave 2 は Wave 1 の結果に依存するが、`traceFontTasks` と既存 parallel font path を足場にできる
- Wave 3 は単体でも進められるが、TikZ と precise SyncTeX は型の入れ替え量が大きいため、incremental / parallel の境界が固まってから入る方が安全

## 5. 妥当性判定

- **結果**: 継続実装フェーズ
- **判断**: もはや「骨格を作る段階」ではなく、「残る高難度 frontier を 1 つずつ潰す段階」
- **直近の推奨**: incremental/cache を先に完了させ、その後に commit barrier / partition parallelism へ進む
