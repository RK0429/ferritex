# Ferritex 要件定義書

## メタ情報


| 項目    | 内容              |
| ----- | --------------- |
| バージョン | 0.1.22          |
| 最終更新日 | 2026-03-17      |
| ステータス | ドラフト            |
| 作成者   | Claude Opus 4.6 |
| レビュー者 | —               |


## 1. プロジェクト概要

### 1.1 背景・動機

既存の TeX エンジン（pdfLaTeX, XeTeX, LuaTeX 等）はコンパイル速度が遅く、大規模文書（100ページ以上の論文）の執筆・編集サイクルにおいてボトルネックとなっている。また、差分コンパイルや LSP 連携、リアルタイムプレビューなど、モダンなソフトウェア開発で一般的な機能が TeX エコシステムには欠けている。

### 1.2 目的

- 既存 TeX エンジンを大幅に上回るコンパイル速度を実現する（pdfLaTeX 比 100 倍）
- Rust ネイティブランタイムと事前インデックス化された Ferritex Asset Bundle により、実行時の TeX 処理を外部 TeX ランタイム非依存で完結させる
- 差分コンパイル・LSP 連携・リアルタイムプレビュー・並列処理など、既存エンジンにない機能を提供する

### 1.3 対象ユーザー


| ユーザー種別        | 説明                                  | 優先度 |
| ------------- | ----------------------------------- | --- |
| LaTeX ヘビーユーザー | 研究者、論文執筆者。100ページ以上の文書を頻繁に編集・コンパイルする | 主要  |
| 開発者自身         | ドッグフーディングによる品質向上                    | 副次  |


### 1.4 スコープ

**スコープ内**:

- LaTeX 文書をそのまま処理できるレベルの TeX/LaTeX 互換性
- 主要パッケージ（amsmath, graphicx, hyperref, tikz 等）のサポート
- PDF 直接出力
- OpenType / TFM フォントサポート
- Ferritex Asset Bundle によるクラス・パッケージ・フォント資産の事前インデックス化
- 差分コンパイル（インクリメンタルコンパイル）
- パイプライン並列化による高速化
- LSP サーバー（構文診断・補完）
- リアルタイムプレビュー
- CLI インターフェース
- クロスプラットフォーム対応（Linux / macOS / Windows）

**スコープ外**:

- DVI 出力
- エディタプラグインの実装（LSP 準拠で対応）
- Ferritex 自身による CTAN / TeX Live パッケージマネージャーの実装
- 実行時の kpathsea / TeX Live 依存（資産の取り込み元としての利用は許容）

### 1.5 制約条件


| 制約       | 内容                                       |
| -------- | ---------------------------------------- |
| 実装言語     | Rust                                     |
| 出力形式     | PDF 直接出力のみ                               |
| パッケージ互換性 | 主要パッケージ（amsmath, graphicx, hyperref, tikz 等）の動作が必須 |
| 実行時資産供給  | Ferritex Asset Bundle を使用し、TeX Live / kpathsea を実行時依存にしない |
| プラットフォーム | Linux / macOS / Windows                  |


### 1.6 成功基準


| 基準               | 現行（基準）           | 目標                       |
| ---------------- | ---------------- | ------------------------ |
| FTX-BENCH-001 のフルコンパイル時間 | 同一入力・同一マシンでの pdfLaTeX baseline | 中央値 1.0 秒未満 |
| FTX-BENCH-001 の相対速度    | 1x               | pdfLaTeX 比 100x 以上                     |
| LaTeX 互換性        | —                | 主要パッケージを含む標準的な論文がコンパイル可能 |

※ 絶対速度と相対速度は同じ benchmark profile `FTX-BENCH-001` で判定し、詳細条件は `REQ-NF-001` / `REQ-NF-002` に定義する。


## 2. 用語集


