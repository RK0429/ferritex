# ADR-0001: ドメイン境界を持つ単一プロセス・モジュラーモノリスを採用する

## ステータス

提案

## コンテキスト

Ferritex は `REQ-NF-001` のフルコンパイル 1.0 秒未満、`REQ-NF-002` の差分コンパイル 100ms 未満、`REQ-NF-004` の LSP 低遅延、`REQ-NF-008` のクロスプラットフォーム再現性、`REQ-NF-009` の単一バイナリ配布を同時に満たす必要がある。

同時に、[domain_model.md](../domain_model.md) ではパーサー、タイプセッティング、差分コンパイル、アセット、グラフィック、PDF、フォント、開発者ツールの 8 コンテキストが明示されている。

## 検討した選択肢

### 選択肢 A: 技術レイヤ中心のプレーンモノリス

`controllers / services / repositories` のような構造で単一バイナリを構成する。

- 利点:
  - 初期実装が最も速い
  - バイナリ配布は容易
- 欠点:
  - `DocumentState` などの共有状態が肥大化しやすい
  - 境界が曖昧になり、差分コンパイルや LSP の変更が全域に波及しやすい

### 選択肢 B: ドメイン境界を持つモジュラーモノリス

単一プロセスを維持しつつ、runtime path を構成するトップレベルのレイヤ crate は `ferritex-cli` / `ferritex-application` / `ferritex-core` / `ferritex-infra` に分ける。repo 直下には benchmark / compatibility harness 用の `ferritex-bench` を追加してよいが、これは runtime layer crate には含めない。ドメイン境界はまず `ferritex-core` 内の module として表現し、安定した narrow interface を持つものだけ独立 crate に昇格させる。`kernel` は数値/寸法演算、stable ID、source span などの基底型だけを置く shared base module とし、package/class/bibliography semantics や I/O を入れない。接続は ports and adapters とする。

- 利点:
- IPC なしで低遅延を維持できる
- 単一バイナリ配布を維持できる
- crate と module の役割を分けて、過剰な分割を避けつつドメイン境界で変更コストを抑えられる
- 欠点:
  - 境界規律を CI で守る必要がある
  - コンテキスト分割の設計コストがかかる

### 選択肢 C: 複数プロセス / マイクロサービス

LSP、preview、compile、cache を別プロセスまたは別サービスに分ける。

- 利点:
  - 故障分離や独立デプロイはしやすい
  - 一部機能だけを独立スケールできる
- 欠点:
  - ローカル製品には過剰で、IPC / RPC が低遅延要件を阻害する
  - 単一バイナリ配布と相性が悪い
  - 決定的な job / session 状態共有が複雑になる

## 判断

選択肢 B を採用する。

## 根拠

- 性能要件が極めて厳しく、プロセス境界のオーバーヘッドを許容しにくい
- 単一バイナリ配布と 3 OS 対応を維持しやすい
- ドメインモデルに明確な境界が既に存在するため、技術レイヤ分割より自然である
- 将来、一部を別 crate / 別 process に抽出する余地を残しつつ、現時点では最小の複雑性で済む
- crate 昇格条件と依存方向を CI で検査すれば、境界規律を実装段階でも維持できる

## 帰結

### ポジティブ

- compile / watch / lsp / preview が同じ core と policy を共有できる
- IPC なしで `CompilationJob` / `CompilationSession` を扱える
- ドメイン境界ベースの保守性を確保できる

### ネガティブ

- 単一プロセス障害でその workspace の機能全体が停止する
- compile 時の crate 境界設計を誤ると逆に複雑化する

### リスク

- 実装が境界を破って `ferritex-core` に OS / network 依存を持ち込むと設計が崩れる
- `DocumentState` を共有万能モデルにするとモジュール分割が形骸化する

### 追加規律

- crate 昇格は stable API、独立 compile の利点、OS 依存や外部ライブラリ依存の局所化が揃った場合だけ許可する
- `kernel` は catch-all shared module にせず、基底型だけを置く
- CI は `ferritex-core` から `ferritex-application` / `ferritex-infra` への依存 0、循環依存 0 を強制する