| 用語               | 定義                                                     |
| ---------------- | ------------------------------------------------------ |
| カテゴリコード（catcode） | TeX が各文字に割り当てる種別コード。字句解析の挙動を制御する                       |
| トークン             | TeX の字句解析が生成する処理の最小単位。コントロールシーケンストークンと文字トークンの2種類がある    |
| コントロールシーケンス      | `\` で始まる TeX コマンド（例: `\section`, `\newcommand`）        |
| マクロ展開            | TeX のマクロ定義に基づき、トークン列を置換・変換する処理                         |
| タイプセッティング        | テキストとコマンドをレイアウト（行分割・ページ分割・配置）する処理                      |
| ボックス             | TeX の組版における基本的なレイアウト単位。水平ボックス（hbox）と垂直ボックス（vbox）がある    |
| グルー              | TeX の伸縮可能なスペース。自然長・伸び量・縮み量を持つ                          |
| ペナルティ            | 行分割・ページ分割の位置を制御する数値。高いほど分割されにくい                        |
| フロート             | 図表など、テキストの流れから独立して配置されるオブジェクト                          |
| TFM              | TeX Font Metric。TeX 固有のフォントメトリクスバイナリ形式                 |
| OpenType         | 現代的なフォント形式（OTF/TTF）。高度なタイポグラフィ機能を含む                    |
| 差分コンパイル          | ソースの変更箇所のみを再処理し、フルコンパイルを回避する手法                         |
| 依存グラフ            | ファイル・マクロ・参照間の依存関係を表すグラフ構造                              |
| LSP              | Language Server Protocol。エディタとの間で構文診断・補完等の機能を提供するプロトコル |
| SyncTeX          | TeX ソースの位置と PDF 出力の位置を双方向に対応付けるデータ形式                   |
| kpathsea         | TeX のファイル探索ライブラリ。`TEXMF` ツリーからファイルを検索する                |
| CTAN             | Comprehensive TeX Archive Network。TeX 関連パッケージのリポジトリ    |
| Ferritex Asset Bundle | Ferritex が実行時に参照するクラス・パッケージ・フォント資産のスナップショット。事前インデックス化され memory-mapped に読み込まれる |
| Asset Index      | Ferritex Asset Bundle 内の資産を論理名から O(1) 近傍で引ける索引構造        |
| Host Font Catalog | platform font discovery API（fontconfig / CoreText / DirectWrite）から事前収集したホストフォント索引。Ferritex では host-local overlay として扱う |
| Configured Overlay Root | 起動時設定で明示された読み取り専用の追加資産ディレクトリ。project root 外に置かれた `.tex` / `.sty` / クラス / フォント資産を allowlist として解決面へ追加する |
| Execution Policy | `compile` / `watch` / `lsp` など全 entry point で共有される実行制約の集合。shell-escape 可否、パス許可境界、タイムアウト、出力上限に加え、preview 配信専用の `Preview Publication Policy` を含む |
| Runtime Options | compile / watch / LSP の入口固有指定を正規化した共通実行記述。`primaryInput`、`artifactRoot`、`jobname`、`parallelism`、`reuseCache`、`assetBundleRef`、`interactionMode`、`synctex`、`shellEscapeAllowed` を保持し `ExecutionPolicy` 構築に使う |
| Asset Bundle Reference | Ferritex Asset Bundle を参照するための値。ファイルパスまたは組み込みバンドル識別子で表す |
| Artifact Kind | Output Artifact Registry が記録する補助ファイル種別。`.aux`、`.toc`、`.lof`、`.lot`、`.bbl`、`.synctex` など trusted readback 対象の論理分類を表す |
| Artifact Producer Kind | Output Artifact Registry が記録する生成主体種別。Ferritex 本体が生成した成果物か、Ferritex が制御した外部ツールが生成した成果物かを区別する |
| Compilation Job | 1 回の `compile` / `watch` / LSP 再コンパイル要求に対応する単位。最大 3 パスまでの `Compilation Session` を束ね、参照状態と出力 artifact provenance を pass 間で保持する |
| Compilation Session | `Compilation Job` 内の 1 パスで共有される可変 TeX 状態。カテゴリコード、レジスタ、スコープ、コマンド/環境レジストリを保持する |
| Compilation Snapshot | 並列ステージ境界で共有する読み取り専用のコンパイル状態スナップショット。マクロ・レジスタ・文書状態の確定済み部分を含み、並列タスクから破壊的更新しない |
| Commit Barrier | 並列ステージの結果を決定的な順序で `Compilation Job` へ反映する同期点。可変状態の commit はここでのみ行う |
| Output Artifact Registry | Ferritex または Ferritex が制御した外部ツール実行で生成された readback 対象補助ファイルの正規化パス、主入力、artifact kind、jobname、生成パス番号、生成者種別、生成パス、コンテンツハッシュを記録する active-job 限定の in-memory 台帳。trusted readback の same-job 判定キーは主入力と jobname であり、生成パス番号は監査属性として保持する。job 完了または process restart 時に無効化し、append-only manifest は監査専用とする |
| Job Context | `Compilation Job` 内の現在パスを識別する jobname・主入力・現在パス番号の組。same-job readback の一致判定は jobname と主入力で行い、現在パス番号は出力命名・順序・診断のために使う |
| Bbl Snapshot | `.bbl` から取り込んだ引用・参考文献情報の正規化スナップショット |
| Definition Provenance | マクロ・ラベル・参考文献エントリの定義元を示すファイル名・行番号・列番号・由来種別の組。定義ジャンプと診断に用いる |
| Navigation State | hyperref とセクショニングが蓄積する PDF ナビゲーション用状態。PDF metadata draft、しおり候補、named destination、既定リンク装飾設定を保持する |
| Link Annotation Plan | 配置済みリンク 1 件分の PDF 注釈化計画。リンク矩形、リンク先、装飾設定を保持し、PDF 生成段階で Annotation に射影される |
| Link Style | hyperref の `colorlinks` や枠線設定から正規化したリンク装飾値。テキスト色と注釈境界線の描画規則を保持する |
| Source Span | 組版済みノードに対応付くソース範囲。開始/終了の SourceLocation を持ち、SyncTeX と診断の由来追跡に用いる |
| Placed Node | `PageBox` 内で確定した配置矩形と Source Span を伴う組版ノード。PDF 射影と SyncTeX の共通入力 |
| Placed Destination | internal named destination の配置済みアンカー。destination 名とページ内矩形を持ち、内部リンク・しおり解決に用いる |
| Table Of Contents State | `.toc` / `.lof` / `.lot` 由来の目次・図表一覧エントリを保持する job-scope 状態 |
| Index State | `\index` で収集した索引語とソートキー、対応ページを保持し、makeindex 互換整列へ渡す job-scope 状態 |
| File Access Gate | `\input` / `\include` / `\openin` / `\openout` / engine-temp / engine-readback を単一の Execution Policy で裁く共通ゲート |
| グラフィックシーン | tikz/graphicx の描画結果を PDF 非依存のベクター・PDF グラフィック・ラスタ・テキスト要素へ正規化した中間表現 |
| SyncTeX Trace Fragment | 1 つの Source Span と 1 つのページ内矩形を対応付ける SyncTeX の最小断片。1 つの Source Span に複数 fragment が対応してよい |
| Pending Change Queue | watch 実行中にコンパイル中の追加変更を集約する待ち行列。連続変更を coalesce し、コンパイル完了後の再トリガーに使う |
| Recompile Scheduler | `FileWatcher` からの変更イベントと Pending Change Queue を受け取り、同時実行を避けながら差分コンパイルを順序制御する調停役 |
| Preview Publication Policy | preview 配信専用の制約。loopback bind 限定、active job の最新 PDF のみ publish、session owner 一致の必須化、process restart または preview target 変更時の session 再発行規約を保持する |
| Preview Target | preview session / revision が紐づく対象文書の識別子。workspace root、primaryInput、jobname の組で表す |
| Preview Session | sessionId ごとの preview 状態。`Preview Target` を owner として保持し、同一 target かつ同一 process の間だけ再利用される。閲覧位置を保持し、`Preview Transport` から受ける view-state 更新を反映する |
| Preview View State | プレビューアの現在ページ、ページ内オフセット、ズーム倍率など、更新後も保持すべき閲覧位置情報 |
| Preview Revision | active job が生成した PDF の改訂。`Preview Target` に紐づく revision 番号と pageCount を保持する |
| Preview Session Service | `Preview Session` の発行・再発行・失効を管理し、`POST /preview/session` から受けた `Preview Target` を同一 process / 同一 target の既存 session に解決する。`Execution Policy.previewPublication` に照らして許可された publish だけを `Preview Transport` へ委譲する調停役 |
| Preview Transport | loopback のみへ bind し、session bootstrap / document / events endpoint を提供する preview 配信契約。`POST /preview/session` への `sessionId` / `documentUrl` / `eventsUrl` 応答内容は `Preview Session Service` が決定し、`GET /preview/{sessionId}/document` で PDF 本体、`WS /preview/{sessionId}/events` で `Preview Revision` 更新通知と view-state 更新を扱う |
| Page Render Plan | 1 ページ分の `PageBox`、placed destination、リンク注釈計画、`GraphicsScene`、SyncTeX 用 source trace を束ねた PDF 射影入力 |
| Open Document Buffer | エディタが保持する未保存変更を含む最新のテキスト状態。LSP の診断・補完・定義ジャンプ・hover は保存済みファイルよりこれを優先して参照する |
| Stable Compile State | 最新の成功した `CommitBarrier` 完了時点で確定した `CompilationSession` / `DocumentState` の投影。worker-local な未 commit 状態や失敗 pass の部分結果を含まない |
| Live Analysis Snapshot | `Open Document Buffer` と Stable Compile State（command/environment registry、label/citation 状態など）を合成した LSP 用の解析スナップショット |
| Partition Kind | 文書パーティションの種別。`chapter` / `section` など、`DocumentPartitionPlanner` が work unit を分類するための論理タグ |
| Partition ID | `DocumentPartitionPlanner` が各文書パーティションへ安定に発行する識別子。`Commit Barrier` の total order の一部として使う |
| Partition Locator | 章/セクション境界を一意に示す論理位置。`entryFile`、見出しの source span、同一ファイル内での出現順を組にして表す |
| Citation Table | `.bbl` 由来の citation key と citation 表示文字列 / provenance を対応付ける索引。`\cite` 解決に使い、provenance は本文側 citation 表示の trace に使う |
| Bibliography Entry | 参考文献 1 件分の整形済みエントリ。表示文字列、citation key、由来情報を持ち、`\cite` の定義ジャンプはこの provenance を authority とする |
| FTX-ASSET-BUNDLE-001 | 互換性・性能評価で基準に使う versioned 公式 Asset Bundle。LaTeX カーネル、標準クラス、標準パッケージ、基準フォント資産を固定内容で含む |
| FTX-BENCH-001 | Ferritex の性能要件を判定する共通 benchmark profile。100 ページの学術論文テンプレート、`amsmath` + `hyperref` + `graphicx`、固定 Ferritex Asset Bundle、外部参考文献処理なし、tikz/pgf なし、4 コア以上の CPU、同一入力・同一マシンでの pdfLaTeX 比較を前提にした versioned 計測条件を指す |
| FTX-LSP-BENCH-001 | Ferritex の LSP 応答性能を判定する versioned benchmark profile。`FTX-BENCH-001` の入力文書と同一の 100 ページ学術論文テンプレートを LSP で開き、`FTX-BENCH-001` が規定する 4 コア以上の CPU を含むハードウェア条件を適用し、キャッシュと `Stable Compile State` が構築済みの warm 状態から、診断・補完・定義ジャンプの各操作を含む replayable LSP trace を再生する計測条件を指す |
| FTX-CORPUS-COMPAT-001 | pdfLaTeX 互換性を判定する versioned 回帰コーパス。article/report/book/letter の基準文書に加え、hyperref、フォント埋め込み、画像埋め込み、外部 PDF 埋め込み、参考文献、目次/しおりを含む 100 文書で構成し、`FTX-ASSET-BUNDLE-001` を前提に評価する。参考文献を含む文書には事前生成済みの `.bbl` ファイルを同梱し、`bibtex` / `biber` の実行を前提としない |
| FTX-CORPUS-COMPAT-001/layout-core | `FTX-CORPUS-COMPAT-001` のうち article/report/book/letter の baseline 文書群を束ねる stable subset ID。レイアウト互換の基準ケースに使う |
| FTX-CORPUS-COMPAT-001/layout-core/article | `FTX-CORPUS-COMPAT-001/layout-core` に含まれる article baseline 文書の stable case ID |
| FTX-CORPUS-COMPAT-001/navigation-features | `FTX-CORPUS-COMPAT-001` のうち hyperlink、named destination、しおり、PDF metadata を含む stable subset ID |
| FTX-CORPUS-COMPAT-001/embedded-assets | `FTX-CORPUS-COMPAT-001` のうち埋め込みフォント、画像埋め込み、外部 PDF 埋め込みを含む stable subset ID |
| FTX-CORPUS-TIKZ-001 | tikz/pgf 適合度を判定する固定回帰コーパス。基本図形、nested scope の style 継承、transform、clip、arrow、text node を含み、pdfLaTeX を参照出力とする |
| FTX-CORPUS-TIKZ-001/basic-shapes | `FTX-CORPUS-TIKZ-001` の基本図形ケースを束ねる stable subset ID |
| FTX-CORPUS-TIKZ-001/nested-style-transform-clip-arrow | `FTX-CORPUS-TIKZ-001` の style 継承、transform、clip、arrow を束ねる stable subset ID |
| MoSCoW           | 優先度分類法。Must / Should / Could / Won't の4段階              |


## 3. 機能要件

### 3.1 TeX パーサー・マクロエンジン

#### REQ-FUNC-001: TeX 字句解析

- **説明**: TeX ソースを読み込み、カテゴリコードに基づいてトークン列に変換する
- **入力**: TeX ソースファイル（UTF-8）
- **処理**:
  - 文字ごとにカテゴリコードを参照しトークン種別を決定
  - コントロールシーケンス、文字トークン、特殊文字の識別
  - `\catcode` 命令によるカテゴリコードの動的変更への対応
- **出力**: トークンストリーム
- **例外**: 不正な UTF-8 シーケンス検出時にエラーを報告し、該当バイトをスキップして処理を継続
- **受け入れ基準**:
  - Given 標準的なカテゴリコード設定の LaTeX 文書, When 字句解析を実行, Then pdfLaTeX と同一のトークン列が生成される
  - Given `\catcode` による動的なカテゴリコード変更を含む文書, When 字句解析を実行, Then 変更後のカテゴリコードが即座に反映される
- **優先度**: Must
- **出典**: ユーザー明示

#### REQ-FUNC-002: マクロ展開

- **説明**: `\def`, `\edef`, `\gdef`, `\newcommand`, `\renewcommand` 等で定義されたマクロを展開する
- **入力**: トークンストリーム
- **処理**:
  - マクロ定義のパターンマッチングと引数取得
  - ネストされたマクロの再帰的展開
  - `\expandafter`, `\noexpand` 等の展開制御プリミティブ
  - グルーピング（`{}`, `\begingroup`/`\endgroup`）によるスコープ管理
  - `ScopeStack` が group frame ごとの local macro / register / catcode 差分を保持し、group 終了時に frame 入口値へ巻き戻す
- **出力**: 展開済みトークンストリーム
- **例外**: 無限再帰検出時にエラーを報告し展開を中断（再帰深度上限: 設定可能、デフォルト 1000）
- **受け入れ基準**:
  - Given `\def\foo#1#2{#2#1}` が定義された文書, When `\foo{A}{B}` を展開, Then `BA` が得られる
  - Given `\def\foo{outer}{\begingroup\def\foo{inner}\foo\endgroup\foo}` を展開, When group を抜ける, Then 内側では `inner`、外側では `outer` が得られる
  - Given 再帰深度が上限を超えるマクロ, When 展開を実行, Then エラーメッセージと該当マクロのスタックトレースが出力される
- **優先度**: Must
- **出典**: ユーザー明示

#### REQ-FUNC-003: 条件分岐処理

- **説明**: TeX の条件分岐プリミティブ（`\if`, `\ifnum`, `\ifx`, `\ifcat`, `\ifdim`, `\ifcase` 等）および LaTeX の `\ifthenelse` を処理する
- **入力**: 条件分岐トークン列
- **処理**: 条件を評価し、真偽に応じたブランチのトークンを選択。偽ブランチのスキップ処理（ネストされた `\if`/`\fi` の正しい対応）
- **出力**: 選択されたブランチのトークンストリーム
- **受け入れ基準**:
  - Given `\ifnum\value{page}>10` を含む文書, When 条件評価, Then ページ番号に応じた正しいブランチが選択される
  - Given ネストされた `\if...\if...\fi\fi` 構造, When 偽ブランチをスキップ, Then ネスト対応が正しく処理される
- **優先度**: Must
- **出典**: ユーザー明示

#### REQ-FUNC-004: カウンタ・レジスタ管理

- **説明**: TeX のレジスタ（count, dimen, skip, muskip, toks, box）の割り当て・読み書きを管理する
- **入力**: レジスタ操作コマンド
- **処理**:
  - `\newcount`, `\countdef` 等によるレジスタ割り当て
  - `\the`, 代入、算術演算（`\advance`, `\multiply`, `\divide`）
  - e-TeX 拡張レジスタ（32768 個）のサポート
  - local 代入は current group frame に記録し、group 終了時に frame 入口値へ復元する。`\global` 指定だけが session-root へ反映される
- **出力**: レジスタ値の更新・読み出し結果
- **受け入れ基準**:
  - Given e-TeX 拡張レジスタを使用する文書, When レジスタ 256〜32767 番にアクセス, Then 正常に読み書きできる
  - Given `\count0=1 {\count0=2} \the\count0` を含む文書, When group を抜ける, Then `\count0` は `1` に戻る
- **優先度**: Must
- **出典**: ユーザー確認済み（LaTeX 互換に e-TeX 拡張が事実上必須）

#### REQ-FUNC-005: ファイル入力処理

- **説明**: `\input`, `\include`, `\InputIfFileExists` による外部ファイルの読み込みを処理する
- **入力**: ファイルパスを含むトークン
- **処理**:
  - 現在ファイル基準の相対パス、プロジェクトルート、設定済み read-only overlay roots、Ferritex Asset Bundle の順に解決
  - `\include` のガード処理（`.aux` ファイルの分離）
  - ファイルのネスト深度管理
- **出力**: 読み込んだファイルのトークンストリームを現在のストリームに挿入
- **例外**: ファイル未発見時にエラーを報告（`\InputIfFileExists` の場合は偽ブランチを実行）
- **受け入れ基準**:
  - Given マルチファイル構成の文書, When `\input{chapters/intro}` を実行, Then 現在ファイル相対とプロジェクトルート相対の優先順位に従って正しいファイルが読み込まれる
  - Given 設定済み read-only overlay root に `shared/macros.tex` がある環境, When `\InputIfFileExists{shared/macros.tex}` を実行, Then overlay root から対象ファイルが解決される
  - Given Ferritex Asset Bundle に共有マクロファイルが含まれ、TeX Live がインストールされていない環境, When `\InputIfFileExists{bundle/macros}` を実行, Then Asset Index から対象ファイルが解決される
- **優先度**: Must
- **出典**: ユーザー明示

#### REQ-FUNC-006: エラー回復

- **説明**: 構文エラー時にリカバリを行い、可能な限り処理を継続して追加のエラーを検出する
- **入力**: エラーが発生したトークンストリーム
- **処理**:
  - エラー箇所の特定（ファイル名、行番号、列番号）
  - パニックモードリカバリ（同期点までトークンをスキップ）
  - エラーメッセージの生成（pdfLaTeX 互換の形式に加え、コンテキスト情報を付加）
- **出力**: 診断メッセージとリカバリ後のトークンストリーム
- **受け入れ基準**:
  - Given 複数のエラーを含む文書, When コンパイル, Then 最初のエラーだけでなく後続のエラーも可能な限り報告される
- **優先度**: Must
- **出典**: ユーザー明示

### 3.2 タイプセッティングエンジン

#### REQ-FUNC-007: 行分割アルゴリズム

- **説明**: Knuth-Plass 行分割アルゴリズムにより、段落内の最適な改行位置を決定する
- **入力**: 段落の水平リスト（ボックス、グルー、ペナルティ）
- **処理**:
  - 最適改行点の探索（総デメリット最小化）
  - ハイフネーション（TeX のパターンベースハイフネーション）
  - `\looseness`, `\tolerance`, `\emergencystretch` の考慮
- **出力**: 行分割された水平ボックスのリスト
- **受け入れ基準**:
  - Given pdfLaTeX と同一の入力段落, When 行分割を実行, Then 同一の改行位置が選択される（パラメータが同一の場合）
- **優先度**: Must
- **出典**: ユーザー明示

#### REQ-FUNC-008: ページ分割

- **説明**: 垂直リストから最適なページ区切り位置を決定する
- **入力**: 垂直リスト（行ボックス、グルー、ペナルティ、挿入物）
- **処理**:
  - ページ充填度とペナルティに基づく分割点決定
  - `\pagebreak`, `\newpage`, `\clearpage` の処理
  - 脚注・フロート挿入の考慮
- **出力**: ページ単位に分割された垂直ボックスのリスト
- **受け入れ基準**:
  - Given フロートと脚注を含む文書, When ページ分割を実行, Then フロートと脚注が適切なページに配置される
- **優先度**: Must
- **出典**: ユーザー明示

#### REQ-FUNC-009: 数式組版

- **説明**: インライン数式（`$...$`）およびディスプレイ数式（`\[...\]`, `equation` 等）の組版を行う
- **入力**: 数式モードのトークン列
- **処理**:
  - 数式リスト（Ord, Op, Bin, Rel, Open, Close, Punct, Inner）の構築
  - 上付き・下付き、分数、根号、アクセントの配置
  - 数式フォントファミリの選択とサイズ変更（`\displaystyle` 等）
  - アトム間スペーシング
- **出力**: 数式ボックス
- **受け入れ基準**:
  - Given amsmath の `align` 環境を含む文書, When 数式組版を実行, Then pdfLaTeX と同等のレイアウトで出力される
- **優先度**: Must
- **出典**: ユーザー明示

#### REQ-FUNC-010: フロート配置

- **説明**: `figure`, `table` 環境等のフロートオブジェクトの配置を決定する
- **入力**: フロート指定子（`[htbp!]`）付きの挿入要求
- **処理**:
  - TeX のフロート配置アルゴリズム（指定子の優先順位に従い配置位置を決定）
  - フロートキュー管理（配置不可時の繰り延べ）
  - `\clearpage` によるフロート強制出力
- **出力**: 配置位置が確定したフロートボックス
- **受け入れ基準**:
  - Given `[htbp]` 指定のフロートが複数ある文書, When フロート配置を実行, Then 指定子の優先順位に従い配置される
- **優先度**: Must
- **出典**: ユーザー明示

#### REQ-FUNC-011: 相互参照解決

- **説明**: `\label`, `\ref`, `\pageref` 等の文書内相互参照を解決する
- **入力**: 相互参照コマンドを含む文書
- **処理**:
  - `.aux` ファイルへのラベル情報書き出し・読み込み（`--output-dir` 指定時は、Output Artifact Registry に記録された current Compilation Job の `jobname` と主入力に整合する readback 対象 `.aux` のみを正規化済み output root 配下から再読込し、生成パス番号は監査属性として扱う）
  - マルチパス処理（同一 Compilation Job 内で pass ごとに新しい Compilation Session を作成し、job-scope の参照状態と Output Artifact Registry を引き継ぎながら、参照解決が安定するまで繰り返し、最大3パス）
  - 未解決参照の検出・警告
- **出力**: 解決済みのラベル・ページ参照テキスト
- **例外**: 未定義ラベル参照時に `??` を出力し警告を表示
- **受け入れ基準**:
  - Given `\label` と `\ref` を含む文書, When コンパイルを実行, Then 最大3パスで全参照が解決される
  - Given 未定義の `\ref{unknown}`, When コンパイルを実行, Then `??` が出力され警告が表示される
- **優先度**: Must
- **出典**: ユーザー明示
- **関連要件**: REQ-FUNC-024

#### REQ-FUNC-012: 目次・索引生成

- **説明**: `\tableofcontents`, `\listoffigures`, `\listoftables`, `\makeindex` 等を処理する
- **入力**: 目次・索引生成コマンドを含む文書
- **処理**:
  - `.toc`, `.lof`, `.lot` ファイルの書き出し・読み込み（`--output-dir` 指定時は、Output Artifact Registry に記録された current Compilation Job の `jobname` と主入力に整合する readback 対象ファイルのみを正規化済み output root 配下から再読込し、生成パス番号は監査属性として扱う）
  - セクション番号・ページ番号の収集結果を Table Of Contents State として保持し、目次・図表一覧ごとに整形
  - 索引エントリを Index State に収集し、makeindex 互換の順序でソート・整形
  - Table Of Contents State / Index State を専用の組版サービスで box tree へ射影し、`\tableofcontents` / `\listoffigures` / `\listoftables` / `\printindex` の出力に再利用
- **出力**: 目次・索引のボックスツリー
- **受け入れ基準**:
  - Given 章・節構造を持つ文書, When コンパイル, Then 正しいセクション番号とページ番号を含む目次が生成される
- **優先度**: Must
- **出典**: ユーザー明示

### 3.3 PDF 生成

#### REQ-FUNC-013: PDF ページストリーム出力

- **説明**: タイプセッティング結果を PDF コンテンツストリームに変換する
- **入力**: ページ単位の Page Render Plan（配置済みノードと source trace、placed destination、リンク注釈計画、グラフィックシーンの組）
- **処理**:
  - テキスト描画オペレータ（`BT`, `ET`, `Tf`, `Tj`, `TJ`）の生成
  - グラフィック描画オペレータ（罫線、図形）の生成
  - カラー指定（RGB, CMYK, グレースケール）
  - 配置済みノードの矩形と placed destination を使って、内部リンク destination と SyncTeX の座標解決に必要なページ座標系を確定
  - ページ単位の Page Render Plan を共通 PDF レンダリングパイプラインへ射影し、コンテンツストリーム、リンク Annotation、リソース辞書へ一貫して変換
  - PDF オブジェクト構造（ページツリー、リソース辞書）の構築
- **出力**: ISO 32000 準拠の妥当な PDF ファイル
- **受け入れ基準**:
  - Given 100ページの文書, When PDF 生成を実行, Then PDF 構造バリデータで syntax / xref / object structure error がゼロ
- **補足**: PDF/A 適合は本要件の判定対象外とし、必要になった時点で別要件として定義する
- **優先度**: Must
- **出典**: ユーザー明示

#### REQ-FUNC-014: フォント埋め込み

- **説明**: 使用フォントを PDF にサブセット埋め込みする
- **入力**: 使用グリフ情報とフォントファイル
- **処理**:
  - 使用グリフの収集とサブセット化
  - CIDFont / TrueType / Type1 フォントの PDF 埋め込み形式への変換
  - ToUnicode CMap の生成（テキスト検索対応）
- **出力**: フォントが埋め込まれた PDF
- **受け入れ基準**:
  - Given OpenType フォントを使用する文書, When PDF を生成, Then 使用グリフのみがサブセット埋め込みされ、PDF 上でテキスト検索が可能
- **優先度**: Must
- **出典**: ユーザー明示

#### REQ-FUNC-015: ハイパーリンク・しおり

- **説明**: PDF 内リンクとアウトライン（しおり）を生成する
- **入力**: hyperref コマンド・セクション構造
- **処理**:
  - 内部リンク（`\ref`, `\cite` のリンク化）のアノテーション生成と、placed destination に基づく named destination 解決
  - 外部リンク（`\href`, `\url`）のアノテーション生成
  - 配置済みのリンク領域を Link Annotation Plan（矩形、内部 destination または外部 URI、Link Style）へ正規化
  - Navigation State に蓄積された named destination と PDF metadata draft、および Page Render Plan が保持する placed destination を参照し、PDF アウトライン（しおり）のセクション構造を生成
- **出力**: リンクアノテーションとアウトラインを含む PDF
- **受け入れ基準**:
  - Given セクション構造を持つ文書, When PDF を生成, Then PDF ビューアのしおりパネルにセクション階層が表示される
- **優先度**: Must
- **出典**: ユーザー明示
- **関連要件**: REQ-FUNC-022

#### REQ-FUNC-016: 画像埋め込み

- **説明**: PNG, JPEG, PDF 形式の画像を文書に埋め込む
- **入力**: `\includegraphics` コマンドと画像ファイルパス
- **処理**:
  - 画像フォーマットの判別と読み込み
  - スケーリング・クリッピングの適用
  - `graphicx` の指定をグラフィックシーン上の外部グラフィックノードへ正規化し、PNG/JPEG はラスタ画像、PDF はベクターグラフィックとして保持
  - PDF 内への画像 XObject または imported PDF Form XObject の配置
- **出力**: 外部グラフィックを含むページコンテンツストリーム
- **例外**: サポート外形式・破損ファイルの場合はエラーを報告し、プレースホルダーボックスを配置
- **受け入れ基準**:
  - Given `\includegraphics[width=0.8\textwidth]{fig.png}` を含む文書, When コンパイル, Then 指定幅にスケーリングされた画像が PDF に埋め込まれる
  - Given `\includegraphics{diagram.pdf}` を含む文書, When コンパイル, Then PDF のベクター性を保持したまま配置される
- **優先度**: Must
- **出典**: ユーザー明示

### 3.4 フォント管理

#### REQ-FUNC-017: OpenType フォント読み込み

- **説明**: OTF/TTF フォントファイルを読み込み、メトリクスとグリフ情報を取得する
- **入力**: フォントファイルパス
- **処理**:
  - OpenType テーブル（`cmap`, `head`, `hhea`, `hmtx`, `GPOS`, `GSUB` 等）のパース
  - カーニング・リガチャ情報の抽出
  - Unicode → グリフ ID マッピング
- **出力**: フォントメトリクス・グリフデータ
- **受け入れ基準**:
  - Given fontspec でフォントを指定した文書, When コンパイル, Then カーニングとリガチャが正しく適用された出力が得られる
- **優先度**: Must
- **出典**: ユーザー明示

#### REQ-FUNC-018: TFM フォント読み込み

- **説明**: TeX Font Metric（`.tfm`）ファイルを読み込み、メトリクス情報を取得する
- **入力**: TFM ファイルパス
- **処理**: TFM バイナリ形式のパースと文字幅・高さ・深さ・イタリック補正・リガチャ・カーニング情報の抽出
- **出力**: フォントメトリクスデータ
- **受け入れ基準**:
  - Given Computer Modern フォント（`cmr10.tfm`）, When 読み込み, Then pdfLaTeX と同一のメトリクス値が取得される
- **優先度**: Must
- **出典**: ユーザー明示

#### REQ-FUNC-019: フォント解決

- **説明**: 指定されたフォント名またはフォント識別子から、組版に使用するフォント資産を高速に解決する
- **入力**: フォント名、ファミリ名、スタイル指定、ファイル名
- **処理**:
  - OverlaySet を通じて project-local / configured read-only overlay roots、Ferritex Asset Bundle、Host Font Catalog overlay fallback を単一の解決面として扱う
  - Asset Index から TeX フォント資産（TFM, map, OpenType snapshot）を解決し、host-local overlay ではコンパイルごとのフルスキャンを禁止する
  - フォントマップファイル、PostScript 名、family/style の対応付けを行う
  - Host Font Catalog overlay は利便性のための fallback モードとし、project-local / configured read-only overlay roots と Asset Bundle に一致候補がない場合、または明示的に host-local 解決を要求した場合にのみ参照する。host-local font を直接解決した結果は REQ-NF-008 のバイト同一保証対象外とする
- **出力**: フォント資産ハンドル（bundle asset id、overlay asset handle、またはキャッシュ済みファイルハンドル）
- **例外**: フォント未発見時にエラーを報告し、明示的なフォールバックチェーンが設定されている場合のみ代替フォントを使用
- **受け入れ基準**:
  - Given Ferritex Asset Bundle のみが導入された環境, When `cmr10` を解決, Then Asset Index から対応する TFM 資産が返される
  - Given Asset Bundle と Host Font Catalog の両方に同名フォントが存在する環境, When 通常のフォント解決を実行, Then project-local / configured read-only overlay roots または Asset Bundle 側の候補が host-local 候補より優先される
  - Given Host Font Catalog に `Noto Serif` が登録済み, When `\setmainfont{Noto Serif}` を解決, Then OS ディレクトリ全走査なしで対象フォントが選択される
- **優先度**: Must
- **出典**: ユーザー明示

### 3.5 パッケージ互換レイヤー

#### REQ-FUNC-020: LaTeX カーネル互換

- **説明**: LaTeX2e カーネルの基本機構を実装し、標準的な LaTeX 文書を処理可能にする
- **入力**: `\documentclass`, `\usepackage`, `\begin{document}` 等を含む LaTeX ソース
- **処理**:
  - Asset Bundle 上のクラス・カーネル資産を解決し、コマンド/環境レジストリへ登録
  - ドキュメントクラス（`article`, `report`, `book`, `letter` 等）の読み込み・適用
  - パッケージ読み込み機構（オプション処理、依存解決、読み込み順序管理）
  - 環境定義（`\newenvironment`, `\renewenvironment`）
  - セクショニングコマンド（`\chapter`, `\section`, `\subsection` 等）
  - リスト環境（`itemize`, `enumerate`, `description`）
  - クロスリファレンス機構（`\label`, `\ref`, `\pageref`）
  - citation 系参照は `REQ-FUNC-024` の `Citation Table` / `BibliographyState` に委譲する
- **出力**: LaTeX カーネル機構が正しく処理された中間表現
- **受け入れ基準**:
  - Given `FTX-CORPUS-COMPAT-001/layout-core/article` と `FTX-ASSET-BUNDLE-001`, When コンパイル, Then TeX Live 非導入環境でも pdfLaTeX と同等のレイアウトで PDF が生成される
  - Given 複数の `\usepackage` が互いに依存する文書, When パッケージ読み込み, Then 依存順序が正しく解決される
- **優先度**: Must
- **出典**: ユーザー明示

#### REQ-FUNC-021: amsmath サポート

- **説明**: amsmath パッケージが提供する数式環境・コマンドを正しく処理する
- **入力**: amsmath の数式環境を含む LaTeX ソース
- **処理**:
  - 複数行数式環境（`align`, `gather`, `multline`, `flalign`, `alignat`）
  - 数式内構造（`\text`, `\intertext`, `\substack`, `\overset`, `\underset`）
  - `split` 環境（他の数式環境内でのサブ分割）
  - 番号付け制御（`*` 付き環境、`\notag`, `\tag`）
- **出力**: 正しくレイアウトされた数式ボックス
- **受け入れ基準**:
  - Given `align` 環境で `&` による揃え位置指定と `\\` による改行を含む数式, When 組版, Then 揃え位置が正しく整列し各行に番号が付与される
  - Given `align`* 環境, When 組版, Then 数式番号が付与されない
- **優先度**: Must
- **出典**: ユーザー明示
- **関連要件**: REQ-FUNC-009

#### REQ-FUNC-022: hyperref サポート

- **説明**: hyperref パッケージによるハイパーリンク生成と PDF メタデータ設定を処理する
- **入力**: hyperref のコマンド・オプションを含む LaTeX ソース
- **処理**:
  - `\href`, `\url` による外部リンク生成
  - `\ref`, `\cite` の自動リンク化
  - 目次・しおりへのリンク付与
  - PDF メタデータ（`pdftitle`, `pdfauthor` 等）を Navigation State 内の metadata draft に反映
  - セクション構造・named destination・既定リンク装飾設定を Navigation State に蓄積し、PDF 生成段階へ受け渡す
  - 各リンク出現箇所を内部 destination または外部 URI を持つ Link Annotation Plan に正規化する
  - リンクの装飾（色枠、色付きテキスト）を Link Style として正規化し、annotation border と text-side paint へ分配する
  - `colorlinks=true` の場合は Link Style の `textColor` をリンク文字列の text run style に反映し、content stream 側のテキスト着色として出力する
- **出力**: ハイパーリンクと PDF メタデータを含む PDF
- **受け入れ基準**:
  - Given `\hypersetup{colorlinks=true}` と `\href{URL}{link}` を含む文書, When PDF 生成, Then クリック可能なリンクが色付きテキストとして出力される
  - Given `pdftitle`, `pdfauthor` が設定された文書, When PDF 生成, Then PDF のドキュメントプロパティに反映される
- **優先度**: Must
- **出典**: ユーザー明示
- **関連要件**: REQ-FUNC-015

#### REQ-FUNC-023: tikz/pgf サポート

- **説明**: tikz/pgf パッケージによるベクター図形描画を処理する
- **入力**: `tikzpicture` 環境を含む LaTeX ソース
- **処理**:
  - tikz の描画コマンド（`\draw`, `\fill`, `\node` 等）のパース・実行
  - 座標計算とパス構築
  - 変換（回転、拡大縮小、平行移動）、スタイル継承、クリッピング、矢印指定の適用
  - 描画結果を PDF 直結ではなくグラフィックシーンへ正規化し、画像埋め込みと共通の描画パイプラインへ受け渡す
  - PDF グラフィックオペレータへの変換
- **出力**: ベクター図形を含む PDF コンテンツストリーム
- **受け入れ基準**:
  - Given `FTX-CORPUS-TIKZ-001/basic-shapes`, When コンパイル, Then 線、矩形、円、テキストノードの幾何関係が pdfLaTeX と一致する
  - Given `FTX-CORPUS-TIKZ-001/nested-style-transform-clip-arrow`, When コンパイル, Then 継承 style、クリッピング境界、描画順、矢印形状が pdfLaTeX と一致する
- **優先度**: Must
- **出典**: ユーザー明示

#### REQ-FUNC-024: 参考文献読み込みと組版

- **説明**: 事前生成済みの `.bbl` ファイルを読み込み、参考文献リストの組版と `\cite` 解決を行う
- **入力**: `\bibliography`, `\addbibresource` 等を含む文書、および事前生成済みの `.bbl` ファイル
- **処理**:
  - `.bbl` ファイルを読み込み、Bbl Snapshot と Citation Table を構築する
  - `\cite` コマンドの参照解決は Citation Table を用いて行い、`REQ-FUNC-011` のラベル/ページ参照とは責務を分離する
  - `BibliographyState` は `.bbl` 取り込み、Citation Table 構築、`BibliographyEntry` provenance 管理、参考文献リストの組版データ生成を担う
  - `CrossReferenceTable` は `\label` / `\ref` / `\pageref` のみを扱い、citation 系は扱わない
  - `.bbl` が存在しないか古い場合は、外部ツール（`bibtex` / `biber`）の手動実行を案内する診断を返す
- **出力**: 引用テキストと参考文献リストが組版された出力
- **受け入れ基準**:
  - Given bibtex で生成された `.bbl` ファイルがある文書, When コンパイル, Then 参考文献リストが正しく組版される
  - Given `\cite{knuth1984}` と対応する `.bbl` エントリがある文書, When コンパイル, Then `\cite` は Citation Table から解決される
- **優先度**: Must
- **出典**: ユーザー明示
- **関連要件**: REQ-FUNC-024a

#### REQ-FUNC-024a: 外部参考文献ツール連携

- **説明**: 外部ツール（`bibtex`, `biber`）の自動実行による `.bbl` 生成を行う
- **入力**: `\bibliography`, `\addbibresource` 等を含む文書、`--shell-escape` オプション
- **処理**:
  - `--shell-escape` 有効時に `REQ-FUNC-047` 経由で `bibtex` / `biber` を実行し、`.bbl` を生成する
  - Ferritex が制御した外部ツールが `.bbl` を output root 配下へ生成した場合は、Output Artifact Registry に trusted external artifact として登録する
- **出力**: 生成された `.bbl` ファイル（`REQ-FUNC-024` の入力として使用される）
- **受け入れ基準**:
  - Given `.bib` ファイルを参照する文書, When `--shell-escape` 付きでコンパイル, Then `bibtex` / `biber` が自動実行され `.bbl` が生成される
  - Given `--shell-escape` なしでコンパイル, When `.bbl` が未生成, Then 外部ツールの手動実行を案内する診断が表示される
- **優先度**: Should
- **出典**: ユーザー明示
- **関連要件**: REQ-FUNC-024, REQ-FUNC-047

#### REQ-FUNC-025: fontspec サポート

- **説明**: fontspec パッケージによる OpenType フォント指定を処理する
- **入力**: `\setmainfont`, `\setsansfont`, `\setmonofont` 等を含む文書
- **処理**:
  - フォント名によるフォントファイルの解決
  - OpenType フィーチャ（`Ligatures`, `Numbers` 等）の適用
  - フォントフォールバックチェーン
- **出力**: 指定フォントが適用された組版結果
- **受け入れ基準**:
  - Given `\setmainfont{Noto Serif}` を指定した文書, When コンパイル, Then Noto Serif フォントで組版され、PDF に埋め込まれる
- **優先度**: Should
- **出典**: ユーザー明示
- **関連要件**: REQ-FUNC-017

#### REQ-FUNC-026: 汎用パッケージ読み込み

- **説明**: CTAN 等で配布される `.sty` ファイルを汎用的に読み込み、TeX/LaTeX コマンドとして実行する
- **入力**: `.sty` ファイルパス
- **処理**:
  - プロジェクトローカル資産、設定済み read-only overlay roots、Ferritex Asset Bundle の順で `.sty` ファイルを探索
  - `.sty` ファイルを中間表現へコンパイルし、再利用可能なパッケージスナップショットとしてキャッシュ
  - パッケージオプションの処理（`\DeclareOption`, `\ProcessOptions`）
  - `\RequirePackage` による依存パッケージの再帰的読み込み
  - パッケージ内で定義されるマクロ・環境をレジストリへ登録
  - `FTX-ASSET-BUNDLE-001` および `FTX-CORPUS-COMPAT-001` が要求する e-TeX と package-facing pdfTeX 拡張プリミティブは互換層で吸収する
- **出力**: パッケージ定義が適用された状態
- **例外**: パッケージ未発見時にエラー報告。XeTeX 固有プリミティブや corpus / bundle 外の未対応 engine 拡張使用時は構造化警告を出力し、依存箇所は non-success として停止する
- **受け入れ基準**:
  - Given `geometry`, `graphicx`, `xcolor` 等の標準パッケージが Asset Bundle に含まれる環境, When 読み込み, Then TeX Live 非導入環境でもエラーなく処理されパッケージの機能が利用可能
  - Given プロジェクト内に同名の `mystyle.sty` があり Bundle 内にも同名資産がある環境, When `\usepackage{mystyle}` を実行, Then プロジェクトローカル版が優先される
  - Given `FTX-ASSET-BUNDLE-001` に含まれる標準パッケージが package-facing pdfTeX 拡張プリミティブを使用する場合, When 読み込み, Then 互換層がそのプリミティブを吸収し package 機能が利用可能
  - Given XeTeX 固有プリミティブまたは corpus / bundle 外の未対応 engine 拡張を含むパッケージ, When 読み込み, Then 未対応プリミティブ名と停止理由を含む警告が返る
- **優先度**: Must
- **出典**: ユーザー明示

### 3.6 差分コンパイル

#### REQ-FUNC-027: 依存グラフ構築

- **説明**: TeX ソースファイル・マクロ定義・相互参照間の依存関係をグラフとして構築する
- **入力**: コンパイル時のファイル読み込み・マクロ定義・参照情報
- **処理**:
  - ファイル間依存（`\input`, `\include`, `\usepackage`）の記録
  - マクロ依存（マクロ A の定義がマクロ B の展開結果に依存する関係）の追跡
  - 相互参照依存（`\label` → `\ref` の参照関係）の記録
  - 依存グラフの永続化（キャッシュディレクトリへの保存）
- **出力**: 依存グラフデータ構造（ノード: ファイル/マクロ/ラベル、エッジ: 依存関係）
- **受け入れ基準**:
  - Given 10ファイルから成るマルチファイル文書, When 初回コンパイル, Then 全ファイル間の依存関係が正しく記録される
  - Given 依存グラフが保存済みの状態, When 次回コンパイル起動時, Then キャッシュから依存グラフが復元される
- **優先度**: Must
- **出典**: ユーザー明示

#### REQ-FUNC-028: 変更検知

- **説明**: ソースファイルの変更を検知し、依存グラフに基づいて再処理が必要な範囲を算出する
- **入力**: 依存グラフ、変更されたファイル群
- **処理**:
  - ファイルハッシュ（内容ベース）による変更検知
  - 依存グラフ上の影響伝播解析（変更ノードから到達可能な全ノードを特定）
  - マクロ定義変更の影響範囲特定
- **出力**: 再処理が必要なノード（ファイル/セクション）のリスト
- **受け入れ基準**:
  - Given 1ファイルのみ変更されたマルチファイル文書, When 変更検知, Then そのファイルと依存先のみが再処理対象として特定される
  - Given マクロ定義が変更された場合, When 変更検知, Then そのマクロの全使用箇所が再処理対象に含まれる
- **優先度**: Must
- **出典**: ユーザー明示
- **関連要件**: REQ-FUNC-027

#### REQ-FUNC-029: キャッシュ管理

- **説明**: コンパイル中間結果をキャッシュし、差分コンパイル時に再利用する
- **入力**: コンパイル中間結果（トークン列、ボックスツリー、ページレイアウト等）
- **処理**:
  - 中間結果のシリアライズとキャッシュディレクトリへの保存
  - 変更検知結果に基づくキャッシュの選択的無効化
  - キャッシュ整合性の検証（バージョン不一致・破損検出）
  - キャッシュサイズ管理（上限設定、LRU による古いエントリの削除）
- **出力**: 有効なキャッシュエントリの読み出し結果
- **例外**: キャッシュ破損検出時は警告を出力し、フルコンパイルにフォールバック
- **受け入れ基準**:
  - Given 初回コンパイル済みのキャッシュがある状態, When 変更なしで再コンパイル, Then キャッシュが再利用されフルコンパイルの 10% 以下の時間で完了
  - Given 破損したキャッシュファイル, When コンパイル, Then 警告を出力しフルコンパイルが正常に完了する
- **優先度**: Must
- **出典**: ユーザー明示

#### REQ-FUNC-030: 部分再コンパイル

- **説明**: 変更影響範囲のみを再処理し、キャッシュ済みの結果とマージして出力する
- **入力**: 変更検知結果、依存グラフ、キャッシュ済み中間結果、current Compilation Job
- **処理**:
  - 変更影響範囲と依存グラフから、再構築ノードと再利用ノードを分離した再コンパイルプランを構築
  - 差分コンパイル統括コンポーネントが `Compilation Job` を所有単位として再コンパイルプランを実行し、各反復で新しい `Compilation Session` を生成する
  - 変更対象範囲の再パース・再展開・再組版
  - ページ番号・相互参照の再計算（変更による連鎖的な番号ずれの検知）
  - 再処理結果とキャッシュ結果の統合
  - 同一 Compilation Job 内で参照安定性を判定し、pass ごとに新しい Compilation Session を作り直しながら、安定するまで反復（差分コンパイルでも最大3パス）
- **出力**: 更新された PDF
- **受け入れ基準**:
  - Given 100ページの文書の1段落を変更, When 差分コンパイル, Then フルコンパイル結果と同一の PDF が生成される
  - Given ページ番号がずれる変更, When 差分コンパイル, Then 目次・相互参照が正しく更新される
- **優先度**: Must
- **出典**: ユーザー明示
- **関連要件**: REQ-FUNC-028, REQ-FUNC-029

### 3.7 並列処理

#### REQ-FUNC-031: パイプライン並列化

- **説明**: コンパイルパイプラインの各ステージを並列実行する
- **入力**: TeX ソース
- **処理**:
  - ストリーミング処理: 前段の出力を後段がインクリメンタルに消費
  - ステージ間バッファリング（生産者-消費者パターン）
  - 並列安全なレジスタ・マクロ状態管理
  - 並列ステージが参照するマクロ・レジスタ・文書状態は `Compilation Snapshot` として読み取り専用で受け渡し、可変状態への反映は `Commit Barrier` で逐次化する
  - スレッドプール管理（CPU コア数に基づく自動設定）
- **出力**: 並列処理による高速化されたコンパイル結果
- **受け入れ基準**:
  - Given 4コア以上の CPU 環境, When 100ページ文書をコンパイル, Then シングルスレッド実行と同一の出力が得られ処理時間が短縮される
  - Given 1コアの環境, When コンパイル, Then シングルスレッドにフォールバックし正常に動作する
  - Given 並列実行中に複数ステージが同じマクロ・レジスタ状態を参照, When 片方のステージが処理を完了, Then 他方のステージは同一 `Compilation Snapshot` を観測し、可変状態への反映は `Commit Barrier` 通過後にのみ行われる
- **優先度**: Must
- **出典**: ユーザー明示

#### REQ-FUNC-032: 文書パーティション単位並列化

- **説明**: 独立した章またはセクション単位の文書パーティションを並列に処理する
- **入力**: 章またはセクションで分割可能なマルチファイル構成の文書
- **処理**:
  - 章・セクション間の独立性判定（相互参照の依存解析）
  - 独立パーティションの並列組版
  - 結果のマージとページ番号の統合
- **出力**: 並列処理された文書パーティションが統合された出力
- **受け入れ基準**:
  - Given 10章から成る `book` 文書, When コンパイル, Then 独立した章が並列に処理され、シングルスレッドと同一の出力が得られる
  - Given chapter を持たない `article` 文書で独立した section 群がある場合, When コンパイル, Then 安全な section 単位だけが並列に処理され、シングルスレッドと同一の出力が得られる
- **優先度**: Should
- **出典**: ユーザー明示
- **関連要件**: REQ-FUNC-031

#### REQ-FUNC-033: フォント処理並列化

- **説明**: 複数フォントの読み込み・埋め込みを並列に実行する
- **入力**: 使用フォントのリスト
- **処理**: 各フォントのファイル読み込み・パース・サブセット化を独立したタスクとして並列実行
- **出力**: 並列処理されたフォントデータ
- **受け入れ基準**:
  - Given 10種類以上のフォントを使用する文書, When コンパイル, Then フォント処理が並列実行され、逐次処理より短い時間で完了する
- **優先度**: Could
- **出典**: ユーザー明示

### 3.8 LSP サーバー

#### REQ-FUNC-034: 構文エラー診断

- **説明**: LSP プロトコルに準拠し、TeX/LaTeX ソースの構文エラー・警告をリアルタイムにエディタへ通知する
- **入力**: 編集中の TeX ソースと最新の `Open Document Buffer`（`textDocument/didOpen`, `textDocument/didChange`）
- **処理**:
  - `textDocument/didOpen` / `textDocument/didChange` を `Open Document Buffer` へ反映する
  - `Open Document Buffer` と最新の成功した `CommitBarrier` 完了時点で確定した `Stable Compile State` から `Live Analysis Snapshot` を構築し、それを唯一の解析入力として使う
  - diagnostics / completion / definition / hover の read path は active compile/watch job の完了を待たず、最新の `Stable Compile State` を用いて応答する
  - インクリメンタルなパース・マクロ展開によるエラー検出
  - エラー位置（行・列）の特定
  - エラーの重大度分類（Error, Warning, Information, Hint）
  - 代表的なエラーパターンに対する修正候補の提案（`codeAction`）
- **出力**: `textDocument/publishDiagnostics` 通知、および `textDocument/codeAction` で返却可能な修正候補
- **受け入れ基準**:
  - Given `\begin{equation}` に対応する `\end` がない文書, When エディタで開く, Then 該当行にエラー診断が表示される
  - Given `\begin{equation}` に対応する `\end{equation}` がない文書, When `textDocument/codeAction` を要求, Then `\end{equation}` を補う修正候補が返される
  - Given 未保存の編集で不足していた `\end{equation}` を補った `Open Document Buffer`, When `textDocument/didChange` 後に再診断, Then 保存前でも当該エラー診断が消える
  - Given ソース編集後, When 保存前の段階で, Then 500ms 以内に診断が更新される
  - Given watch による再コンパイルが進行中, When `textDocument/publishDiagnostics` の更新を行う, Then 進行中 job の完了を待たず最新の `Stable Compile State` と `Open Document Buffer` から診断を返す
- **優先度**: Must
- **出典**: ユーザー明示

#### REQ-FUNC-035: 補完

- **説明**: TeX/LaTeX のコマンド・環境名・ラベル・参考文献キーの補完候補を提供する
- **入力**: カーソル位置のコンテキストと最新の `Open Document Buffer`（`textDocument/completion`）
- **処理**:
  - `Open Document Buffer` と最新の成功した `CommitBarrier` 完了時点で確定した `Stable Compile State` から `Live Analysis Snapshot` を構築し、それを唯一の解析入力として使う
  - `\` 入力後にコマンド名候補を提示（使用中パッケージのコマンドを含む）
  - `\begin{` 入力後に環境名候補を提示
  - `\ref{` 入力後に定義済みラベル一覧を提示
  - `\cite{` 入力後に参考文献キー一覧を提示
- **出力**: `CompletionItem` のリスト
- **受け入れ基準**:
  - Given `graphicx` を使用中の文書で `\includegr` と入力, When 補完要求, Then `\includegraphics` が候補に含まれる
  - Given amsmath を使用中の文書で `\begin{al` と入力, When 補完要求, Then `align`, `alignat`, `aligned` 等が候補に含まれる
  - Given 未保存の `Open Document Buffer` 内に `\label{fig:overview}` が定義済み, When `\ref{fig:` と入力, Then 保存前でも `fig:overview` が候補に表示される
  - Given `.bbl` スナップショットに `knuth1984` が含まれる文書, When `\cite{kn` と入力, Then `knuth1984` が候補に表示される
- **優先度**: Must
- **出典**: ユーザー明示

#### REQ-FUNC-036: 定義ジャンプ

- **説明**: `\label`, `\ref`, `\cite`, マクロ定義へのジャンプ機能を提供する
- **入力**: カーソル位置と最新の `Open Document Buffer`（`textDocument/definition`）
- **処理**:
  - `Open Document Buffer` と最新の成功した `CommitBarrier` 完了時点で確定した `Stable Compile State` から `Live Analysis Snapshot` を構築し、それを唯一の解析入力として使う
  - マクロ定義、ラベル、参考文献エントリの Definition Provenance を `Live Analysis Snapshot` から再構築したシンボル索引として保持する
  - `\ref{label}` → 対応する `\label{label}` の位置
  - `\cite{key}` → 対応する参考文献エントリの位置（`BibliographyEntry.provenance` があれば元ソース位置、なければ `.bbl` スナップショット上の定義位置）
  - `\command` → `\newcommand{\command}` の定義位置
- **出力**: 定義位置の `Location`
- **受け入れ基準**:
  - Given `\ref{fig:overview}` にカーソルを置いた状態, When 定義ジャンプを実行, Then `\label{fig:overview}` の位置にジャンプする
  - Given `\newcommand{\foo}[1]{...}` が定義された文書で `\foo{bar}` にカーソルを置いた状態, When 定義ジャンプを実行, Then `\newcommand{\foo}` の位置にジャンプする
  - Given `\cite{knuth1984}` と対応する `.bbl` エントリがある文書, When 定義ジャンプを実行, Then `knuth1984` の参考文献エントリ位置が返される
- **優先度**: Should
- **出典**: ユーザー明示

#### REQ-FUNC-037: ホバー情報

- **説明**: コマンドにカーソルを合わせた際にドキュメント情報を表示する
- **入力**: カーソル位置と最新の `Open Document Buffer`（`textDocument/hover`）
- **処理**:
  - `Open Document Buffer` と最新の成功した `CommitBarrier` 完了時点で確定した `Stable Compile State` から `Live Analysis Snapshot` を構築し、それを唯一の解析入力として使う
  - コマンド名に基づくドキュメントの検索と表示
- **出力**: ホバー情報（Markdown 形式）
- **受け入れ基準**:
  - Given `\frac` にカーソルを合わせた状態, When ホバー, Then 構文と使用例を含む説明が表示される
- **優先度**: Could
- **出典**: ユーザー明示

### 3.9 リアルタイムプレビュー

#### REQ-FUNC-038: ファイル監視

- **説明**: TeX ソースファイルおよび関連ファイルの変更を監視する
- **入力**: 監視対象のファイルパス群
- **処理**:
  - OS ネイティブのファイル監視 API（`inotify`, `FSEvents`, `ReadDirectoryChangesW`）を使用
  - `\input`, `\include` 先の依存ファイルも自動で監視対象に追加
  - 各再コンパイル完了後に最新の依存グラフから監視対象集合を再同期し、新たに解決された `\input`, `\include`, `\usepackage` 先を次回監視へ反映する
  - デバウンス処理（短時間の連続変更をまとめて1回のトリガー、デフォルト 100ms）
- **出力**: 変更イベント（変更されたファイルパスのリスト）
- **受け入れ基準**:
  - Given `\include{chap1}` を含む文書を監視中, When `chap1.tex` を変更, Then 変更イベントが発火する
  - Given watch 実行中の再コンパイルで `\input{appendix}` が新たに解決された文書, When その後 `appendix.tex` を変更, Then 追加設定なしで変更イベントが発火する
- **優先度**: Must
- **出典**: ユーザー明示

#### REQ-FUNC-039: 差分コンパイル連携

- **説明**: ファイル変更イベントを受けて差分コンパイルをトリガーする
- **入力**: ファイル監視からの変更イベント
- **処理**:
  - 変更検知（REQ-FUNC-028）の呼び出し
  - 部分再コンパイル（REQ-FUNC-030）の実行
  - `Recompile Scheduler` がコンパイル中フラグを管理し、同時に複数の差分コンパイルを開始しない
  - コンパイル中の新たな変更を `Pending Change Queue` にキューイングし、完了後に coalesce した変更集合で再トリガーする
- **出力**: 更新された PDF
- **受け入れ基準**:
  - Given ウォッチモード中にソースを編集, When 保存, Then 差分コンパイルが自動実行され更新された PDF が出力される
  - Given コンパイル中にさらに変更が発生, When 現在のコンパイル完了後, Then キューされた変更に対して再コンパイルが実行される
- **優先度**: Must
- **出典**: ユーザー明示
- **関連要件**: REQ-FUNC-028, REQ-FUNC-030

#### REQ-FUNC-040: PDF 配信

- **説明**: コンパイル結果の PDF をプレビューアに配信する
- **入力**: 生成された PDF ファイル
- **処理**:
  - preview client は `(workspaceRoot, primaryInput, jobname)` から成る `Preview Target` を loopback 上の `POST /preview/session` へ送信し、`Preview Session Service` は同一 process かつ同一 target の既存 session があれば再利用し、なければ新しい `sessionId` と `documentUrl` / `eventsUrl` を返す。process restart または target 変更時は sessionId を再発行し、旧 sessionId を失効させる
  - `Preview Session Service` が `Execution Policy.previewPublication` に照らして publish 可否を判定し、active job の `Preview Target` と session owner が一致する場合だけ `Preview Transport` を loopback に bind した `GET /preview/{sessionId}/document` で最新 PDF を配信する
  - 同じ session に対して `Preview Session Service` が `Preview Transport` の `WS /preview/{sessionId}/events` を介して target 付き `Preview Revision`、page count、view-state 更新を交換する
  - sessionId ごとに `Preview Session` を保持し、`Preview Target` と `Preview View State` として現在ページ、ページ内オフセット、ズーム倍率を保存する
  - PDF 更新時は保持済みの `Preview View State` を優先して再適用し、該当ページが消滅した場合のみ最近傍の有効ページへフォールバックする
- **出力**: プレビューア上での更新された PDF 表示
- **受け入れ基準**:
  - Given preview client が `POST /preview/session` に `(workspaceRoot, primaryInput, jobname)` を送る, When 同一 process かつ同一 target の session が存在する, Then 同じ `sessionId` と `documentUrl` / `eventsUrl` が返る
  - Given loopback 上で `(workspaceRoot, primaryInput, jobname)` に紐づく preview session が確立済みで `GET /preview/{sessionId}/document` と `WS /preview/{sessionId}/events` に接続したプレビューア, When 同じ target の再コンパイル完了, Then 1秒以内に target 付き document revision 通知が届き閲覧ページ位置が維持される
  - Given 再コンパイル前に 20 ページ目を閲覧中で、再コンパイル後の PDF が 15 ページに短縮された場合, When プレビューが更新, Then 最近傍の有効ページである 15 ページ目へフォールバックしズーム倍率を維持する
  - Given process restart 後または別 `Preview Target` へ切り替え後の古い `sessionId`, When 旧 session へ接続または publish を試みる, Then 旧 session は `410 Gone` 相当で拒否され、新しい `POST /preview/session` による sessionId 再取得が要求される
- **優先度**: Must
- **出典**: ユーザー明示

#### REQ-FUNC-041: SyncTeX 互換

- **説明**: ソース位置と PDF 位置の双方向ジャンプを実現する
- **入力**: ソース位置（ファイル名:行:列）または PDF 位置（ページ:座標）
- **処理**:
  - タイプセッティング時に placed node ごとの Source Span と配置矩形、および placed destination を記録する
  - 1 つの Source Span が複数行・複数ページに分割された場合でも、各配置断片を `SyncTeX Trace Fragment` として保持する
  - Page Render Plan 上の source trace から fragment ベースの SyncTeX 互換データを生成する
  - ソース → PDF（フォワードサーチ）では SourceLocation に交差する fragment 群から候補位置を返す
  - PDF → ソース（インバースサーチ）では指定座標を含む fragment から対応する Source Span を解決する
- **出力**: 対応する PDF 位置群またはソース範囲
- **受け入れ基準**:
  - Given SyncTeX データが生成された文書, When ソースの特定行を指定, Then 対応する PDF のページと座標が返される
- **優先度**: Should
- **出典**: ユーザー明示

### 3.10 CLI インターフェース

#### REQ-FUNC-042: コンパイル実行

- **説明**: CLI からの基本的なコンパイルコマンドを提供する
- **入力**: `ferritex compile <file.tex>`
- **処理**:
  - 入力ファイルの読み込みとコンパイルパイプラインの実行
  - 進捗表示（ページ数、処理中ファイル名）
  - 終了コード: 0（成功）、1（警告あり成功）、2（エラー）
- **出力**: PDF ファイル、コンパイルログ
- **受け入れ基準**:
  - Given 有効な LaTeX ファイル, When `ferritex compile main.tex` を実行, Then カレントディレクトリに `main.pdf` が生成される
  - Given コンパイルエラーを含むファイル, When コンパイル実行, Then エラー箇所（ファイル名:行番号）とメッセージが表示され終了コード 2 で終了する
- **優先度**: Must
- **出典**: ユーザー明示

#### REQ-FUNC-043: オプション管理

- **説明**: コンパイルの動作を制御する各種 CLI オプションを提供する
- **入力**: CLI フラグ・引数
- **処理**: 以下のオプションをサポート
  - `--output-dir <dir>`: PDF / `.aux` / `.log` / SyncTeX 等の成果物出力先。指定時は正規化後のディレクトリを明示的 output root として `ExecutionPolicy` に追加し、Ferritex または Ferritex が制御した外部ツール実行で生成され Output Artifact Registry に記録された `.aux` / `.toc` / `.lof` / `.lot` / `.bbl` / `.synctex` 等のうち、current Compilation Job の `jobname` と主入力に整合するものだけ readback を許可する。現在パス番号と生成パス番号は監査用に保持するが一致条件には含めない
  - `--jobname <name>`: ジョブ名（出力ファイル名）の指定。`Runtime Options.jobname` に正規化され、same-job 判定と出力命名の共通語彙として使う
  - `--jobs <N>`: 並列処理のスレッド数（デフォルト: CPU コア数）
  - `--no-cache`: キャッシュを無効化しフルコンパイル
  - `--asset-bundle <ref>`: 使用する Ferritex Asset Bundle の指定。`<ref>` はファイルパスまたは組み込みバンドル識別子
  - `--interaction <mode>`: インタラクションモード（`nonstopmode`, `batchmode`, `scrollmode`）
  - `--synctex`: SyncTeX データの生成有無
  - `--shell-escape` / `--no-shell-escape`: 外部コマンド実行の許可
  - compile / watch / LSP の各入口で受け取った指定は `primaryInput`, `artifactRoot`, `jobname`, `parallelism`, `reuseCache`, `assetBundleRef`, `interactionMode`, `synctex`, `shellEscapeAllowed` から成る共通の `Runtime Options` に正規化され、それを基に同一の `Execution Policy` を構築する
- **出力**: 指定オプションに従ったコンパイル動作
- **受け入れ基準**:
  - Given `--output-dir build` を指定, When コンパイル, Then `build/` ディレクトリに PDF が生成される
  - Given `--jobs 1` を指定, When コンパイル, Then シングルスレッドで実行される
- **優先度**: Must
- **出典**: ユーザー明示

#### REQ-FUNC-044: ウォッチモード

- **説明**: ファイル監視と自動再コンパイルを CLI から利用可能にする
- **入力**: `ferritex watch <file.tex>`
- **処理**:
  - ファイル監視（REQ-FUNC-038）の開始
  - 変更検知時の差分コンパイル連携（REQ-FUNC-039）
  - Ctrl+C によるグレースフル停止
- **出力**: 継続的な PDF 更新
- **受け入れ基準**:
  - Given `ferritex watch main.tex` を実行中, When `main.tex` を編集・保存, Then 自動的に再コンパイルが実行される
- **優先度**: Should
- **出典**: ユーザー明示
- **関連要件**: REQ-FUNC-038, REQ-FUNC-039

#### REQ-FUNC-045: LSP サーバー起動

- **説明**: CLI から LSP サーバーを起動し、エディタとの通信を開始する
- **入力**: `ferritex lsp`
- **処理**:
  - 標準入出力（stdio）を介した LSP プロトコル通信の開始
  - `initialize` ハンドシェイクで必須 capability（`textDocumentSync`, `completionProvider`, `codeActionProvider`）を通知し、optional provider の capability（`definitionProvider`, `hoverProvider` など）は provider 有効時のみ advertise する
  - プロジェクトルートの自動検出
- **出力**: LSP プロトコルに準拠したリクエスト/レスポンス
- **受け入れ基準**:
  - Given `ferritex lsp` を起動, When エディタから `initialize` リクエストを受信, Then 必須 capability（`textDocumentSync`, `completionProvider`, `codeActionProvider`）を含む応答が返される
  - Given definition provider が有効な build, When `initialize` リクエストを受信, Then `definitionProvider` を含む応答が返される
  - Given hover provider が有効なビルド, When `initialize` リクエストを受信, Then `hoverProvider` を含む応答が返される
- **優先度**: Must
- **出典**: ユーザー明示

### 3.11 アセットランタイム・実行制御

#### REQ-FUNC-046: アセットバンドル読み込み

- **説明**: クラス・パッケージ・フォント資産を Ferritex Asset Bundle から高速に読み込む
- **入力**: Asset Bundle のパスまたは組み込みバンドル識別子
- **処理**:
  - バンドルマニフェストとバージョンの検証
  - Asset Index を memory-mapped に読み込み、クラス・パッケージ・フォントの解決 API を提供
  - プロジェクトローカルオーバーレイ、設定済み read-only overlay roots、Ferritex Asset Bundle、Host Font Catalog overlay fallback の順で優先順位付き合成
- **出力**: アセット解決ハンドル
- **例外**: バージョン不一致または破損時は診断を表示し、互換バンドルがなければ起動を失敗させる
- **受け入れ基準**:
  - Given `FTX-ASSET-BUNDLE-001` のみが存在する環境, When `ferritex compile main.tex` を実行, Then TeX Live 非導入でも `FTX-CORPUS-COMPAT-001/layout-core` の baseline 文書群がコンパイルできる
  - Given Asset Bundle と Host Font Catalog の両方に同名フォント資産が存在する環境, When 通常の解決 API を呼び出す, Then Asset Bundle 側の資産が優先される
  - Given バンドルが破損している環境, When 読み込み, Then 破損診断が表示されコンパイルは開始されない
- **優先度**: Must
- **出典**: ユーザー明示（高速化方針として TeX ランタイムの実行時依存を排除）

#### REQ-FUNC-047: 外部コマンド実行ゲート

- **説明**: `\write18` 等による外部コマンド実行を単一のゲートウェイ経由で制御する
- **入力**: コマンド文字列、コンパイルオプション、実行ポリシー
- **処理**:
  - `--shell-escape` 未指定時は実行要求を拒否し、診断を返す
  - `--shell-escape` 指定時のみサブプロセスを生成し、同一 Compilation Job あたり最大 1 プロセスまでの同時実行制限下で終了コード・標準出力・標準エラーを収集
  - デフォルト実行上限として、タイムアウト 30 秒、標準出力+標準エラーの合計捕捉量 4 MiB を適用し、超過時はプロセスを停止して診断を返す
  - Ferritex が制御した外部ツールが readback 対象補助ファイルを生成した場合、生成パス・artifact kind・主入力・jobname・producer kind・コンテンツハッシュを trusted external artifact として Output Artifact Registry に記録する
  - 実行ログを記録し、失敗時は TeX 側へ診断を返す
- **出力**: 実行結果または拒否診断
- **受け入れ基準**:
  - Given `\write18{echo ok}` を含む文書, When `--shell-escape` なしでコンパイル, Then コマンドは実行されず拒否診断が表示される
  - Given 同じ文書, When `--shell-escape` 付きでコンパイル, Then コマンドが実行され終了コードと出力が取得される
  - Given 同一 Compilation Job で 2 件の外部コマンド要求が連続して発生, When `--shell-escape` 付きでコンパイル, Then 同時実行数は 1 を超えず、後続コマンドは先行コマンド完了後に開始される
  - Given 4 MiB を超える標準出力を生成するコマンド, When `--shell-escape` 付きでコンパイル, Then プロセスは停止され出力上限超過の診断が表示される
- **優先度**: Must
- **出典**: REQ-NF-005 を機能要件へ具体化

#### REQ-FUNC-048: ファイルアクセスサンドボックス

- **説明**: コンパイル中のすべてのファイル読み書きを共通 File Access Gate とパスアクセスポリシーで制御する
- **入力**: パス要求（read/write/create）、アクセス目的（tex-input / tex-output / engine-output / engine-readback / engine-temp）、プロジェクトルート、設定済み overlay roots、Asset Bundle ルート、キャッシュディレクトリ、明示的 output root、current Job Context、Output Artifact Registry
- **処理**:
  - パス正規化とシンボリックリンク解決
  - すべての `\input`, `\include`, `\openin`, `\openout`, asset read, engine-temp / engine-output / engine-readback 要求を共通 File Access Gate に集約する
  - 許可領域の判定。読み取りはプロジェクト、設定済み read-only overlay roots、Asset Bundle、キャッシュに限定し、`engine-readback` に限って Output Artifact Registry が current Compilation Job の `jobname` と主入力の双方に整合する trusted artifact として確認した補助ファイル（`.aux`, `.toc`, `.lof`, `.lot`, `.bbl`, `.synctex` など）の再読込を許可する。現在パス番号と生成パス番号は監査・診断属性として保持するが same-job 一致条件には含めない。書き込みはキャッシュ、明示的 output root、private temp root に限定する
  - Ferritex 自身が確保した private temp dir をキャッシュ配下または明示的 output root 配下に作成し、`engine-temp` 用にのみ許可
  - Ferritex または Ferritex が制御した外部ツール実行で生成した readback 対象補助ファイルを、正規化パス・主入力・artifact kind・jobname・生成パス番号・生成者種別・生成パス・コンテンツハッシュ付きで Output Artifact Registry に記録する。生成パス番号は監査属性であり、trusted readback の一致判定は主入力と jobname で行う
  - Output Artifact Registry は current active `Compilation Job` にだけ属する in-memory authority とし、job 完了または process restart で必ず無効化する。append-only manifest は監査専用であり trusted 判定には使わない
  - システム一時領域全体は許可 root として公開しない
  - 拒否時の診断生成と、許可時のファイルハンドル発行
- **出力**: 許可されたファイルハンドルまたは拒否診断
- **受け入れ基準**:
  - Given プロジェクト内の `chap1.tex` を `\input` する文書, When コンパイル, Then 読み込みが許可される
  - Given 設定済み read-only overlay root にある `shared.sty` を読み込む文書, When コンパイル, Then 読み込みは許可されるが同 root への書き込みは拒否される
  - Given `--output-dir ../dist` を指定してコンパイル, When PDF / `.aux` / `.log` を生成, Then 正規化済み output root 配下への書き込みのみが許可される
  - Given `--output-dir ../dist` を指定して 2 パス以上のコンパイルを行う文書, When Ferritex が前パスで生成し Output Artifact Registry に記録した `../dist/main.aux` と `../dist/main.toc` を後続パスから再読込, Then `producedPass` と current pass number が異なっていても `engine-readback` として許可される
  - Given 同じ output root 配下に `foo.aux` と `bar.aux` が存在する環境, When current Job Context の jobname が `foo` のコンパイルから `bar.aux` を `engine-readback` しようとする, Then same-job 不一致として拒否される
  - Given `thesis.tex` と `article.tex` を同じ `--jobname shared` と同じ output root で順にコンパイルする環境, When `thesis.tex` の current Job Context から `article.tex` が生成した `shared.aux` を `engine-readback` しようとする, Then 主入力不一致として拒否される
  - Given `--output-dir ../dist` 配下にユーザーが事前配置した未登録の `main.aux` がある文書, When コンパイル, Then `engine-readback` は拒否され provenance 不一致の診断が表示される
  - Given `../../outside.txt` への `\openout` を試みる文書, When コンパイル, Then 書き込みが拒否され診断が表示される
- **優先度**: Must
- **出典**: REQ-NF-006 を機能要件へ具体化

## 4. 非機能要件

### 4.1 性能

#### REQ-NF-001: フルコンパイル速度

- **説明**: versioned benchmark profile `FTX-BENCH-001` をフルコンパイルで 1.0 秒未満に処理する
- **定量基準**: `FTX-BENCH-001`（100 ページ、`amsmath` + `hyperref` + `graphicx`、4 コア以上の CPU、同一入力・同一マシンで pdfLaTeX baseline と比較）に対して、`--no-cache` 指定のフルコンパイル完了時間の中央値が 1.0 秒未満である
- **計測方法**: `FTX-BENCH-001` の入力を同一マシンで 1 回ウォームアップ後に 5 回計測し中央値を採用する。相対速度比較も同一 profile・同一マシンで実施する
- **優先度**: Must
- **出典**: ユーザー明示

#### REQ-NF-002: 差分コンパイル速度

- **説明**: 単一段落の変更に対する差分コンパイルを高速に処理する
- **定量基準**: `FTX-BENCH-001` を一度フルコンパイルしてキャッシュと依存グラフを構築した状態で、本文 1 段落だけを変更した差分コンパイル完了時間の中央値が 100ms 未満である
- **計測方法**: `FTX-BENCH-001` を同一マシンでフルコンパイル済みの状態から、本文 1 段落の変更を適用して 5 回計測し中央値を採用する
- **優先度**: Must
- **出典**: ユーザー確認済み（2026-03-16）

#### REQ-NF-003: メモリ使用量

- **説明**: フルコンパイルと `LiveAnalysisSnapshot` 構築を含むピークメモリ使用量を合理的な範囲に抑える
- **定量基準**: `FTX-BENCH-001` のフルコンパイルと `LiveAnalysisSnapshot` 構築を含むピークメモリ使用量 < 1GB
- **計測方法**: `FTX-BENCH-001` を対象にフルコンパイルと `LiveAnalysisSnapshot` 構築を同時実行した状態で RSS（Resident Set Size）のピークを計測する
- **優先度**: Should
- **出典**: ユーザー確認済み（2026-03-16）

#### REQ-NF-004: LSP 応答速度

- **説明**: LSP サーバーの各操作がエディタ操作を阻害しない速度で応答する。read path は最新の `Stable Compile State` と `Open Document Buffer` を使い、active compile/watch job の完了を待たない
- **定量基準**:
  - 診断更新: < 500ms（編集後）
  - 補完候補提示: < 100ms
  - 定義ジャンプ: < 200ms
- **計測方法**: versioned benchmark profile `FTX-LSP-BENCH-001` の LSP trace を 1 回ウォームアップ後に 5 回再生し中央値を採用する
- **優先度**: Must
- **出典**: ユーザー明示（REQ-FUNC-034 の受け入れ基準から導出）

### 4.2 セキュリティ

#### REQ-NF-005: 外部コマンド実行制御

- **説明**: `\write18`（shell escape）による外部コマンド実行はデフォルトで無効とし、明示的なオプション指定時のみ有効にする。有効時も `ExecutionPolicy` のデフォルト上限（タイムアウト 30 秒、同時実行 1 プロセス / Compilation Job、捕捉出力 4 MiB）を適用する
- **定量基準**: `--shell-escape` フラグなしの状態で外部コマンドが実行される経路がゼロであり、`--shell-escape` 有効時もデフォルト上限（30 秒、1 プロセス / Compilation Job、4 MiB）を超える実行は必ず拒否または停止される
- **優先度**: Must
- **出典**: エージェント推測（TeX のセキュリティモデルとして標準的な対策）

#### REQ-NF-006: ファイルアクセス制御

- **説明**: TeX の `\openin`, `\openout` によるファイルアクセスを、読み取りではプロジェクトディレクトリ、設定済み read-only overlay roots、Ferritex Asset Bundle、キャッシュディレクトリに制限し、明示的 output root は Output Artifact Registry により current `Compilation Job` の `jobname` と主入力の双方に整合する trusted artifact と確認された補助ファイルの readback に限って読み取りを許可する。Output Artifact Registry は active job 限定の in-memory authority とし、job 完了または process restart で無効化する。現在パス番号と生成パス番号は監査属性として保持するが same-job 一致条件には含めない。書き込みはキャッシュディレクトリ、明示的 output root、Ferritex 管理下の private temp dir に制限する
- **定量基準**: 許可領域（読み取り: プロジェクト、設定済み read-only overlay roots、Asset Bundle、キャッシュ、active job の Output Artifact Registry に記録され current `Compilation Job` の `jobname` / 主入力の双方が一致する output root 配下の trusted artifact。書き込み: キャッシュ、明示的 output root、Ferritex 管理下の private temp dir）外への読み書きが発生する経路がゼロ
- **優先度**: Must
- **出典**: エージェント推測（pdfLaTeX の `openout_any = p` を発展させ、runtime bundle 設計へ適用）

### 4.3 互換性

#### REQ-NF-007: pdfLaTeX 出力互換性

- **説明**: `FTX-CORPUS-COMPAT-001` に対して、pdfLaTeX とレイアウト・リンク・埋め込み資産を含めて互換な PDF を生成する。互換対象の engine surface は `FTX-ASSET-BUNDLE-001` と `FTX-CORPUS-COMPAT-001` が要求する e-TeX および package-facing pdfTeX 拡張プリミティブに限定し、XeTeX 固有プリミティブは本要件の範囲外とする
- **定量基準**:
  - `FTX-CORPUS-COMPAT-001` の全 100 文書を文書単位で集計し、各文書について「全ページの行分割位置差分率 <= 5% かつ全ページのページ分割位置が一致」を満たす文書数が 95 文書以上である。ここで行分割位置差分率は、各ページごとに `|Ferritex の改行位置集合 △ pdfLaTeX の改行位置集合| / max(1, |pdfLaTeX の改行位置集合|)` を計算し、その文書内ページ平均を取った値とする
  - `FTX-CORPUS-COMPAT-001/navigation-features` の全文書で、正規化した PDF manifest 上の annotation 数、named destination 数、outline 階層、主要 metadata key（`Title`, `Author`）が 100% 一致する
  - `FTX-CORPUS-COMPAT-001/embedded-assets` の全文書で、埋め込みフォント集合、画像・外部 PDF の resource inventory、参照先ページ数が 100% 一致する
  - `FTX-CORPUS-COMPAT-001` 内の参考文献を含む文書で、参考文献リストのエントリ数・エントリ順序・各エントリの citation label が 100% 一致する。レイアウト互換性の判定は上記の行分割位置差分率基準を参考文献リスト部分にも適用する
- **計測方法**: `FTX-CORPUS-COMPAT-001` の各文書を `FTX-ASSET-BUNDLE-001` 前提で両エンジンから生成し、レイアウト差分はページごとの改行位置集合の対称差から算出した差分率と、ページ分割位置一致を文書単位で集計する。PDF 機能差分は `FTX-CORPUS-COMPAT-001/navigation-features` と `FTX-CORPUS-COMPAT-001/embedded-assets` に対して annotation / destination / outline / metadata / resource inventory / 埋め込みフォント集合 / 外部 PDF 参照先ページ数を正規化した manifest と埋め込み検証で比較する。参考文献互換性は参考文献リスト内のエントリ抽出・正規化後に citation label とエントリ順序を比較する
- **優先度**: Must
- **出典**: ユーザー明示

#### REQ-NF-008: クロスプラットフォーム動作

- **説明**: Linux, macOS, Windows の主要プラットフォームで、Ferritex Asset Bundle と project-local / configured read-only overlay roots に固定した資産のみを参照する入力に対して同一の出力を生成する
- **定量基準**: Host Font Catalog overlay を使わない同一入力に対して、全プラットフォームでバイト単位で同一の PDF が生成される（タイムスタンプ等のメタデータを除く）
- **計測方法**: CI で3プラットフォームの出力を比較
- **優先度**: Must
- **出典**: ユーザー明示

### 4.4 運用性

#### REQ-NF-009: インストール容易性

- **説明**: 単一バイナリで配布可能とし、外部ランタイムへの依存を最小化する
- **定量基準**: `cargo install ferritex` または単一バイナリのダウンロードと公式 Ferritex Asset Bundle の配置で利用開始可能。コンパイル実行時に TeX Live / kpathsea のインストールを要求しない
- **優先度**: Must
- **出典**: エージェント推測（Rust ツールチェーンの標準的な配布方法）

#### REQ-NF-010: エラーメッセージ品質

- **説明**: compile / watch / lsp / preview の全入口で、ユーザーが原因を特定し回復できるエラー応答を返す
- **定量基準**:
  - ソース診断（compile / watch / lsp）: 全エラーメッセージにファイル名・行番号・要約メッセージ・コンテキスト snippet が含まれる。可能な場合は修正候補を提示する
  - セッション応答（preview）: session 失効（`410 Gone` 相当）を含む全エラー応答にエラー種別・対象 sessionId・回復手順（`POST /preview/session` による再取得）が含まれる
- **優先度**: Must
- **出典**: エージェント推測（pdfLaTeX のエラーメッセージの分かりにくさへの改善として）

## 5. 未確定事項


| #   | 内容                                                                                                    | 関連要件         | 確認相手 |
| --- | ----------------------------------------------------------------------------------------------------- | ------------ | ---- |
| 1   | Ferritex Asset Bundle のスナップショット更新戦略。CTAN / TeX Live からどの頻度で資産を取り込み、互換バージョンをどう保持するか                                     | REQ-FUNC-046 | 開発者  |

## 変更履歴


| バージョン | 日付         | 変更内容 | 変更者             |
| ----- | ---------- | ---- | --------------- |
| 0.1.22 | 2026-03-17 | REQ-NF-004 の計測対象を専用 benchmark profile `FTX-LSP-BENCH-001` として定義し、用語集に追加 | Claude Opus 4.6 |
| 0.1.21 | 2026-03-17 | REQ-FUNC-024 を .bbl 読み込み＋参考文献組版（Must）と外部ツール連携 REQ-FUNC-024a（Should）に分割し、REQ-NF-007（Must）との優先度整合性を解消 | Claude Opus 4.6 |
| 0.1.20 | 2026-03-17 | REQ-NF-004 に計測方法を追加、REQ-FUNC-024 に処理境界を明記、REQ-NF-007 に参考文献互換指標を追加、FTX-CORPUS-COMPAT-001 に .bbl 同梱前提を明記 | Claude Opus 4.6 |
| 0.1.19 | 2026-03-16 | REQ-NF-003 の計測スコープを compile + LiveAnalysisSnapshot に拡張し、REQ-NF-010 の対象を preview session エラーに拡大 | Claude Opus 4.6 |
| 0.1.18 | 2026-03-16 | preview session bootstrap API、partition locator、pdfLaTeX 互換範囲、エラーメッセージ品質の必須項目を明文化し、未確定事項を整理 | Codex |
| 0.1.17 | 2026-03-15 | `Runtime Options.jobname` への語彙統一、preview session の owner/lifecycle/policy 追加、active-job 限定の Output Artifact Registry 寿命、LSP 非ブロッキング read path を反映 | Codex |
| 0.1.16 | 2026-03-15 | same-job readback の判定キーを主入力 + jobname に固定し、pass number を監査属性へ切り分け。メタ情報を最新版へ同期 | Codex |
| 0.1.15 | 2026-03-15 | preview transport 契約、PDF 妥当性判定、tikz 回帰コーパス、group-scope 巻き戻し、bibliography / runtime options の責務境界を明確化 | Codex |
| 0.1.14 | 2026-03-12 | LSP 要件に Open Document Buffer / Live Analysis Snapshot を導入し、completion の受け入れ基準と hover capability 条件を整合化 | Codex |
| 0.1.13 | 2026-03-12 | 性能要件の共通 benchmark profile `FTX-BENCH-001` を導入し、絶対速度/相対速度/差分速度の判定条件を統一 | Codex |
| 0.1.12 | 2026-03-12 | 並列実行の snapshot/barrier 契約、watch 対象集合の依存グラフ同期、preview の最近傍ページ fallback を追記 | Codex |
| 0.1.11 | 2026-03-12 | SyncTeX の fragment-based trace、watch 再トリガーの scheduler/queue、preview の view state を追記 | Codex |
| 0.1.10 | 2026-03-12 | SyncTeX の source trace / placed destination、colorlinks の text-side style、共通 File Access Gate を追記 | Codex |
| 0.1.9 | 2026-03-12 | Link Annotation Plan / Link Style、TOC/索引の box tree 投影、外部ツール成果物の trusted artifact 登録責務を追記 | Codex |
| 0.1.8 | 2026-03-12 | Navigation State / Table Of Contents State / Index State / Page Render Plan を用語化し、目次・hyperref・PDF 射影の責務を明確化 | Codex |
| 0.1.7 | 2026-03-12 | 差分コンパイルの job 単位反復、共通 PDF レンダリングパイプライン、entry point 非依存の Runtime Options を明文化 | Codex |
| 0.1.6 | 2026-03-12 | Compilation Job / Session を導入し、same-job readback の主入力照合と shell-escape のデフォルト実行上限を明文化 | Codex |
| 0.1.5 | 2026-03-12 | Definition Provenance と定義ジャンプ要件、差分コンパイルの再コンパイルプラン、same-job readback 用 Job Context を反映 | Codex |
| 0.1.4 | 2026-03-12 | output root readback provenance、font 解決優先順位、citation/bibliography の責務分離を反映 | Codex |
| 0.1.3 | 2026-03-12 | output root の補助ファイル readback、host-local font の再現性スコープ、LSP codeAction、PDF Form XObject 表現を追記 | Codex |
| 0.1.2 | 2026-03-12 | overlay roots の許可境界を明確化し、PDF グラフィック表現とフォント解決面の整合性を修正 | Codex |
| 0.1.1 | 2026-03-12 | フォント資産源を host-local overlay として明確化し、グラフィックシーン/出力 root/private temp root の方針を追記 | Codex |
| 0.1.0 | 2026-03-11 | 初版作成 | Claude Opus 4.6 |
