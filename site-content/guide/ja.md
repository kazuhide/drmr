---
title: ユーザーガイド
updated: 2026-10-24
version: 1.0.1
---

# 1. はじめに

drmr は映像・音声の符号化データを維持しながら、ファイルの格納形式、配信形式、暗号化方式を変更するツールです。復号に必要な鍵を持つ技術者が、管理下のオンデマンドコンテンツを解析・変換・検証する用途を想定しています。画質を変更するエンコーダーや DRM ライセンスを取得するクライアントの機能はありません。

このガイドでは現在のソースで使える操作と対応条件を説明します。配布版を使う場合は同梱のドキュメントも確認してください。リポジトリ内の関連ドキュメントは英語で記載しています。実行例が成功しても、すべてのメディアや再生端末での動作を確認したことにはなりません。

公開版の CLI は [GitHub Releases](https://github.com/kazuhide/drmr/releases) から入手できます。展開する前に、配布ファイルのハッシュを同じ版の `SHA256SUMS` と照合してください。外部のメディア処理ツールは別途必要です。配布対象と互換性の方針は[リリースの説明（英語）](https://github.com/kazuhide/drmr/blob/main/docs/releases.md)に記載しています。

利用する配布物の **Pre-release** 表示と説明を確認してください。ソースからのビルド手順は [CLI のインストール](#installation)に記載しています。CLI の互換性と各版の検証範囲は別に定めています。Web コンソールとデスクトップアプリは実験的な機能です。

## 目的別のコマンド

| 目的 | コマンド | わかること・実行すること |
| --- | --- | --- |
| 外部ツールを探す | `doctor` | 起動環境の PATH に必要な実行ファイルがあるか |
| ツールのバージョンと機能を確認する | `doctor --probe` | バージョンとヘルプに必要なオプションがあるか。メディア処理はしない |
| 入力の種類と暗号化方式の宣言を確認する | `analyze --metadata-only` | ローカルまたは HTTP(S) URL の MP4・DASH・HLS メタデータ。鍵は不要。コンテンツ全体の検証はしない |
| 入力データが対応条件を満たすか確認する | `analyze` | 原本全体の解析と入力条件の確認。URL 入力は指定先に保存する |
| 変換先の候補と条件を確認する | `plan` | 指定した形式・暗号方式の組み合わせと実行時に必要な条件 |
| 変換計画を保存する | `plan --save` | 原本のハッシュと出力条件を記録する。複数出力の場合は DRM の選択とマニフェスト検証方針も含む |
| コンテンツを変換する | `convert` | 入力データの条件を確認し、変換と照合を終えてから指定した場所に出力する |
| 原本と結果を改めて比べる | `verify` | 原本と結果を独立に読み、全サンプルの内容・設定・時刻・再生範囲を照合する。--decode で完全デコードも追加できる |
| 完成したマニフェストだけを調べる | `verify --manifest-only` | Rust の内蔵 Linter で HLS/DASH の宣言を検査する。--external-validator で外部検査も追加できる。メディアの比較や復号は行わない |

原本を確認してから変換計画を保存する場合は、通常の `analyze`、`plan --save`、`convert --plan`、`verify --plan` の順に使います。メタデータだけを確認するには `analyze` に `--metadata-only` を付けます。計画を保存せずに `convert INPUT` を実行することもできます。

これら四つのメディア処理は、既定で Rust による検査を行います。対象トラックの全サンプルまたは全アクセスユニット、コーデック設定、時刻、対応する再生範囲を読み取ります。`--decode` を付けると、コンテンツ全体のデコードも追加の必須条件になります。変換後の照合と独立検証では原本と実際の出力を別々に読み、以前の成功レポートだけでは判定しません。未対応の構造や不完全な証拠は、どちらの検査範囲でも拒否します。圧縮データの一致と、その全体をデコードできることは別の性質です。後者の証拠が必要な場合に `--decode` を使います。メタデータのみの解析、`--save` なしの計画、マニフェストのみの検査では指定できません。

## 最短の実行例

最初の動作確認には、対応条件を満たす非暗号化のローカル MP4 を使います。出力先の親ディレクトリをあらかじめ作成し、出力ファイルには既存のファイルと重複しない名前を指定してください。原本は変更されません。

```sh
drmr --version
drmr doctor --probe --route progressive-mp4
drmr analyze --metadata-only original.mp4 --json
drmr analyze original.mp4 --json
drmr plan original.mp4 --to clear --format mp4 \
  --output clear.mp4 --save clear.plan.json --json
drmr convert --plan clear.plan.json --json
drmr verify --plan clear.plan.json --json
```

入力が暗号化されている場合や出力を暗号化する場合は、第5章の手順でアクセス権を制限した鍵設定ファイルを渡します。この例でも、入力が対応範囲外なら処理は拒否されます。入力の確認や計画の保存に成功しても、その後の変換が成功するとは限りません。実行時に原本、鍵、ツール、指定した出力条件を改めて確認します。

主な操作手段は CLI です。依存ツールもまとめて実行したい場合は Docker を使えます。実験的な Web コンソールと Desktop では、同じ保存計画による解析・計画保存・変換・検証を操作できます。CLI の全コマンドを画面から実行する機能はありません。

# 2. 形式・暗号・DRM の読み方

## コンテナ形式と配信形式

| 用語 | 意味 |
| --- | --- |
| MP4 | 符号化済みの映像・音声と、その解釈に必要な情報を格納するコンテナ形式 |
| プログレッシブ MP4 | サンプルの位置と時刻をまとめた表を持つ MP4。通常の単一ファイル配布で使われる |
| fMP4 | fragmented MP4。初期化情報と、その後の小さな断片に分けた MP4。単一ファイルにも、DASH/HLS の分割配信にも使われる |
| MPEG-TS | MPEG transport stream。188 バイトのパケットを用いるコンテナ形式。このガイドでは TS と略す |
| DASH | Dynamic Adaptive Streaming over HTTP。MPD というマニフェストで初期化情報とメディア断片を参照する配信方式 |
| HLS | HTTP Live Streaming。プレイリストでメディアを参照する配信方式。名前に Live を含むが、drmr が扱うのは配信内容が確定したオンデマンドコンテンツ |
| セグメント | 配信のために分割されたメディアの一部分。映像と音声で境界や個数が一致するとは限らない |
| トラック・レンディション | トラックは映像や音声の系列。レンディションは選択できる画質や音声などの版 |

HLS は TS と fMP4 の両方を使えます。`--format hls` で fMP4 HLS、`--format hls-ts` で TS HLS を選択します。HLS という配信方式だけでは、コンテナ形式を特定できません。

## 暗号方式

AES は共通鍵暗号の名前です。CTR は counter mode、CBC は cipher block chaining の略で、どちらも AES の適用方法です。サンプルとは、符号化された映像の一枚や音声フレームなど、処理の単位になるデータです。

| `--to` | 暗号と適用単位 | 対応する出力 |
| --- | --- | --- |
| `clear` | 暗号化しない | 対応プロファイルの MP4、DASH、fMP4 HLS、TS HLS |
| `cenc` | MP4 サンプルに AES-CTR を適用する | MP4、DASH |
| `cbcs` | MP4 サンプルに AES-CBC を適用する。映像では暗号化するブロックと残すブロックのパターンを使う | MP4、DASH、fMP4 HLS |
| `hls-aes128` | HLS リソース全体に、パディングを伴う AES-CBC を適用する | fMP4 HLS、TS HLS |
| `hls-sample-aes` | TS 内の H.264・AAC フレームの定められた部分に AES-CBC を適用する | TS HLS |

部分サンプル暗号化は、コーデックのヘッダーなどを平文のまま残し、サンプルの一部を暗号化する方式です。CTR/CBC の選択や、暗号化するブロックとスキップするブロックのパターンとは別の区分です。`cenc` と記載されていても、ファイルの全バイトが暗号化されるわけではありません。

`cbcs` と HLS AES-128 は、どちらも CBC を使いますが、適用単位とメタデータが異なります。fMP4 HLS の CBCS と、FairPlay 向け TS の SAMPLE-AES も同じファイル形式ではありません。方式名を置き換えるだけでは変換できません。

これらの違いは W3C の MP4 暗号化説明、RFC 8216 の暗号化宣言、Apple の SAMPLE-AES 説明にも記載されています。一次資料のリンクは第18章にまとめています。

## DRM システムと暗号方式の違い

Widevine、PlayReady、FairPlay は、鍵の取得や利用条件、プレーヤーとの連携を含む DRM システムです。drmr は既知の鍵を使ってメディアを変換し、対応する経路では必要なシグナリングを格納します。シグナリングとは、プレーヤーが DRM システムや鍵の場所を判断するための付加情報です。

複数の出力を作る場合も、共通の `plan`、`convert`、`verify` を使います。出力一覧で、Widevine/PlayReady 向けの DASH、FairPlay 向けの HLS をそれぞれ指定できます。PSSH は Protection System Specific Header の略で、DRM の初期化情報を格納するデータです。鍵設定と出力一覧の具体例は第5・6章に示します。単一出力の PlayReady 設定も引き続き使えます。

暗号方式の変換、シグナリングの整合性確認、ライセンス取得、実機再生はそれぞれ別の確認事項です。メディアの検証に成功しても、対象機器でそのコンテナ形式・コーデック・暗号方式・DRM の組み合わせを再生できるとは限りません。サービスで配信する前に、実際に使うライセンスサーバーと機器で確認してください。

`cens` と `cbc1` は解析で識別できる場合がありますが、現在の変換先ではありません。対応表の「対象外」は drmr の現在の実装・検証範囲を表し、その形式で技術的に実現不可能という意味ではありません。

# 3. 入力と出力の対応表

メディア全体の入力確認は、実装した AVC（H.264）と、構造・設定を解析できる AAC の対応条件を満たす必要があります。HEVC・AV1・VP9・AC-3・E-AC-3・Opus・FLAC の設定をメタデータとして識別できても、全体の検証を実装したことにはなりません。未実装のコーデック条件は `native-codec-profile-not-supported` で拒否し、`--decode` でも迂回できません。コンテナ・暗号方式の対応表は、その形式に格納できる全コーデックの動作確認を意味しません。

## コンテナ形式別の対応表

この表は、配信内容が確定したオンデマンドコンテンツをローカルに保存し、汎用 `convert` で変換する場合の対応範囲です。各セルに示す暗号方式のほか、後述のコーデック・トラック・時刻・暗号データの配置条件を満たす必要があります。URL からの取得には別の制限があります。

| 入力 | MP4 出力 | DASH 出力 | fMP4 HLS 出力 | TS HLS 出力 |
| --- | --- | --- | --- | --- |
| MP4 | clear / cenc / cbcs。複数トラックは条件付き | clear / cenc / cbcs | clear / cbcs / AES-128 | 対象外 |
| DASH・fMP4 | clear / cenc / cbcs。単一トラックのみ | clear / cenc / cbcs | clear / cbcs / AES-128 | 対象外 |
| HLS・fMP4 | clear / cenc / cbcs。単一トラックのみ | clear / cenc / cbcs | clear / cbcs / AES-128 | 対象外 |
| HLS・TS、映像あり | 対象外 | clear / cenc / cbcs | clear / cbcs / AES-128 | clear / AES-128 / SAMPLE-AES |
| HLS・TS、音声のみ | 対象外 | clear / cenc / cbcs | clear / cbcs / AES-128 | clear / AES-128 / SAMPLE-AES |

MP4 に映像と音声をまとめた出力は、同じプログレッシブ MP4 の対応トラックを保持する、後述の有限音声範囲プロファイルに限ります。DASH/HLS の別々のトラックや異なるファイルを一つの MP4 に組み合わせる機能ではありません。

## HLS の入力暗号方式

| 入力のコンテナ形式 | 対応する暗号方式 | 必要な入力鍵 |
| --- | --- | --- |
| fMP4 | clear、cenc、cbcs、AES-128 | cenc/cbcs は KID ごと、AES-128 はプレイリストの正確な鍵 URI ごとの設定 |
| TS | clear、AES-128、SAMPLE-AES | 暗号化入力は鍵 URI ごとの設定。SAMPLE-AES は関連付けられた IV も必要 |

fMP4 メディアプレイリストは一つの音声または映像トラックを持つ必要があります。マスタープレイリストで別々の映像・音声プレイリストを関連付ける構成は扱えます。保持できない言語・選択情報や、混在した音声グループの関連付けは拒否します。

## 出力一覧で DRM を明示する場合

下表は `auto`・`stitch` の入力条件です。この経路では既存の DRM 初期化情報を含む入力を拒否します。`segments` では、対応する clear または既知の鍵を持つ暗号化 fMP4 から、下表の DRM を指定した fMP4 DASH/HLS を作成できます。TS からは TS FairPlay を作成できます。入力と出力のコンテナ形式は同じにします。

| 出力一覧の format と target | DRM システム | auto・stitch の入力 |
| --- | --- | --- |
| `dash`、`cbcs` | Widevine、PlayReady、または両方 | 対応する clear HLS/TS または clear MP4 |
| `hls`、`cbcs` | FairPlay | 対応する clear HLS/TS または clear MP4 |
| `hls-ts`、`hls-sample-aes` | FairPlay | 対応する clear HLS/TS |

`hls` のコンテナ形式は fMP4、`hls-ts` は TS です。DASH と組み合わせる HLS をどちらか一方にすることも、両方を出力することもできます。各出力には別々の名前と鍵設定を指定します。DRM と暗号方式の組み合わせは上表の範囲に限られます。

## MP4 の有限音声範囲

このガイドでいう「有限音声範囲」は、MP4 の編集情報で音声の再生開始位置と長さが指定されている状態を指します。別の入力形式の名前ではありません。エンコーダーの初期遅延や末尾のパディング（長さを調整するための余分なデータ）を再生から除外するために、一般的な MP4 でも使われます。

対応する入力は、一つの完全なプログレッシブ MP4 に AAC-LC 音声トラックを一つ含むものです。H.264 映像トラックも一つまで含められます。音声の格納識別子は `mp4a`、映像は `avc1` です。音声の編集情報は通常速度での連続した一範囲を指定し、開始位置が負でなく、長さを音声クロックの整数値に正確に換算できる必要があります。

| 入力の保護 | MP4 出力 | DASH 出力 | fMP4 HLS 出力 |
| --- | --- | --- | --- |
| clear | clear / cenc / cbcs | clear / cenc / cbcs | clear / cbcs / AES-128 |
| cenc | clear / cenc / cbcs | clear / cenc / cbcs | clear / cbcs / AES-128 |
| cbcs | clear / cenc / cbcs | clear / cenc / cbcs | clear / cbcs / AES-128 |

clear MP4 出力はプログレッシブのサンプル表を保持します。暗号化 MP4 出力はフラグメント化され、この条件を満たす映像・音声を一つのファイルに保持できます。DASH/HLS 出力は役割ごとのトラックに分けます。入力暗号化には各トラックの鍵が必要です。

複数の編集、先頭に空の編集を入れた遅延、速度変更、追加トラック、別ファイルの結合、鍵の切り替え、入力の DRM 初期化ヘッダーは、この変換プロファイルでは拒否します。cenc は検証済みの 8/16 バイトのサンプル IV、cbcs は検証済みの 16 バイト定数 IV とパターンなど、実装が確認できる配置に限ります。任意の暗号化 MP4 を受け入れるという意味ではありません。

## TS の入力条件

TS 入力の対象は配信内容が確定した HLS です。番組構成が一つに固定されている必要があります。映像はプログレッシブ H.264 を一つ、音声は 44.1 kHz または 48 kHz の AAC-LC を一つまで含められます。同じ条件なら、音声のみの入力から DASH、fMP4 HLS、TS を出力することもできます。

TS パケットは 188 バイトです。映像の復号設定を表す SPS/PPS は途中で変更できません。AAC は 7 バイトの ADTS ヘッダーを使い、チェックサムを持たず、一単位に音声フレームを一つ含む必要があります。サンプルレートとチャンネル構成も途中で変更できません。複数番組、設定変更、壊れたパケット、時刻カウンターの周回、不連続、インターレース映像、未対応の記述子などは拒否します。

音声のみのプレイリストでは、最終区間の `EXTINF` が最後の AAC フレームの途中で終わる場合もあります。宣言の精度から再生する音声サンプル数を一つに決められる場合は、符号化された最終フレームをすべて残し、指定された再生範囲を HLS・DASH 出力に引き継ぎます。精度不足で複数の範囲が当てはまる指定、途中区間の短縮、丸ごと一フレーム以上を除く指定は拒否します。別の音声プレイリストを映像に組み合わせることはできますが、映像・音声を多重化したプレイリストでは音声だけの短縮範囲を宣言できません。この編集形式は HLS・DASH のマニフェストと組み合わせて検証します。同じ編集を持つ単一 MP4 の入力・出力は、独立した検証経路をまだ実装していないため拒否します。

SAMPLE-AES 入力では暗号方式を途中で変更できません。鍵形式は `identity` または `com.apple.streamingkeydelivery` の一つで、指定する形式版は 1 です。異なる鍵形式の同時宣言は拒否します。非ゼロの音声初期遅延も対象外です。

TS 出力で映像と音声がある場合は、それぞれ別の TS プレイリストと、それを関連付けるマスタープレイリストを作ります。一つの TS セグメント内に両方を多重化する出力は、現在の実装では作りません。

## 共通の対象外と上限

ローカル DASH は静的な一つの Period、連続した有限の SegmentTimeline、対応する相対参照を持つ必要があります。ライブ、動的 DASH、複数 Period、バイト範囲、不連続、保持できないイベントやレンディション情報などは対象外です。字幕トラックや任意のトラック選択も、現在の変換対象に含みません。

汎用変換の上限は、作業用に保存するデータが一トラックあたり 16 GiB、トラック数が 64、DASH の参照セグメント数が 100,000 個です。このほか、初期化メタデータや断片ごとの上限があり、利用可能な CPU とメモリに応じても処理を制限します。上限を超えた部分を無視して成功とすることはありません。

参照（英語）: [変換対応範囲](https://github.com/kazuhide/drmr/blob/main/docs/conversion-support.md)、[処理・検証条件](https://github.com/kazuhide/drmr/blob/main/docs/media-processing.md)。

# 4. 導入と依存ツール

## ソースからの CLI のインストール

ソースからのビルドには Rust 1.95 以上が必要です。特定のバージョンに自動で切り替わることはなく、利用者が選んだツールチェーンを使います。1.99 などの記載は検証に使ったバージョンを示し、そのバージョンだけを必須とするものではありません。ビルド済みの CLI アーカイブを使う場合、Rust のインストールは不要です。

リポジトリのルートで実行します。

```sh
cargo install --path crates/drmr --locked
drmr --version
```

インストールせずビルドする場合:

```sh
cargo build --release --locked -p drmr
./target/release/drmr --help
```

`--locked` は Cargo.lock に記録した依存関係を使う指定です。Cargo.toml には依存ライブラリの基準バージョンと互換性の範囲を、Cargo.lock には検証した具体的な組み合わせを記録しています。依存ライブラリを更新した後も、最低要件の Rust と安定版 Rust の両方で CI の検査に通る必要があります。

## 外部メディアツール

| 実行ファイル | 最低要件 | 用途 |
| --- | --- | --- |
| `ffmpeg` / `ffprobe` | 9.0.1 | --decode による追加の完全デコード。FFmpeg は暗号化された有限プログレッシブ MP4 のコンテナコピーにも使用 |
| `packager` | Shaka Packager 3.8.0 | MP4 サンプル暗号化、格納形式の変更、セグメント化 |

必要な実行ファイルを信頼できる配布元から入手し、シェルがプログラムを検索する PATH に追加してください。CLI アーカイブと Desktop バンドルには、これらのメディアツールは含まれません。Docker イメージには含まれます。

最低要件より新しいバージョンでも、必要なオプションを備えていないビルドや、全パケットの時刻を出力できないビルドは使えません。Rust による通常の `analyze`・`plan --save`・`verify` には外部メディアツールは不要です。変換のパッケージングには Shaka Packager を使います。非暗号化の有限プログレッシブ MP4 は Rust でコピーし、暗号化入力の同経路では FFmpeg によるコンテナコピーを行います。これは再符号化や検証用のデコードではありません。

## doctor の使い分け

doctor の既存プロファイルは、変換に加えて `--decode` による完全デコードも含むツール構成を確認します。通常の Rust 解析・検証に必要な最低要件とは別です。メタデータ計画の `required_tools` は Rust 既定処理の最低要件を示し、追加ツールの条件は `condition_codes` と説明に記録します。

```sh
drmr doctor
drmr doctor --probe --route all --json
drmr doctor --probe --route dash-presentation
```

既定の `doctor` は実行ファイルの探索だけを行い、外部ツールを起動しません。`--probe` はバージョンとヘルプを、各呼び出し 3 秒、標準出力・標準エラー各 1 MiB の上限で確認します。生の出力はレポートに含めません。メディア、鍵、ライセンス、再生機器は検証しません。

`doctor` は Rust のメディア検査とは別に、外部ツールの準備を診断します。`--route` では、どの外部ツール構成を確認するかを選びます。`progressive-mp4` では FFmpeg と ffprobe、`all` では三つのツールすべてを確認します。他の処理経路では必要に応じて Shaka Packager を確認します。選んだ経路で使わないツールの検査は任意です。ただし、任意の検査でも起動したプロセスの終了を確認できない場合は、準備完了と判定しません。

レポートの `ready` は選んだ診断項目が通ったという意味です。`media_compatibility_checked` は false のままです。`path` は実際に解決した実行ファイルの場所です。GUI の起動環境とターミナルの PATH は異なることがあるため、Desktop 内の外部ツール確認も使ってください。

参照（英語）: [必要条件](https://github.com/kazuhide/drmr/blob/main/docs/requirements.md)、[Windows](https://github.com/kazuhide/drmr/blob/main/docs/windows.md)。

# 5. 鍵設定と秘密情報の扱い

## 入力鍵と出力鍵の役割

`convert` と `verify` は `--keys-file` で指定した JSON ファイルから鍵を読み込みます。ファイルには所有者だけがアクセスできる権限が必要です。環境変数から自動的に鍵を補うことはありません。入力鍵は原本の復号に、出力鍵は結果の暗号化と検証に使います。同じ暗号方式への変換でも、入力鍵と出力鍵は独立しています。

`analyze` と `plan --save` も、原本の確認に入力鍵が必要な場合は同じ `--keys-file` を使えます。計画を保存する段階では出力鍵は不要です。`convert --plan` と `verify --plan` に、実行時の鍵を別途渡します。

鍵は 16 バイトを 32 桁の十六進数で表します。KID は key identifier、つまり鍵を選ぶための識別子で、暗号鍵そのものではありません。IV は initialization vector、暗号化の開始状態を指定する値です。KID やメディア内の IV は公開メタデータになる場合があります。

macOS/Linux では、まず所有者だけがアクセスできるディレクトリを作成します。その中にエディターや秘密情報の管理ツールで設定ファイルを作ってください。次の例では、後述の構造例を `keys.json` に保存した後、ファイルの権限も設定します。

```sh
umask 077
mkdir config
chmod 700 config
```

```sh
chmod 600 config/keys.json
```

実行時には、設定が所有者だけがアクセスできる通常ファイルであり、ハードリンク数が一つであることなどを確認します。シンボリックリンク、他のユーザーが読める設定、重複キー、不明なフィールド、未対応の schema_version、サイズが上限を超える設定は拒否します。設定ファイルは出力ディレクトリとバージョン管理の対象から外してください。Windows では後述の所有者専用 ACL を使います。

## cenc / cbcs の設定例

以下は構造例で、そのまま使える鍵ではありません。山括弧を含む値を、ご自身の 32 桁の十六進値に置き換えてください。clear 入力から CBCS DASH を作る最小構成です。

```json
{
  "schema_version": 1,
  "output": {
    "kid": "<32-hex-output-kid>",
    "key": "<32-hex-output-key>"
  }
}
```

暗号化 MP4/DASH/fMP4 HLS 入力には、必要な各トラックの KID と鍵を追加します。映像と音声で KID が異なる場合は両方を指定してください。

```json
{
  "schema_version": 1,
  "input": {
    "keys": [
      {"kid": "<32-hex-video-kid>", "key": "<32-hex-video-key>"},
      {"kid": "<32-hex-audio-kid>", "key": "<32-hex-audio-key>"}
    ]
  },
  "output": {
    "kid": "<32-hex-output-kid>",
    "key": "<32-hex-output-key>"
  }
}
```

clear 出力では `output` 全体を省きます。CBCS HLS 出力では `output.hls_key_uri` も必要です。その URI はプレイリストに書く値であり、drmr が鍵ファイルを作成・公開する場所ではありません。

## HLS AES-128 と SAMPLE-AES

HLS の鍵 URI は、入力プレイリストの値と正確に対応させます。drmr はこの URI から鍵を取得しません。鍵は運用者が設定します。SAMPLE-AES は、その鍵に関連付けられた IV がプレイリストにない場合に設定が必要です。両方にある場合は一致しなければなりません。

```json
{
  "schema_version": 1,
  "input": {
    "hls_keys": [{
      "uri": "skd://source-content",
      "key": "<32-hex-source-key>",
      "iv": "<32-hex-associated-iv>"
    }]
  }
}
```

上の `iv` は SAMPLE-AES の例です。SAMPLE-AES ではセグメント番号を IV の代わりにしません。AES-128 入力ではプレイリストの明示 IV または仕様に従うシーケンス番号由来 IV を扱います。

AES-128 出力は `output.key` と `output.hls_key_uri` を使い、`output.kid` と `output.iv` は指定しません。各出力リソースで新しい IV を作ります。

```json
{
  "schema_version": 1,
  "output": {
    "key": "<32-hex-output-key>",
    "hls_key_uri": "https://keys.example.invalid/content.key"
  }
}
```

FairPlay 向け TS SAMPLE-AES 出力には、関連付けられた明示 IV も必要です。現在のプロファイルでは映像・音声に同じ出力鍵と IV を使います。

```json
{
  "schema_version": 1,
  "output": {
    "key": "<32-hex-output-key>",
    "iv": "<32-hex-associated-iv>",
    "hls_key_uri": "skd://output-content"
  }
}
```

入力と出力のプレイリストに同じ URI を記載する場合でも、それぞれの鍵と IV の役割は異なります。`output.iv` は SAMPLE-AES TS 専用であり、clear、MP4 サンプル暗号化、AES-128 出力では拒否します。

## 複数出力の鍵と DRM 情報

出力一覧で鍵や DRM 情報が必要な場合は、`schema_version: 2` の所有者専用鍵ファイルを使います。共有の入力鍵は `input`、出力ごとの鍵と付加情報は `outputs` に記録します。`outputs` の名前は出力一覧の ID と対応させ、暗号化する各出力に一つの設定を用意します。clear 出力には設定を置きません。余分な ID や不足した ID は拒否します。

次は第6章の `dash` と `hls` に対応する構造例です。山括弧の値を正しい鍵・識別情報に置き換え、例えば `config/multi-keys.json` に保存してください。

```json
{
  "schema_version": 2,
  "outputs": {
    "dash": {
      "kid": "<32-hex-dash-kid>",
      "key": "<32-hex-dash-key>",
      "signaling": {
        "system": "multi-drm",
        "widevine_pssh": "<complete Widevine PSSH box in hex or base64>",
        "playready_license_url": "https://license.example.invalid/playready"
      }
    },
    "hls": {
      "kid": "<32-hex-hls-kid>",
      "key": "<32-hex-hls-key>",
      "hls_key_uri": "skd://<content-identifier>",
      "signaling": {
        "system": "multi-drm",
        "fairplay": true
      }
    }
  }
}
```

Widevine の PSSH は、DASH 出力の `kid` に合った完全な PSSH box を十六進または Base64 で渡します。drmr は内容を検査し、PlayReady の情報は指定した KID とライセンス URL から生成します。FairPlay は明示的な選択と `hls_key_uri` からプレイリストの鍵宣言を生成します。実行時に選ぶ DRM の集合が計画と一致しなければ拒否します。ライセンスサービスやプレイリストに書かれた鍵 URI には接続しません。

FairPlay の TS 出力を `hls-ts` と名付ける場合は、`outputs` の HLS 用メンバーを次の形に置き換えます。

```json
{
  "hls-ts": {
    "key": "<32-hex-ts-key>",
    "iv": "<32-hex-associated-iv>",
    "hls_key_uri": "skd://<content-identifier>",
    "signaling": {
      "system": "multi-drm",
      "fairplay": true
    }
  }
}
```

TS の SAMPLE-AES では、鍵に対応する IV を実行時の鍵設定に明示します。MP4 用の KID は指定しません。プレイリストの `IV` 属性が省略されていても、設定で渡した IV を使ってメディアを検証します。プレイリストにも IV がある場合は、両方の値が一致する必要があります。fMP4 FairPlay では、初期化セグメントのトラック暗号化情報 `tenc` に必要な定数 IV が含まれていれば、プレイリストの IV 属性を省略できます。IV 属性の省略だけでは、暗号化されているかどうかを判定しません。

ファイルの版番号は役割ごとに異なります。出力一覧は version 1・2（出力別の処理方式を指定）、鍵ファイルは 1・2、保存計画は単一出力が 5、名前付き出力が 6 です。旧計画も元の検査条件を維持して読み込めます。

## ログ、メモリ、一時ファイル

| 情報・場所 | 実装上の扱いと限界 |
| --- | --- |
| 正常な結果・ジョブ履歴 | コンテンツキーを含めない。KID は公開の識別情報として扱う |
| 診断に含まれる既知の秘密情報 | 認識した鍵・設定 IV・シグナリングと、それらの十六進、Base64、エスケープ表現などをマスクする |
| パス・名前・メディアタグ | 一律に秘密とはみなさない。成功レポートや一部診断に表示される |
| 外部プロセスの引数 | FFmpeg や Shaka に平文の鍵を渡す経路がある。OS のプロセス情報から見える可能性がある |
| メモリ | 通常のプロセスメモリに平文が載る。一部の Rust バッファは破棄時に消去するが、全コピーの消去は保証しない |
| 作業用ディスク | 復号済みメディアや平文鍵ファイルを所有者専用の一時領域に書く経路がある。メモリ内だけの処理ではない |
| 通常終了後 | drmr が作成した一時ファイルを削除する。強制終了した場合や子プロセスの終了を確認できない場合は残ることがある |

一部のバッファを消去しても、Secure Enclave などの隔離環境で処理するわけではありません。外部プロセス、メモリ管理機構が作るコピー、スワップ、クラッシュダンプ、バックアップまで一括で消去する保証はありません。ファイルを削除しても、ストレージやスナップショットからデータが完全に消えるとは限りません。

ライセンス URL、HLS 鍵 URI、PSSH に含めた URL は、意図して出力メディアへ書かれる場合があります。Base64 や十六進表現は暗号化ではありません。公開する予定のない認証トークンをこれらの値へ含めないでください。入力 URL、原本プレイリスト、保存したダウンロードにも署名や認証情報が含まれることがあります。

信頼できる OS アカウント、外部メディアツール、ブラウザーを使い、作業ディレクトリも鍵と同様に保護してください。ログを共有する前に、マスクの対象外となる名前や外部ツールの出力も確認してください。Rust API が返す外部ツールの出力バッファには秘密情報が残る可能性があるため、そのままログに記録しないでください。

参照（英語）: [鍵とローカルセキュリティ](https://github.com/kazuhide/drmr/blob/main/docs/security.md)。

# 6. メタデータの確認と plan

## 鍵を使わずに宣言情報を確認する

```sh
drmr analyze --metadata-only media.mp4
drmr analyze --metadata-only source/manifest.mpd --json
drmr analyze --metadata-only source/master.m3u8 --json
drmr analyze --metadata-only "https://example.com/vod/master.m3u8" --json
drmr analyze --metadata-only "https://example.com/vod/manifest.mpd" --json
drmr analyze --metadata-only "https://example.com/vod/media.mp4" --json
```

`analyze --metadata-only` は MP4 のメタデータや DASH/HLS の宣言を読み取ります。鍵、デコーダー、Shaka Packager は不要です。入力形式は内容から判定します。JSON は `schema_version: 1` を持ち、形式、トラック・サンプル記述ごとの保護方式、既知の KID、DRM システム名、`findings` の説明トークンなどを返します。鍵、IV、ライセンス URL、鍵 URL は含めません。

MP4 では構造情報を読み、メディアのデータ全体は走査しません。DASH/HLS では宣言情報を読みます。いずれも `media_verified: false` であり、全サンプルの復号・デコードや再生を確認するものではありません。未対応または不完全な暗号情報は `unknown` のままです。HLS の SAMPLE-AES 宣言だけで MP4 の暗号方式を断定することはありません。

ローカル HLS の子プレイリストは、入力ディレクトリ配下の参照だけをたどります。リモート、欠落、循環、ツリー外参照は解決しません。URL の HLS では宣言と必要な子プレイリスト、DASH では MPD を取得します。初期化ファイル、セグメント、鍵、ライセンスは取得しません。

URL の MP4 は HTTP の Range リクエストで必要な位置だけを読みます。応答の範囲を確認し、box（MP4 を構成するデータの単位）の見出しと、再生構造を記録した `moov` を取得します。`moov` がファイル末尾にあっても、映像・音声のデータを格納する `mdat` は読み飛ばします。Range 指定を無視する配信元では、ファイル全体を取得せず拒否します。

URL のメタデータは非公開の一時データとして扱い、保存計画やオフライン変換の原本にはしません。`--download-dir` は不要です。ライブ HLS や dynamic DASH の宣言も読める場合がありますが、更新を追い続けたり、VOD が完結していることや変換可能なことを確認したりする機能ではありません。原本全体の取得と検査には、第8章の通常の URL 解析を使います。

HLS のマスタープレイリストは、画質や音声などの選択肢を列挙し、それぞれのメディアプレイリストを参照します（[RFC 8216 第4.3.4.2節](#references)）。ローカル・URL のどちらでも、参照先が別のマスタープレイリストだった場合は、その先をたどらず `hls-nested-master-playlist` で拒否します。参照を何段もたどることで解析結果が増幅し、メモリを過剰に使うことも防ぎます。

`--metadata-only` と `--download-dir`、`--keys-file`、`--packager-bin`、`--max-workers`、`--measure` は併用できません。URL には第8章の `--fetch-policy` を指定できますが、ローカル入力には指定できません。メタデータ解析と取得ポリシーの両方の上限を適用します。解析の主な上限は、一マニフェスト 4 MiB、合計 16 MiB、32 プレイリスト、128 参照、トラックごとに 64 KID、MP4 の `moov` 情報 16 MiB です。MP4 はローカル入力・URL 入力とも、入れ子を含む解析対象の box を 16,384 個までに制限します。小さな box が多数あると、データ量以上に解析用メモリが必要になるためです。上限に達した後は新たな box の管理領域を確保せず、`mp4-box-count-limit` で拒否します。

プレリリース中の CLI 整理に伴い、従来の `inspect INPUT` は削除しました。`analyze --metadata-only INPUT` に置き換えてください。旧名の別名コマンドは残していません。[移行の説明（英語）](https://github.com/kazuhide/drmr/blob/main/docs/releases.md#pre-release-cli-consolidation)も確認してください。

## analyze による入力データ全体の確認

```sh
drmr analyze source/master.m3u8 --json
drmr analyze encrypted.mp4 --keys-file config/keys.json --json
```

`--metadata-only` を付けない `analyze` は、変換と同じ処理エンジンで原本全体を読み、全サンプルの符号化データ、コーデック設定、時刻、対応する再生範囲を Rust で確認します。`--decode` を付けると、原本全体のデコードも必須になります。`input_admitted: true` は、確認した入力が対応条件を満たしていることを示します。出力形式の候補が表示されても、指定する暗号方式との組み合わせは計画時に確認します。変換結果や DRM 再生の動作確認を済ませたという意味ではありません。ローカル MP4・DASH・HLS が対象です。URL 入力の保存手順は第8章に示します。

入力の確認では、同じトラック一式を複製して照合する処理を省き、各トラックを直接検査します。MPEG-TS では全パケットの内容と時刻を読み、元の TS パケットから得た証拠との一致、対応する AVC の全スライスヘッダーと時刻設定を確認します。`--decode` はコンテンツ全体のデコードを追加します。プレイリストが参照する画質別の映像と音声はすべて対象です。別コマンドの `verify` では、引き続き原本と出力をそれぞれ読み取って比較します。入力確認の結果を使って、後のコマンドの再検査を省くことはありません。

入力に互換処理を適用した場合は、`compatibility_notes` に固定の識別コードを示します。例えば、実データを含まない TS のパディングや、実際の区間を持たない隣接した HLS の広告境界タグが対象です。原本のバイト列は保持し、メディア内容と時刻の検査も行います。受理する条件と処理方法は、[TS の互換処理の説明（英語）](https://github.com/kazuhide/drmr/blob/main/docs/media-processing.md#transport-stream-compatibility)に記載しています。

## plan による変換条件の確認と保存

```sh
drmr plan media.mp4 --to cbcs --format dash --json
drmr plan source/master.m3u8 --to clear --format dash,hls
```

`--save` を付けない `plan INPUT` は、入力の宣言情報を使って変換先の候補と必要な条件を確認するモードです。`--to` は必須で、`--format` を省略すると入力形式を引き継ぎます。このモードでは、カンマで区切って指定した形式をそれぞれ変換候補として示します。複数の出力を実際にまとめて作るには、後述の `--outputs` を使います。

`compatible` は宣言と指定の組み合わせが整い、すべての要求形式に候補経路があることを表します。`execution_available` には、出力形式が一つであることも必要です。複数形式の計画は成功しても `execution_available: false` になり得ます。

どちらも、入力データが対応条件を満たすこと、鍵が正しいこと、必要なツールがあること、実行時の処理方法が決まったことまでは保証しません。`source-profile-validation-required` などの条件は実行時に確認します。SAMPLE-AES の宣言が正しくても、実際の TS 構成や IV の検証は必要です。このメタデータ確認用の JSON を、実行用の計画ファイルとして `convert --plan` に渡すことはできません。

`details` がある場合は、宣言された暗号、暗号化単位、パターン、未確定の範囲を示します。変換成功レポートの `processing` は実際に選んだ経路です。計画の候補と区別してください。

単一出力の実行用計画は、新規の出力先と `--save` を指定して保存します。原本全体を確認し、解決後の出力形式を一つ記録します。

```sh
drmr plan source/input.m3u8 --to cbcs --format dash \
  --output converted-dash --save conversion.plan.json --json
drmr convert --plan conversion.plan.json --keys-file config/keys.json --json
drmr verify --plan conversion.plan.json --keys-file config/keys.json --json
```

計画には、変換が参照するローカルのプレイリストとメディアファイル、それぞれのサイズと SHA-256 ハッシュ、出力形式、暗号方式、出力先の絶対パスを記録します。SHA-256 は内容の変更を確認するための値です。記録するパスは保存時のマシンと配置に依存します。計画ファイルも所有者だけがアクセスできるようにし、原本と出力のディレクトリの外に保管してください。未対応のバージョン、不明なフィールド、計画識別子の不整合、原本の変更、安全でない保存先は拒否します。識別子は整合性の確認用であり、第三者による署名や実行許可を表すものではありません。

新しい単一出力計画は schema 5、名前付き出力計画は schema 6 です。要求した処理方式と選択結果、境界を保持するかどうかも固定します。`required_depth` を計画の ID に含め、既定では native、`plan --save --decode` を付けると decoded を記録します。後の `convert --decode`・`verify --decode` で最低条件を強めることはできますが、下げることはできません。旧単一出力の version 1 と名前付き出力の version 2 は、完全デコードを最低条件として保持します。version 3・4 は保存済みの検査水準と従来の結合処理を維持します。

鍵、署名付き入力 URL、認証ヘッダー、ライセンス URL、HLS 鍵 URI、実行プログラムの引数は計画に保存しません。鍵は実行・検証時に `--keys-file` で渡します。version 1・3・5 の単一出力計画では、DRM シグナリングの選択も実行時の設定です。version 2・4・6 では DRM システムの選択とマニフェスト検証方針を計画に固定し、鍵やアドレスは引き続き別の設定で渡します。`verify --plan` は原本のハッシュと実際の出力の形式・暗号方式を照合し、原本と結果のコンテンツ全体および時刻が一致するか確認します。ライセンス取得や実機再生は別途検証が必要です。

Unix 系では、出力と計画の親ディレクトリは、自分が所有し、他のユーザーが書き換えられない信頼できる場所を使ってください。保存する計画の出力先が安全でない場合は、メディアの確認に入る前に拒否します。共有一時ディレクトリを扱う条件と削除時の制約は、[作業領域の権限の説明（英語）](https://github.com/kazuhide/drmr/blob/main/docs/security.md#temporary-files-and-cleanup)に記載しています。

計画保存では変換結果を作りません。暗号化された原本の確認には入力鍵を使った復号を伴う場合があり、実行時にも原本の条件を再確認します。

## 複数出力を一つの計画に保存する

複数の出力をまとめて作る場合は `--outputs` を使います。次の出力一覧を `outputs.json` に保存します。このファイルには秘密情報を含める必要はありません。

```json
{
  "schema_version": 1,
  "outputs": [
    {
      "id": "dash",
      "format": "dash",
      "target": "cbcs",
      "drms": ["widevine", "playready"],
      "conformance": "required"
    },
    {
      "id": "hls",
      "format": "hls",
      "target": "cbcs",
      "drms": ["fairplay"],
      "conformance": "required"
    }
  ]
}
```

FairPlay HLS を TS にする場合は、HLS の要素を `id: "hls-ts"`、`format: "hls-ts"`、`target: "hls-sample-aes"`、`drms: ["fairplay"]` に変更し、第5章の TS 用鍵設定と名前を対応させます。

出力は最大八つです。ID は一文字目を英小文字とし、続きは英小文字・数字・ハイフンを使った 1〜32 文字にします。予約されたデバイス名、重複 ID、不明なフィールド、任意の子パスは拒否します。出力一覧に鍵や DRM 初期化データは含めません。秘密の鍵ファイルとは異なり、通常の読み取り可能な JSON として保存できます。

```sh
drmr plan clear-ts/master.m3u8 --outputs outputs.json \
  --output result --save multi.plan.json --json
```

出力一覧の version 2 は出力ごとの `processing` を指定でき、version 1 では拒否します。この version 6 の計画は、一つの原本ファイル集合と、各出力の形式・暗号方式・DRM 選択・外部マニフェスト検証方針を記録します。原本のハッシュと出力ルートも固定します。`plan` の `--outputs` には `--save` が必要で、`--to`・`--format` とは併用しません。

`conformance: "required"` を指定すると、変換時に加え、後から `verify --plan` を実行する際にも外部マニフェスト検査が必須になります。`"none"` を指定するかフィールドを省略すると、外部検査は要求しません。どちらの場合も内蔵 Linter の検査とコンテンツ全体の検証を行います。第9章のツールを実行前に準備してください。計画の保存に成功しても、そのツールが使えることまでは確認できません。



参照（英語）: [処理経路と解析で確認する内容](https://github.com/kazuhide/drmr/blob/main/docs/processing-routes.md)。

# 7. ローカル変換の実例

## 共通の実行規則

`convert INPUT --to ... --format ... --output ...` は、ローカルの MP4、DASH、HLS を内容から判定します。一回の実行では出力形式を一つ選びます。`--format` を省略すると入力形式を引き継ぐため、TS と fMP4 HLS の違いが重要な場合は明示してください。

入力形式としての HLS は、TS と fMP4 を区別しません。TS HLS を入力した場合も、省略時の出力は hls、つまり fMP4 HLS です。TS で出力するには `--format hls-ts` を明示します。

出力先には、新しい MP4 ファイルのパスか、新しい DASH/HLS ディレクトリのパスを指定します。親ディレクトリはあらかじめ作成してください。既存の出力は上書きしません。入力を専用の作業領域に保存し、内容・時刻・原本の整合性と停止要求の有無を確認してから、完成した結果を出力先に配置します。このガイドでいう出力の「公開」は、このローカル配置を指します。インターネットへのアップロードは行いません。

## 複数出力の変換計画の実行

第5章の version 2 の鍵ファイルと、第6章で保存した複数出力計画を使います。

```sh
drmr convert --plan multi.plan.json \
  --keys-file config/multi-keys.json --json
drmr verify --plan multi.plan.json \
  --keys-file config/multi-keys.json --json
```

この例では `result/dash/manifest.mpd` と `result/hls/master.m3u8` を作り、それぞれの初期化情報、セグメント、子プレイリストを同じ出力ディレクトリ内に置きます。`hls-ts` を要求した場合は、その出力用の TS リソースを作ります。DASH と fMP4 HLS を含め、各出力は別々にパッケージするため、実メディアファイルを共有する構成ではありません。

すべての結果を所有者専用の作業領域で作成します。各出力のメディアと指定した DRM 情報を確認し、必須のマニフェスト検査にも成功してから、出力ルート全体を一回のファイルシステム操作で配置します。失敗やキャンセルがあった場合に、一部の出力だけを完成結果として残すことはありません。既存出力は上書きせず、保存した原本も保持します。

計画ファイルを保存せず、一回のローカル変換として実行することもできます。

```sh
drmr convert clear-ts/master.m3u8 --outputs outputs.json \
  --output result --keys-file config/multi-keys.json --json
```

後から `verify --plan` で原本のハッシュや出力一覧まで再確認する場合は、計画を保存する方法を使ってください。以下の単一出力の例も引き続き使えます。

以下の例では、対応条件を満たす入力と必要な鍵設定を準備済みとします。鍵設定のファイル名は一例であり、拡張子以外に命名の決まりはありません。

## MP4 から MP4 または DASH への変換

```sh
drmr convert original.mp4 --to cenc --format mp4 \
  --output encrypted.mp4 --keys-file config/output.json
drmr verify encrypted.mp4 --source original.mp4 \
  --keys-file config/output.json --json
```

```sh
drmr convert original.mp4 --to cbcs --format dash \
  --output result-dash --keys-file config/output.json
drmr verify result-dash/manifest.mpd --source original.mp4 \
  --keys-file config/output.json --json
```

音声の有限再生範囲を持つ MP4 も、対応条件を満たせば同じ `convert` で処理できます。専用の「音声編集 MP4」入力コマンドはありません。処理の都合で再生範囲を短くしたり、最後の音声パケットを捨てたりすることはありません。

## 暗号化 DASH の復号と暗号方式の変更

clear 出力には入力側の鍵だけを持つ設定を使います。

```sh
drmr convert protected/manifest.mpd --to clear --format dash \
  --output clear-dash --keys-file config/source.json
drmr verify clear-dash/manifest.mpd \
  --source protected/manifest.mpd --keys-file config/source.json --json
```

別の出力鍵を使って CBCS に変える場合は、入力鍵と出力鍵の両方を記載した設定を使います。

```sh
drmr convert cenc-source/manifest.mpd --to cbcs --format dash \
  --output cbcs-result --keys-file config/both.json
drmr verify cbcs-result/manifest.mpd \
  --source cenc-source/manifest.mpd --keys-file config/both.json --json
```

## clear HLS から fMP4 HLS への変換

```sh
drmr convert source/master.m3u8 --to cbcs --format hls \
  --output cbcs-hls --keys-file config/hls-output.json
drmr verify cbcs-hls/master.m3u8 --source source/master.m3u8 \
  --keys-file config/hls-output.json --json
```

CBCS HLS には `output.hls_key_uri` が必要です。AES-128 にする場合は `--to hls-aes128` と対応する出力設定を使います。鍵 URI のサーバー運用や FairPlay のライセンス提供は drmr の外で行います。

## TS HLS から非暗号化または暗号化 DASH への変換

```sh
drmr convert ts-source/media.m3u8 --to clear --format dash \
  --output clear-dash
drmr verify clear-dash/manifest.mpd --source ts-source/media.m3u8 --json
```

```sh
drmr convert ts-source/media.m3u8 --to cbcs --format dash \
  --output protected-dash --keys-file config/output.json
drmr verify protected-dash/manifest.mpd \
  --source ts-source/media.m3u8 --keys-file config/output.json --json
```

上記は clear TS を入力にする例です。AES-128 / SAMPLE-AES 入力を使う場合は、それぞれの入力鍵と必要な IV を `input.hls_keys` に追加します。DASH 出力では `--to cenc` も指定できます。コンテンツの暗号化と、特定の DRM 用のライセンス情報の設定は別の操作です。

## 音声のみの TS から fMP4 HLS への変換

完了済みの音声のみの TS プレイリストから、clear、CBCS、AES-128 の fMP4 HLS を作れます。CBCS には `output.hls_key_uri` を持つ出力鍵設定を使います。暗号化入力には、それとは別に入力鍵 URI の対応付けも必要です。

```sh
drmr convert audio-ts/master.m3u8 --to cbcs --format hls \
  --output audio-fmp4-hls --keys-file config/hls-output.json
drmr verify audio-fmp4-hls/master.m3u8 \
  --source audio-ts/master.m3u8 --keys-file config/hls-output.json --json
```

音声トラック全体を分離してから、Shaka Packager で新しいセグメントを作ります。44.1 kHz の AAC は Rust で読み、原本の 90 kHz の時刻と実際の音声レートを保持した作業用 fMP4 を用意します。48 kHz の経路では Shaka Packager でトラックを分離します。Rust が全サンプル・設定・時刻を検査し、`--decode` を指定すると FFmpeg/FFprobe による全体のデコードと照合を追加します。原本の PES の順序、全 AAC フレーム、絶対時刻での PTS・DTS を維持し、照合の基準にします。二秒のセグメント目標に合わせて AAC フレームを切り落とすことはしません。従来の 48 kHz の回帰試験に使う TS 入力は 470 フレームで、一チャンネルあたり 481,280 サンプルを保持します。再生範囲を 480,000 サンプルに指定したプログレッシブ MP4 原本とは、再生する範囲が異なります。

## FairPlay 向け SAMPLE-AES TS の生成と復号

```sh
drmr convert clear-ts/media.m3u8 --to hls-sample-aes --format hls-ts \
  --output fairplay-ts --keys-file config/sample-aes-output.json
drmr verify fairplay-ts/master.m3u8 --source clear-ts/media.m3u8 \
  --keys-file config/sample-aes-output.json --json
```

対応する SAMPLE-AES TS を復号して clear にする場合は、次のように指定します。

```sh
drmr convert encrypted-ts/media.m3u8 --to clear --format hls-ts \
  --output clear-ts --keys-file config/sample-aes-source.json
drmr verify clear-ts/master.m3u8 --source encrypted-ts/media.m3u8 \
  --keys-file config/sample-aes-source.json --json
```

SAMPLE-AES TS は入力・出力の両方に対応しています。これらは既知の鍵でのメディア変換で、FairPlay のライセンス取得を行いません。

## PlayReady の明示シグナリング

`auto`・`stitch` では、対応する clear MP4 から CBCS DASH を作る場合に使えます。`segments` の入力条件は第3章を参照してください。次の signaling を持つ出力設定を使い、鍵はご自身の値へ置き換えてください。

```json
{
  "schema_version": 1,
  "output": {
    "kid": "<32-hex-output-kid>",
    "key": "<32-hex-output-key>",
    "signaling": {
      "system": "playready",
      "license_url": "https://license.example.invalid/acquire"
    }
  }
}
```

```sh
drmr convert qualified-clear.mp4 --to cbcs --format dash \
  --output playready-dash --keys-file config/playready.json
```

この機能は、一つの完全なファイルに含まれる H.264 映像と、存在する場合は AAC 音声を対象にする限定プロファイルです。有限 AAC の変換対応表にあるすべての組み合わせに、シグナリングを追加できるわけではありません。既存の PSSH・保護情報や、保持できることを検証していない解釈不能なメタデータは拒否します。詳細な入力条件は公開の対応表を確認してください。

ライセンス URL は絶対 HTTP(S) で、認証情報・フラグメント・空白・制御文字なし、UTF-8 で 4096 バイト以内です。指定した文字列をそのまま PSSH/PRO と MPD へ含め、URL へのリクエストは行いません。すべてのトラックの実初期化情報と MPD のシグナリングを照合します。検証結果の `license_playback_verified` は false のままです。

# 8. URL 入力の取得と変換

## 原本の取得から計画・変換・検証までの手順

次の例は、Clear HLS を取得して CBCS の DASH に変換する手順です。第5章の `config/keys.json` には出力鍵だけを設定します。`&&` でつなぐと、成功した場合だけ次のコマンドに進み、失敗した時点で止まります。取得後の `--metadata-only` は宣言内容の概要を確認する操作で、不要なら省略できます。

```sh
drmr analyze "https://example.com/vod/master.m3u8" \
  --download-dir ./saved-source &&
drmr analyze --metadata-only ./saved-source/input.m3u8 &&
drmr plan ./saved-source/input.m3u8 --to cbcs --format dash \
  --output ./converted-dash --save ./conversion.plan.json &&
drmr convert --plan ./conversion.plan.json --keys-file config/keys.json &&
drmr verify --plan ./conversion.plan.json --keys-file config/keys.json
```

原本全体を取得する URL 解析では、`--download-dir`、計画ファイル、出力先に互いに別の新しいパスを指定します。親ディレクトリは既存のものを使います。既存ファイルの上書きや取得先の自動再利用はしません。ローカル入力では `--download-dir` や `--fetch-policy` は不要です。

MP4、static DASH、配信が完結した HLS を URL から取得できます。形式は取得した内容で判別するため、URL に拡張子がなくても利用できます。クエリの有無や、許可されたリダイレクトの有無で形式が決まることはありません。

| URL の入力形式 | 保存後に使うローカルファイル | 取得できる構成 |
| --- | --- | --- |
| MP4 | `saved-source/input.mp4` | ローカル入力の対応条件を満たす一つの完全な MP4 |
| DASH | `saved-source/input.mpd` | 一つの `Period` を持ち、`SegmentTemplate` と有限の `SegmentTimeline` が明示された static MPD |
| HLS | `saved-source/input.m3u8` | 対応条件を満たす、配信が完結した TS/fMP4 プレイリスト |

DASH または MP4 では、上のローカル入力パスを表に合わせます。DASH は参照する初期化ファイルとセグメントをすべて保存し、`input.mpd` の参照をローカルに書き換えます。元の MPD は別に残し、暗号化情報、DRM 初期化データの PSSH、時系列情報を保持します。

取得後の解析には、ローカル入力と同じコーデック・トラック・暗号・時刻の条件を適用します。ライブ、バイト範囲による参照、複数の DASH Period、`SegmentList`、`SegmentBase`、`duration` だけで区間を表す DASH テンプレート、繰り返し回数が負の時系列指定は現在の取得機能の対象外です。これらは実装上の制限で、形式として変換が不可能という意味ではありません。

HLS は clear に加え、AES-128 の TS/fMP4、SAMPLE-AES の TS、CENC/CBCS の fMP4 に対応します。MP4 と DASH も、ローカル入力で対応する clear・暗号化済みデータを取得できます。

AES-128 と TS SAMPLE-AES の `input.hls_keys[].uri` は、リダイレクト後のプレイリスト URL から解決した絶対 URI と一致させます。`https://example.com/vod/media.m3u8` 内の `keys/key.bin` は `https://example.com/vod/keys/key.bin`、絶対 URI の `skd:` はそのまま使います。CENC/CBCS は `input.keys` に KID と鍵を指定します。鍵 URI やライセンスには接続しません。暗号化入力では、通常の `analyze` と `plan --save` による原本の確認にも入力鍵の `--keys-file config/keys.json` が必要です。Clear 入力とメタデータだけの確認では不要です。

HLS は元のプレイリストとリソースを保存し、メディア参照と鍵識別 URI をローカル用に解決した `saved-source/input.m3u8` を作ります。時刻・シーケンス番号・音声の関連付けは作り直しません。どの形式でも取得中は暗号化済みデータを復号しません。原本マニフェストは元のバイト列で別に残り、署名付き URL を含み得るため非公開で保管してください。

この手順で配信元へ接続するのは、最初の通常の `analyze` だけです。取得が完了した原本は後の処理が失敗しても残り、未完了の取得は受け付けません。`convert URL --download-dir NEW_DIR` なら計画保存なしで取得・変換できます。

## URL 入力からの複数 DRM 出力

入力が対応する clear TS HLS の URL であれば、第6章の `outputs.json` と、第5章の version 2 の鍵ファイルを使って次のように進めます。

```sh
drmr analyze "https://example.com/vod/master.m3u8" \
  --download-dir ./multi-source --json
drmr plan ./multi-source/input.m3u8 --outputs outputs.json \
  --output multi-result --save multi.plan.json --json
drmr convert --plan multi.plan.json \
  --keys-file config/multi-keys.json --json
drmr verify --plan multi.plan.json \
  --keys-file config/multi-keys.json --json
```

保存した一つの原本をすべての出力に使います。URL からの取得後も、第3章で説明した処理方式ごとの DRM 入力条件が適用されます。

取得と変換を一回のコマンドで行い、計画ファイルを保存しない使い方は次のとおりです。

```sh
drmr convert "https://example.com/vod/master.m3u8" \
  --download-dir ./multi-source --outputs outputs.json \
  --output multi-result --keys-file config/multi-keys.json --json
```

どちらの例でも、実行時には新しい原本保存先と出力先を選びます。URL を直接変換する場合も、取得時に記録したファイルの所有関係と内容を照合し、すべての出力を配置する直前まで確認します。

## 保存した原本のオフライン利用

```sh
drmr convert saved-source/input.m3u8 --to cbcs --format hls \
  --output retry-hls --keys-file config/hls-output.json
drmr verify retry-hls/master.m3u8 --source saved-source/input.m3u8 \
  --keys-file config/hls-output.json --json
```

取得が完了すれば、配信元へ再接続せずに保存した原本で処理と検証ができます。変換に失敗しても、取得済みの原本は残ります。ダウンロードが未完了のデータを原本として使うことはありません。停止時は取得やメディア処理を止めてから、一時ファイルを削除します。

出力の配置直前にも、保存した原本とローカル参照を確認します。取得後に原本が改変・削除されていた場合は出力を配置しません。URL からの取得を再開するために、取得済みのディレクトリを指定することもできません。再実行には、保存したローカルのエントリーポイントを使ってください。

`convert --plan` は保存済みの出力先を使い、入力・暗号方式・形式・出力先の上書きや、既存の結果への上書きを拒否します。別の出力先を選ぶ場合は新しい計画を作ります。完成済みの結果を改めて確認する場合は `verify --plan` を使います。

## 取得ポリシー

既定では、HTTPS かつ入力 URL と同じ origin への接続だけを許可します。origin はスキーム、ホスト、ポートの組み合わせです。別 origin の参照や認証が必要な場合は、所有者だけがアクセスできる JSON ファイルに設定し、`--fetch-policy` で指定します。`analyze URL` と直接の `convert URL` の両方で使えます。`analyze --metadata-only URL` にも指定でき、その場合は取得先を指定せず、原本を保存しません。

次の内容を、例えば `config/fetch.json` という UTF-8 のテキストファイルに保存します。ファイル名は任意です。入力 URL はコマンドで指定するため、この JSON に書く必要はありません。取得ポリシーには `schema_version` を付けません。認証が不要な場合は `headers_by_origin` を省けます。追加の設定が不要なら `--fetch-policy` 自体を省略できます。

```json
{
  "allowed_origins": ["https://media.example.com"],
  "headers_by_origin": {
    "https://example.com": {"Authorization": "Bearer <your-token>"}
  },
  "max_total_bytes": 1073741824,
  "connections": 4
}
```

| 項目 | この例の意味 |
| --- | --- |
| `allowed_origins` | 入力 URL の origin に加え、`https://media.example.com` への参照を許可する。プレイリストのファイルパスではなく origin を指定する |
| `headers_by_origin` | `https://example.com` へのリクエストだけに認証ヘッダーを付ける。`<your-token>` は実際のトークンに置き換える |
| `max_total_bytes` | 取得量の合計を 1,073,741,824 バイト（1 GiB）までに制限する |
| `connections` | 同時取得数を最大 4 にする。CPU・メモリや `--max-workers` によっては、実際の同時取得数はさらに少なくなる |

第5章と同様に `config` ディレクトリを保護し、JSON を保存した後にファイルの権限を設定します。macOS/Linux の指定例です。Windows では第12章の所有者専用 ACL を使います。

```sh
chmod 600 config/fetch.json
drmr analyze "https://example.com/vod/master.m3u8" \
  --download-dir analyzed-source --fetch-policy config/fetch.json --json
```

```sh
drmr convert "https://example.com/vod/master.m3u8" \
  --download-dir authenticated-source --fetch-policy config/fetch.json \
  --to clear --format hls --output clear-result
```

ヘッダーは設定した origin にだけ送ります。リダイレクト先と子参照にも許可が必要です。HTTP とプライベートネットワークへの接続は既定で無効で、必要な場合だけ `allow_http`、`allow_private_network` を明示します。JSON に認証情報がある場合は、鍵設定と同じく秘密として扱います。

主な追加設定は `max_playlist_bytes`、`max_playlists`、`max_segments_per_track`、`max_resource_bytes`、`max_total_resources`、`min_free_disk_bytes`、`timeout`、`attempts` です。数値の型と範囲を確認し、不明なフィールドや重複したフィールドは拒否します。`connections` は同時取得数の上限です。利用可能な CPU・メモリと `--max-workers` によっては、実際の同時取得数がそれより少なくなる場合があります。

`max_playlist_bytes` は HLS プレイリストだけでなく DASH MPD 一つあたりの上限にも使います。MP4 やメディアリソース一つあたりの上限は `max_resource_bytes`、取得全体の上限は `max_total_bytes` で指定します。

取得処理の実装上の上限は、HLS プレイリストまたは DASH マニフェスト一つあたり 4 MiB、マニフェストとメディアの合計リソース数が 100,000、取得計画に使うテキストと URL メタデータが 16 MiB です。リソース本体は所有者だけがアクセスできるファイルに逐次保存します。取得ポリシーを緩めても、メディアの対応条件や上限は変わりません。

成功 JSON の `download` には保存先の表示パス、エントリーポイントと相対パス、`retained: true` が入ります。入力 URL とリクエストヘッダーは含めません。一方、保存した原本には署名 URL などが残り得るため、保存先を保護してください。

## 変換例の準備と鍵ファイル {#recipe-setup}

対応表の出力方式から例を開き、入力の暗号化方式に合う**4コマンドの組を一つ選んで**実行します。ほかの組は別の入力向けです。macOS/Linux などの POSIX シェル用で、行末の `\` は同じコマンドの続きです。`&&` で失敗時に止め、ファイルを介して処理をつなぎます。`|` は標準出力を渡す仕組みです。`https://example.com` の URL は差し替えてください。

所有者だけがアクセスできる既存の作業ディレクトリで実行します。入力を保存する `src`、`plan.json`、`result`（MP4 は `result.mp4`）は新規作成のため、同名のものがない場所を選びます。`mkdir` や `cd` は不要です。再実行時は各パスを揃えて変更するか、別の作業場所を使います。取得条件は `analyze` に `--fetch-policy config/fetch.json` を追加します。[設定例](#url-input)を参照してください。

鍵が必要なら[第5章の構造例と権限設定](#key-configuration)に従い、`config/keys.json` をこの作業場所の `keys.json` に置き換えます。原本・出力の中には置きません。`schema_version: 1` を使い、Clear 側の `input` または `output` は省略します。`analyze` と `plan` は暗号化入力の鍵だけ、`convert` と `verify` は入力・出力それぞれの暗号化に使う鍵が必要です。両方 Clear なら鍵ファイルは不要です。

CENC/CBCS 入力は `input.keys`、AES-128 と TS SAMPLE-AES は `input.hls_keys` に絶対鍵 URI と必要な IV を設定します。cenc/cbcs 出力は `output.kid` と `output.key`、HLS はさらに `output.hls_key_uri` が必要です。他の出力項目は第5章に例があります。入力鍵と出力鍵は別の値にできます。鍵 URI やライセンス URL には接続しません。

入力は[第3章の対応条件](#supported-profiles)を満たすものとします。MP4・DASH・fMP4 HLS の例は映像1トラックを想定しています。TS 映像は音声を含められ、音声のみの TS は 44.1 kHz または 48 kHz の AAC-LC が対象です。映像・音声を含む TS 出力は別プレイリストになります。

FairPlay HLS や Widevine/PlayReady DASH の情報を明示する場合は、[第6章の複数出力の手順と対応条件](#inspect-plan)を使います。

{{conversion-examples}}

# 9. 検証の意味と処理の単位

## Rust の標準検査と追加の完全デコード

| 結果の項目 | 照合する内容 |
| --- | --- |
| `stream_metadata` | コーデック設定、画面寸法、音声レート・チャンネル、色、回転、対応する言語情報。一致は `pass` |
| `packet_payloads_equal` | 符号化済みの全サンプルまたはアクセスユニットの内容・順序・個数。格納形式の変更に伴う枠組みを区別する |
| `packet_timing_equal` | 全 PTS・DTS・継続時間、正確な終端、対応する再生範囲 |
| `decode_evidence` | 既定では `not-checked`、理由は `not-requested`。--decode では実際に得た完全デコードまたはデコード内容の比較結果 |

DASH・HLS の出力には実際の `manifest.mpd`・`master.m3u8` を指定します。Rust が原本と結果を独立に読み、暗号化されている場合はそれぞれの役割の鍵で復号します。最終サンプルを含め、対象の全サンプルを検査します。先頭セグメントやファイル名の一致では判定しません。圧縮データが同じでも、編集情報が変われば再生範囲は変わり得ます。

PTS は表示時刻、DTS はデコーダーに渡す時刻です。B ピクチャーなど表示順とデコード順が異なる映像では両者がずれます。音声と映像の同期も確認するため、絶対時刻で比較します。

TS の AVC では SPS/PPS/VUI と全スライスヘッダーを解析し、元のアクセスユニット、PTS・DTS、対応する画像の時刻、原本の `EXTINF` を照合します。VUI の時刻情報がない映像、フィールド符号化や混在映像、MBAFF、未対応の繰り返し画像時間、スライスグループやスライス順序は拒否します。プロファイル名、ビット深度、色差形式を識別できても、その全組み合わせの動作確認を済ませたという意味ではありません。エントロピー符号化された画像本体を完全にデコードする検査は `--decode` で追加します。追加しても Rust の標準検査は省きません。

原本の有限な再生範囲が結果から失われれば、全サンプルの内容が一致していても時刻の照合は失敗します。入力準備でも元の開始位置と長さを保持します。どちらの検査範囲も、DRM ライセンスの取得や特定機器での再生を保証しません。

これらの暗号方式には、復号した内容を暗号学的に認証する仕組みはありません。独立した既知の非暗号化データがなければ、Rust による対応の照合だけで鍵の正しさや全体のデコード可否は証明できません。`--decode` の成功は全体のデコード検査が通った証拠であり、鍵や元の平文の暗号学的な認証を意味しません。

## AAC と二秒セグメント

一般的な AAC-LC フレームは 1,024 サンプルです。48,000 サンプル/秒では約 21.333 ミリ秒になり、二秒は 93.75 フレームです。44,100 サンプル/秒では約 23.220 ミリ秒で、二秒は約 86.133 フレームです。フレームの途中で符号化データを切れないため、音声の境界や個数は二秒の映像セグメントと一致しないことがあります。

音声のサンプルレートと、ファイルに時刻を記録する単位は別のものです。TS の時刻は一秒を 90,000 等分した整数で記録します。AAC の一フレームは 48 kHz なら 1,920 刻みちょうどですが、44.1 kHz では約 2,089.796 刻みになるため丸めが必要です。毎回 2,089 だけ加えると、処理する時間が長いほどずれが大きくなります。

drmr は、原本の PES（符号化した映像や音声をまとめたパケット）にある時刻を保持し、その中の各 AAC フレームの時刻を実際のサンプルレートから求めます。原本を受け入れる際は、パケット先頭の時刻を整数に丸める処理と、パケット内の各フレームの時刻を求める処理を分けて検査します。トラック全体に共通の時刻の基準を使い、丸め方が一貫していることを確認します。確認できた各フレームの時刻は、出力との比較で一つずつ厳密に照合します。

44.1 kHz の AAC を分離するときは、作業用 fMP4 の時刻も 90 kHz のままにします。一方、デコーダーに渡す設定には本来の音声レートである 44,100 サンプル/秒とチャンネル構成を記録します。AAC の符号化データ、フレーム数、元の表示時刻とデコード時刻は保持し、`--decode` ではデコードした全サンプルも照合します。このトラックから Shaka Packager で HLS・DASH・TS の出力を作ります。作業用 fMP4 は最大 128 フレーム（符号化データで最大約 1 MiB）ずつ書き出すため、コンテンツ全体をメモリに保持する必要はありません。

FFprobe が TS の一フレームの継続時間を 2,089 刻みと表示していても、次の復号時刻までは 2,090 刻み離れていることがあります。MP4 のサンプル継続時間には、この実際の時刻差を記録します。次の時刻がない最終フレームだけは、TS の整数の継続時間を使います。検証では MP4 の継続時間が次の復号時刻につながること、原本の全 PTS・DTS と最終的な終端が一致することを確認します。原本の時刻の丸めを理由に、変換結果の時刻ずれを許すことはありません。詳細は [AAC のサンプルレートと時刻の説明（英語）](https://github.com/kazuhide/drmr/blob/main/docs/media-processing.md#aac-sample-rates-and-transport-clocks)を参照してください。

44.1 kHz の入力プレイリストでは、最終区間以外の `EXTINF` が「この区間の先頭フレームから次区間の先頭フレームまで」を表す場合もあります。最終フレーム全体を再生する区間では、音声サンプル数から求めた長さ、または最終フレームを 2,089 か 2,090 刻みとして求めた長さとの一致を確認します。こうした記載方式に応じて原本の区間長を検査しますが、PTS・DTS の比較に許容差を設けるものではありません。

最後のフレームの一部だけを再生する指定は、時刻の丸めとは別に扱います。符号化データと各フレームの正確なサンプル範囲は残し、どこまで再生するかを別途記録します。fMP4 HLS・DASH では、MP4 の二つの編集項目で原本の開始時刻と正確な再生時間を保持します。全体の時刻単位には音声一サンプルと TS 一刻みの両方を正確に表せる値を使い、メディアの時刻は 90 kHz のままにします。編集項目はパッケージング後に追加します。再変換時は、この形式と確認できた編集だけを作業用コピーから除いてパッケージャーに渡すため、時刻調整の丸めを避けられます。

HLS の区間長は出力自身の再生範囲から求めます。DASH は符号化されたセグメントの時刻表を保持し、映像・音声それぞれの再生終端をすべて含むように全体の長さを決めます。TS 出力では最終プレイリストに短縮後の長さを記録します。独立検証は出力自身から再生範囲を読み直し、原本の全データと実際に再生する音声サンプルの両方を照合します。MP4 に記録された音量、再生速度、トラックの有効・無効も検査します。この処理では標準の音量と速度、有効なトラックを前提とし、符号化データが同じでも再生設定を変えた出力は検証を通しません。詳しくは [AAC の再生終端の説明（英語）](https://github.com/kazuhide/drmr/blob/main/docs/media-processing.md#playback-ending-inside-the-final-aac-frame)を参照してください。

整数の `EXT-X-TARGETDURATION` は、各区間の長さを最も近い整数に丸めた値以上である必要があります。drmr は生成した区間の長さを切り上げて余裕のある上限を求め、映像と音声のうち最大の値をすべてのメディアプレイリストに設定します。例えば映像が 2、音声が 3 なら、両方を 3 に揃えます。符号化したセグメントと `EXTINF` の長さは変えません。別途行う検証でも最終ファイルの宣言を読み、上限が揃わない出力を拒否します。RFC 8216 には VOD の字幕と I-frame プレイリストに関する例外がありますが、通常の映像・音声に別々の上限を設定できるという意味ではありません。

この差は、音声の欠落・重複・すき間・重なり・累積ずれを許す理由にはなりません。「一フレーム以内なら同じ」という許容差は使わず、すべてのパケット時刻と全サンプルを確認します。

48 kHz の十秒の試験 MP4 は 470 AAC フレームを含み、すべてをデコードすると一チャンネルあたり 481,280 サンプルになります。先頭の初期遅延 1,024 サンプルと末尾の 256 サンプルを除くため、実際の再生範囲は一チャンネルあたり 480,000 サンプルです。TS への変換でパディングも再生される構成になっている場合は、その TS 自体の再生範囲を基準にします。元の MP4 に合わせて短くすることはありません。

## 有限再生範囲を別に照合する理由

圧縮パケットが同じでも、MP4 の編集情報が消えると、再生すべきでない先頭や末尾が聞こえる場合があります。FFmpeg 9.0.2 の一部のフラグメント化 MP4 読み取りでは、有限の末尾切り詰めが通常デコードに反映されません。このため、単に同じツールで普通に復号して比較するだけでは足りません。

`--decode` を付けた有限 AAC の処理では、編集情報の自動適用やサンプルの破棄をせずに原本と結果をすべてデコードし、内容と個数を比較します。その後、各ファイルの情報から確認した開始位置と長さを一度だけ適用し、再生される内容と個数も比較します。さらに、プログレッシブ MP4 原本を通常どおりデコードした結果が、原本自身の再生範囲と一致することも確認します。変換結果に合わせて原本を切り詰めることはありません。

比較にはデコーダーの 32 ビット浮動小数点出力を使い、16 ビット整数への変換で生じる追加の丸めを避けます。ハッシュの計算と再生範囲の選択は固定サイズのバッファで逐次処理するため、非圧縮音声全体をメモリに保持する必要はありません。ファイル内の再生設定、音量、速度、有効フラグ、映像の表示寸法も確認します。ただし、すべてのプレーヤーで同じ編集情報が適用されることを保証するものではありません。

## マニフェストに記載する時刻の照合

TS の時刻はゼロ以外から始まることが多く、音声が最初の映像より先に始まる場合もあります。TS→DASH では、原本の全トラックの中で最も早い表示時刻を共通の始点にします。DASH の presentation-time offset を使って配信時刻に対応させ、トラックごとに個別にゼロへずらすことはしません。

パケットとセグメントの時刻は整数や有理数で正確に扱います。TS→DASH の最終 Period や、有限再生範囲を持つ DASH/HLS の再生時間を小数文字列で記載する際は、正確な終端を小数九桁で切り上げます。差は 1 ナノ秒未満です。これはマニフェストへの記載方法の規則であり、パケット時刻に誤差を認めるものではありません。記載された値がこの計算結果と異なれば拒否します。

有限編集を持たない通常の HLS では、EXTINF の小数表現に別の丸め規則を使います。その幅は、メディアクロックの一刻みと約一ミリ秒のうち大きい方です。この規則でも、全パケットの時刻は厳密に比較します。有限音声の一サンプルの違いを、この丸め幅を理由に受け入れることはありません。

## DRM 情報とマニフェスト検査

名前付き出力計画の `verify --plan` は現在の各出力を読み直し、一つの保存原本と独立に比較します。結果では次の確認事項を分けて示します。

| 結果項目 | 確認する内容 |
| --- | --- |
| `verification` | 全サンプル・設定・時刻・再生範囲の Rust 検査と、実際に得た追加デコードの証拠 |
| `verification` 内の `native_manifest` | Rust による最終 HLS/DASH 宣言の検査。規則ごとの検査範囲とプロファイル版を含む |
| `output_matches_plan` | 実際のコンテナ形式と暗号方式が計画どおりか |
| `signaling` | 指定した DRM 情報が、実行設定および実際のメディアとマニフェストの宣言に一致するか |
| `conformance` | 必須の外部マニフェスト検査の結果、または検査を要求していないこと |

PSSH、PlayReady 情報、KID、FairPlay の宣言が変われば、復号そのものができても検証は失敗し得ます。`drms` が空の場合はサービス固有の DRM 情報を要求しておらず、シグナリングを検証済みとは表示しません。この場合もメディア全体と暗号方式の確認は必要です。

サービス固有の DRM を指定しない cenc / cbcs 出力にも、W3C の Common SystemID を使う PSSH が含まれることがあります。これは鍵の識別子を伝えるための情報であり、コンテンツキーそのものやサービス固有のポリシーは含みません。drmr は、自身が生成する「KID が一つでデータ部がない形式」を出力設定と実際のトラックの保護情報に照らして検査します。この検査で DRM サービスやライセンスの動作を確認できるわけではなく、入力の対応範囲も変わりません。

共通の HLS/DASH 変換とコンテンツ全体の検証では、Rust で実装した内蔵 Linter が最終マニフェストを検査します。この検査に外部ツールは不要です。Linter は、文法や宣言の整合性を規則に沿って調べる仕組みです。HLS では文法、セグメントと鍵の状態、必要なプロトコルバージョン、関連プレイリストの参照を検査します。Apple 固有の VOD 規則は基本の HLS 規則と区別しています。DASH では、対象とする静的配信プロファイルの構造、継承、参照方式、タイムライン、保護情報の宣言を検査します。独自の Rust 実装であり、XML スキーマや Schematron 全体を実行するものではありません。

結果には、規則ごとの成功・失敗・未検査と仕様の参照先を記録します。検査範囲内の未対応機能、参照先プレイリストの不足、資源上限への到達、診断の打ち切りがあれば、合格とは判定しません。メディアの実測や配信環境が必要な規則はマニフェストだけの検査の対象外とし、コンテンツ全体の検証結果とは分けて示します。診断には規則 ID と原文中の位置を示しますが、マニフェスト本文、鍵 URI、保護情報のペイロードは転記しません。小さい入力は一つの処理として実行し、大きな独立した文書は CPU とメモリの制約内で並列に処理できます。

`conformance: "required"` を指定した出力では、HLS は Apple の `mediastreamvalidator` をプレイリスト検査モードで使います。DASH はローカルの DASH-IF Conformance Software による XML スキーマと Schematron の検査を使います。XML スキーマは MPD の構造、Schematron は追加の記述規則を確認する仕組みです。HLS の参照先の子プレイリストを含め、暗号化された最終出力のマニフェストを検査します。

外部検査は、メディア全体の内部検証とは別に行います。暗号化メディア全体の規格適合、ライセンス取得、実機再生を確認するものではありません。必要なツールがない場合、検査が中断した場合、結果が不完全な場合、検査エラーがあった場合はいずれも失敗です。`"none"` は外部検査を要求しない指定であり、検査に成功した場合とは区別して表示します。

Apple の HLS ツールは macOS の環境に準備します。DASH は対応するローカルの DASH-IF ソフトウェアと依存ツールを用意し、`DRMR_DASH_IF_ROOT` にその配置先を設定します。`--hls-validator-bin` は Apple の実行ファイルを選び、既定値は `mediastreamvalidator` です。`--dash-validator-bin` はローカル DASH-IF アダプターを実行する PHP の場所で、既定値は `php` です。任意の DASH 検査プログラムを渡すオプションではありません。実行ファイルの場所は計画に保存しません。詳細は[マニフェスト検証の準備と範囲（英語）](https://github.com/kazuhide/drmr/blob/main/docs/manifest-validation.md)を参照してください。

HLS のバージョンは、使う機能に必要な最低値を満たしているかで判定します。特定の一つのバージョン番号との完全一致は求めません。

| このプロファイルの出力機能 | `EXT-X-VERSION` の下限 |
| --- | --- |
| `EXT-X-MAP` を使う通常の fMP4 メディアプレイリスト | 6 |
| 鍵形式を宣言する FairPlay SAMPLE-AES TS | 5 |
| 上記いずれかが version 7 を宣言 | 最低版の条件を満たす |

版番号だけで他の宣言の正しさが保証されるわけではありません。第5章で説明した、関連 IV を別に持つ TS や `tenc` に必要な情報を保持する fMP4 では、プレイリストの IV 属性を省略できる場合があります。明示した値の不一致は拒否します。

## マニフェストのみの検査

既存の HLS/DASH 出力について、マニフェストだけを検査する場合は次のモードを使います。

```sh
drmr verify result/hls/master.m3u8 --manifest-only --json
drmr verify result/dash/manifest.mpd --manifest-only --json
```

この二つのコマンドは Rust の内蔵 Linter を使い、外部検査ツールは不要です。Apple または DASH-IF の検査も加える場合は `--external-validator` を指定します。JSON は `native_manifest` と `external_manifest` を分けて返します。`--max-workers` は並列数の上限で、省略時は自動で選びます。Apple の外部検査は parse-only モードを使うため、内蔵の Apple 向け制作規則とは検査項目が異なります。

既定の `--manifest-profile base` では、HLS は RFC 8216 のマニフェスト規則、DASH は static MPD のプロファイルを検査します。HLS に Apple VOD 向けの対応済み規則を加える場合は、`--manifest-only` と `--manifest-profile apple-vod` を指定します。DASH への指定は拒否します。選択したプロファイルはレポートに記録します。例えば映像が一つのビットレートしかない場合、基本の HLS 規則には合格しても、複数ビットレートを求める Apple の 9.9 項には合格しません。drmr は既存の画質を保持し、新しい画質の映像は作りません。どちらのプロファイルもマニフェストの検査範囲に限られ、メディア・配信条件・機器での動作まで保証するものではありません。

結果は `scope: "manifest-only"`、`media_verified: false` です。コンテンツキーを読み込まず、原本と出力のメディア比較も行いません。`--manifest-only` と `--plan`、`--source`、`--keys-file` は併用できません。メディア全体と保存した変換条件を確認する場合は、適切な鍵ファイルを渡して通常の `verify --plan` を使ってください。

## 変換時の処理単位と再セグメント化

| --processing の値 | 主な処理単位 |
| --- | --- |
| `auto` | 入出力に応じて選択。無指定時の従来の変換動作は維持 |
| `segments` | 対応する同一コンテナの元セグメントを処理し、境界・サンプルの所属・時刻を保持 |
| `stitch` | 必要な復号・結合後、MP4 または HLS/DASH を出力。HLS/DASH は二秒を目標に再分割 |

直接の `convert` と `plan --save` で指定します。未対応の方式は理由を示して拒否し、別方式へ切り替えません。保存した処理方式は `convert --plan` で上書きできません。Rust の `segments` 検査では全編の結合ファイルを作りません。`--decode` は別途トラック全体を準備する場合があり、TS の準備には Shaka Packager も使います。`stitch` は元の境界を保証しません。

作業用ディスクには原本のほか、比較用の復号済みデータや変換結果の候補も保存するため、元データと同じ空き容量だけでは足りない場合があります。必要な容量は処理経路によって異なります。十分な空き容量と適切なアクセス権を確保し、原本と鍵は独立検証が終わるまで保持してください。

複数出力では、各出力のコピーを作成してバイト列を照合し、出力ルート全体を配置する直前まで保持します。独立した計画検証でも、各結果のコピーを最後の整合性確認まで保持します。各出力を別々に作成することもあり、必要な作業用ディスクの容量はコンテンツの長さと出力数に応じて増えます。コンテンツ全体をメモリに保持する方式ではなく、ディスクへ逐次保存します。空き容量を確保し、一時領域の削除が終わるまで待ってください。

参照（英語）: [処理方法とコンテンツ全体の検証](https://github.com/kazuhide/drmr/blob/main/docs/media-processing.md)。

# 10. 並列処理・メモリ・停止

## 並列数の自動調整と --max-workers

通常の `analyze`、`plan --save`、`convert`、`verify` は、利用可能な CPU とメモリ、映像の寸法、リソースのサイズ、トラック数から並列数を自動で調整します。`--max-workers` には 1 から 64 の範囲で上限を指定できますが、実際にはそれより少ないワーカーを使う場合があります。省略時に選ぶ推奨数は最大 32 です。`analyze --metadata-only` とは併用できません。

```sh
drmr convert source.mpd --to cbcs --format dash \
  --output result --keys-file config/keys.json --max-workers 2
drmr verify result/manifest.mpd --source source.mpd \
  --keys-file config/keys.json --max-workers 1 --json --measure
```

ワーカー数を変えても、内容と時刻の検証は省略しません。トラック処理、セグメント処理、原本解析では、同じプロセス内で外部処理に使う CPU・メモリの予約枠を共有します。別々の CLI プロセス間では予約枠を共有しません。

通常の `analyze` はダウンロード後も続きます。原本のコピーを照合してトラックを準備し、全サンプルの符号化データ、設定、時刻、再生範囲を Rust で検査するためです。`--decode` では、デコードした映像・音声全体も確認します。並列に動かせるのは各段階の独立した作業で、すべての処理や外部ツールの呼び出しが同時に動くわけではありません。回線やディスクの速度、プロセスの起動、トラック数、順序を保つ必要のある検査も所要時間に影響します。性能を比べる場合は、保存済みの同じローカル原本を使い、並列数一つの場合と自動選択の場合を比較してください。概要だけを知りたい場合は `analyze --metadata-only` を使えますが、変換できるかどうかの確認にはなりません。

TS の準備では、独立したセグメントのメタデータと符号化フレームを並列に読み取ります。メモリに保持するのは、件数を制限した小さな解析結果です。その後、プレイリストの順序で再生時間と時刻の境界を照合し、メディアプレイリストを一つずつ処理します。原本に対する全サンプルの検査は引き続き必要で、`--decode` の場合はトラック全体もデコードします。

内蔵のマニフェスト検査も同じ予約枠を使います。小さい入力は呼び出し元で処理します。大きな HLS は文書ごとに、大きな DASH は一度解析したツリー内の独立した検査ごとに並列処理できます。処理用のプールを入れ子にせず、子プレイリストの解析結果や継承したタイムラインを再利用します。マニフェストの保持と検査に必要なメモリを予約し、ファイルの識別情報と内容も照合します。バイト数、ノード数、参照数、診断数などの上限に達した場合は不完全な検査として報告し、合格にはしません。

## 資源見積もりの範囲

Linux では、読み取り可能な CPU quota、メモリの cgroup 上限、親 cgroup の上限も見積もりに使います。cgroup は、コンテナやサービスが使う OS の資源を制御する仕組みです。資源情報を取得できない場合は、512 MiB の枠で子処理を一つ実行する前提で見積もります。一つの処理もこの枠に収まらないと判断した場合は、`resource-budget` で失敗します。

利用可能なメモリがわかる場合は、その四分の一を基準に、最低 256 MiB の予約枠を確保します。トラックごとの保持データも見積もりに含めます。子処理については基本の使用量に加え、画素数、入力の処理量、取得する出力の上限を考慮します。これは処理を開始できるかを判断するための見積もりであり、OS によるメモリ制限や最大使用量の保証ではありません。

追加のデコードを行う場合、FFmpeg のデコーダー・エンコーダー・フィルターと FFprobe のデコードには、それぞれスレッド数 1 を設定します。補助スレッドも含めて一つになるわけではありません。音声ハッシュの計算とパケット時刻の照合は逐次処理します。ただし、トラックやセグメントのメタデータは保持するため、メモリ使用量全体が常に一定になるわけではありません。

外部処理から取得する標準出力の上限は既定で 16 MiB、標準エラーは 1 MiB です。上限を超えた場合は `output-limit` などで失敗し、途中で切れたデータを使って検証を成功とすることはありません。待機中も制限時間と停止要求を確認します。

## Ctrl+C と終了確認

macOS/Linux の汎用 `convert`・`verify` は SIGINT、SIGTERM、Windows は Ctrl+C と Ctrl+Break で協調停止します。予約待ちと新規処理を止め、所有する子プロセスを終了し、後片付けを確認します。公開前に停止要求を観測した場合は結果を配置しません。

終了コード 130 は、処理の停止と子プロセスの終了確認が完了したことを示します。`cleanup-unconfirmed` や `media-cleanup-unconfirmed` は、停止要求による終了であっても失敗として扱います。強制 kill や OS による強制終了では協調停止の処理を行えず、一時領域が残ることがあります。

Unix では drmr が作成したプロセスグループで、Windows では job object で子孫プロセスを管理します。終了要求を送っただけでは、子孫プロセスがファイルを閉じたと判断しません。Unix で直接の子プロセスが終了した後のグループ確認と、Windows で job の終了通知を待つ処理には、それぞれ別の 30 秒の制限時間があります。ただし、ファイル I/O、名前解決、OS のスケジューリングも含めた全体の終了時刻を厳密に保証するものではありません。

## 計測値の読み方

過去の完全デコードを伴う計測は、現在の `--decode` に相当する処理範囲の記録です。Rust 標準検査の現ビルドを計測した値ではありません。性能は検査範囲を揃えて比較してください。

`--measure` は `--json` と併用します。メディア検証の条件は変えず、数値を追加します。フェーズは重なったり入れ子になったりするため、フェーズ時間を単純合計して全体時間とみなさないでください。

短い十秒アセットの時間は、起動処理やキャッシュの影響を受けます。メモリのサンプリング値は真の最大値ではありません。比較する場合は、原本・ツール版・ビルド設定・ワーカー上限・ホスト負荷を揃え、まず同じ完全検証が通ることを確認してください。

内蔵ライブラリの検査速度を測るため、ソース [`c1458e7d`](https://github.com/kazuhide/drmr/commit/c1458e7deea5b31118073c682d799f60c8f4e8b5) を Rust 1.99 の release ビルドで実行しました。環境は論理 CPU 10 個、メモリ 32 GiB の macOS ARM64 です。2026年10月21日の測定では、マスタープレイリストと、各1000セグメントのメディアプレイリスト64本を検査する時間が、逐次処理で 14.50 ms、自動選択された10ワーカーで 3.37 ms でした。どちらの検査レポートも一致しました。小さい入力は1ワーカーで処理します。

30000項目の DASH タイムラインでは逐次処理が 5.22 ms、自動選択が 5.27 ms となり、並列化で速くならない入力もありました。これらは実行順を変えて三回測った中央値で、変換時間ではなくライブラリによる検査全体の時間です。測定には作成した検査用データを使いました。同時に実行していた本プロジェクトのメディア試験は停止しましたが、ホスト上の他の処理までは分離していません。追加のメモリ確保量とプロセスのメモリ使用量は別々に測定しており、詳細はリンク先に記載しています。

参照（英語）: [資源制限と測定方法](https://github.com/kazuhide/drmr/blob/main/docs/resources.md)。

# 11. Docker による実行

## イメージのビルド

Docker イメージは CLI、FFmpeg/ffprobe、Shaka Packager を含みます。ビルドには Git と BuildKit が使える Docker が必要です。ネイティブ Linux amd64 または arm64 が対象で、Docker Desktop は Linux コンテナを選びます。

リポジトリのルートから実行します。ビルドにはコミット済みのファイルを使います。

```sh
revision=$(git rev-parse HEAD)
tree=$(git rev-parse "${revision}^{tree}")
git archive --format=tar "$revision" | docker build \
  --build-arg DRMR_REVISION="$revision" \
  --build-arg DRMR_TREE="$tree" --tag drmr:local -
docker run --rm --init drmr:local doctor --probe --route all
```

上記で取得したコミットのファイルとロックファイルを使うため、未コミットの変更は含まれません。鍵、入力メディア、ダウンロード、結果はリポジトリ外に置き、実行時にマウントします。最初のビルドでは同梱ツールもコンパイルするため、時間とディスク容量が必要です。ビルド後も Docker のキャッシュは残ります。

イメージのコンパイラーやツールの固定版は、そのイメージを再現するための指定です。ソースビルドの最低要件 Rust 1.95 以上とは別です。

## 入力・設定・出力のマウント

ホストに `media`、`config`、`output` を用意します。設定と出力のディレクトリは所有者だけがアクセスできる権限にし、鍵 JSON の権限を 600 にします。以下は、clear の DASH など、対応する原本と有効な鍵設定を用意済みの場合の例です。

```sh
docker run --rm --init --user "$(id -u):$(id -g)" \
  --mount "type=bind,src=$PWD/media,dst=/input,readonly" \
  --mount "type=bind,src=$PWD/config,dst=/config,readonly" \
  --mount "type=bind,src=$PWD/output,dst=/output" \
  drmr:local convert /input/manifest.mpd \
  --to cbcs --format dash --output /output/protected-dash \
  --keys-file /config/keys.json
```

drmr に渡すのはコンテナ内のパスです。`/input` と `/config` は読み取り専用、`/output` は書き込み可能です。ユーザー・グループ ID をホストに合わせると、所有者専用の鍵を読み、結果を自分の所有物として作れます。イメージの既定ユーザーは root ではありません。ホストの不適切な所有者や権限を自動修復しません。

結果を改めて読むときは、出力も読み取り専用にできます。

```sh
docker run --rm --init --user "$(id -u):$(id -g)" \
  --mount "type=bind,src=$PWD/media,dst=/input,readonly" \
  --mount "type=bind,src=$PWD/config,dst=/config,readonly" \
  --mount "type=bind,src=$PWD/output,dst=/output,readonly" \
  drmr:local verify /output/protected-dash/manifest.mpd \
  --source /input/manifest.mpd --keys-file /config/keys.json --json
```

Docker を使っても、対応するメディアプロファイルは増えません。URL 取得には、別途ダウンロード先の書き込み可能マウントと、その接続ポリシーも必要です。OS の CPU・メモリ制限を読める場合は、ワーカーの見積もりに反映します。

## 停止と後片付け

`--init` は停止シグナルの転送と終了した子プロセスの回収を補助します。drmr 自身による停止と、管理するプロセスの確認も引き続き行います。接続中の CLI では Ctrl+C、名前付きコンテナでは `docker stop --timeout 60 NAME` を使い、協調終了する時間を確保します。drmr が終了確認を完了した場合と、確認できずに強制終了した場合は区別してください。

`--rm` は終了したコンテナの一時ファイルシステムを削除します。マウントした原本・設定・結果は残ります。不要になった自分のイメージのタグは、そのイメージを使うコンテナがなくなった後に削除できます。

```sh
docker image rm drmr:local
```

ビルドキャッシュは別に残ります。専用 builder を使ったなら、その builder を不要時に削除します。共有キャッシュや無関係のイメージを一律に削除する必要はありません。

## Docker の Web コンソール

Linux ホストでは host networking を使うと、既定の `127.0.0.1:8080` を保って接続できます。

```sh
docker run --rm --init --name drmr-console --network host \
  --user "$(id -u):$(id -g)" \
  --mount "type=bind,src=$PWD/media,dst=/media,readonly" \
  --mount "type=bind,src=$PWD/output,dst=/output" \
  --env DRMR_INPUT_ROOT=/media --env DRMR_OUTPUT_ROOT=/output \
  drmr:local serve
```

Docker Desktop では追加の host networking 機能が必要です。コンテナ内の loopback だけで待ち受けるサーバーには、通常のポート公開だけでは接続できません。接続のために loopback・Host・Origin の保護を解除しないでください。ネイティブの Desktop GUI は、このイメージには含まれません。

Windows では Docker Desktop の WSL 2 連携を使い、Linux シェルで実行します。秘密情報を含む設定は Linux 側のファイルシステムに置いてください。Windows ドライブをマウントした場合は権限の扱いが異なるため、ネイティブ Windows 向けの ACL の設定手順は適用できません。

イメージ内の `/usr/share/drmr/build-manifest.json` は元コミット、ツリー、実際の依存版を記録します。ライセンスは `/usr/share/drmr/licenses`、対応するメディアツール等のソースは `/usr/share/drmr/sources` にあります。Debian パッケージ全体のソースミラーではありません。

参照（英語）: [Docker の使い方と確認結果](https://github.com/kazuhide/drmr/blob/main/docs/docker.md)。

# 12. Windows のネイティブ CLI

## 実行環境

Windows x64 の通常ユーザーで、ローカル NTFS 上から実行します。管理者昇格によって新規ファイルの所有者が Administrators になる実行、ネットワーク共有、所有者専用パス内の directory junction（ディレクトリの参照先を切り替える仕組み）、alternate data stream（ファイルの代替データストリーム）、作業領域の不適切な既存権限は拒否します。

CLI アーカイブを展開してユーザーの PATH に追加するか、完全パスで実行します。外部ツールは別途必要です。CI ビルドの CLI は VCRUNTIME140.dll を使うため、必要に応じて Microsoft Visual C++ x64 Redistributable をインストールしてください。

ソースビルドの場合:

```powershell
cargo build --release --locked -p drmr
.\target\release\drmr.exe --help
.\target\release\drmr.exe doctor --probe --route all
```

## 所有者専用の鍵ファイル

Windows では chmod の代わりに access control list（アクセス制御リスト、ACL）を使います。鍵ファイルは現在のユーザーが所有し、継承を無効にした ACL でそのユーザーだけに完全アクセスを許可する必要があります。次の設定は、管理者として起動していない Windows PowerShell 5.1（`powershell.exe`）で実行します。所有者専用の新しいディレクトリと空ファイルを作成してから、エディターで設定を入力します。

```powershell
$ErrorActionPreference = 'Stop'
$sid = [System.Security.Principal.WindowsIdentity]::GetCurrent().User
$private = Join-Path $env:LOCALAPPDATA 'drmr-keys'
if (Test-Path -LiteralPath $private) { throw 'Choose a new directory' }
New-Item -ItemType Directory -Path $private | Out-Null

function Set-OwnerOnlyAcl($path, $security) {
    $directory = $security -is
        [System.Security.AccessControl.DirectorySecurity]
    $sections =
        [System.Security.AccessControl.AccessControlSections]'Owner, Access'
    $current = if ($directory) {
        [System.IO.Directory]::GetAccessControl($path, $sections)
    } else {
        [System.IO.File]::GetAccessControl($path, $sections)
    }
    if (-not $current.GetOwner(
        [System.Security.Principal.SecurityIdentifier]).Equals($sid)) {
        throw 'The current user must own the path'
    }
    $security.SetAccessRuleProtection($true, $false)
    $rule = [System.Security.AccessControl.FileSystemAccessRule]::new(
        $sid, 'FullControl', 'Allow')
    $security.AddAccessRule($rule)
    $checked = if ($directory) {
        [System.IO.Directory]::SetAccessControl($path, $security)
        [System.IO.Directory]::GetAccessControl($path, $sections)
    } else {
        [System.IO.File]::SetAccessControl($path, $security)
        [System.IO.File]::GetAccessControl($path, $sections)
    }
    $rules = @($checked.GetAccessRules($true, $true,
        [System.Security.Principal.SecurityIdentifier]))
    if (
        -not $checked.AreAccessRulesProtected -or
        $rules.Count -ne 1 -or
        -not $checked.GetOwner(
            [System.Security.Principal.SecurityIdentifier]).Equals($sid)
    ) {
        throw 'Private ACL verification failed'
    }
    $actual = $rules[0]
    if (
        -not $actual.IdentityReference.Equals($sid) -or
        $actual.IsInherited -or
        $actual.AccessControlType -ne
            [System.Security.AccessControl.AccessControlType]::Allow -or
        $actual.FileSystemRights -ne
            [System.Security.AccessControl.FileSystemRights]::FullControl -or
        $actual.InheritanceFlags -ne
            [System.Security.AccessControl.InheritanceFlags]::None -or
        $actual.PropagationFlags -ne
            [System.Security.AccessControl.PropagationFlags]::None
    ) {
        throw 'Private ACL verification failed'
    }
}

```

続けてディレクトリとファイルへ ACL を適用し、鍵設定を入力します。

```powershell
Set-OwnerOnlyAcl $private ([Security.AccessControl.DirectorySecurity]::new())
$keyFile = Join-Path $private 'keys.json'
New-Item -ItemType File -Path $keyFile | Out-Null
Set-OwnerOnlyAcl $keyFile ([Security.AccessControl.FileSecurity]::new())
notepad $keyFile
```

この関数は、パスの所有者が現在のユーザーであることを確認してから、アクセス権だけを設定します。設定後も所有者と ACL を検査します。所有者の変更には別の権限が必要ですが、自分で作成したパスでは変更する必要はありません。エディターによるファイルの置き換えで ACL が変わった場合は、ファイルを所有者専用ディレクトリ内に置いたまま ACL を再適用してください。既存ファイルの不適切な権限を drmr が自動で修正することはありません。

## PowerShell からの変換

```powershell
.\drmr.exe analyze --metadata-only 'C:\media\source.mp4' --json
.\drmr.exe plan 'C:\media\source.mp4' --to cbcs --format mp4
$outDir = Join-Path $env:LOCALAPPDATA 'drmr-output'
New-Item -ItemType Directory -Path $outDir -Force | Out-Null
$result = Join-Path $outDir 'protected.mp4'
.\drmr.exe convert 'C:\media\source.mp4' --to cbcs --format mp4 `
  --output $result --keys-file $keyFile
.\drmr.exe verify $result --source 'C:\media\source.mp4' `
  --keys-file $keyFile --json
```

出力の親 `drmr-output` は先に用意し、`protected.mp4` 自体は存在しない名前を選びます。マニフェスト内の URI とオブジェクトキーには引き続き `/` を使います。Windows のネイティブパスとは別です。

Ctrl+C と Ctrl+Break で協調停止できます。子孫プロセスの終了とファイルロックの解放を確認してから、終了コード 130 を返します。終了を確認できなかった場合は、正常に停止したとは扱いません。長いパスなど、外部ツールやファイルシステムの構成によっては処理できない場合もあります。

出力先や一時領域を深い階層に置かないようにしてください。セグメントを作成できても、外部ツールがマニフェスト用に作る長い一時ファイル名で、パス長の制限に達する場合があります。

複数出力の HLS/DASH では、余分な階層を避けるため、出力先の親ディレクトリに共通の作業領域と各メディア出力の作業領域を並べて作成します。検証済みの HLS/DASH 出力は、同じファイルシステム上にある所有者専用の共通領域へ移します。MP4 は従来どおりファイルコピー用の作業領域を使います。いずれも、すべての確認後に出力ルート全体を一括で配置します。ただし、外部ツール自体のパス長制限をなくすものではありません。

参照（英語）: [Windows の保護条件](https://github.com/kazuhide/drmr/blob/main/docs/windows.md)。

# 13. ローカル Web コンソール

## 起動と対象

Web コンソールは認証なしのローカル操作画面です。信頼できるマシン上の loopback で使い、リバースプロキシやポート転送で公開しないでください。Host と Origin の確認は、他のローカルプロセスを認証する機能ではありません。

```sh
DRMR_INPUT_ROOT=/absolute/path/media \
DRMR_OUTPUT_ROOT=/absolute/path/output \
drmr serve
```

`http://127.0.0.1:8080` を開きます。二つのルートディレクトリが必要です。`DRMR_BIND_PORT` でポート、`DRMR_MAX_WORKERS` で保存計画の各処理に使うワーカー数の上限を設定します。自動割当てでもこの上限を超えません。メディアと保存計画は、許可された入力フォルダーから選びます。

サーバーとジョブ用の CLI は同じバージョンを使います。`--cli` で差し替える場合は実行ファイルを直接指定してください。別の CLI を起動するラッパーを指定すると、子プロセスを管理するための起動条件を満たさず、拒否される場合があります。

鍵や取得設定を使う場合は、起動前に `DRMR_COMMON_CONFIG_ROOT` へ非公開の設定フォルダーを指定します。入力、出力、独自の静的ファイル配信先と重ならないフォルダーを使ってください。

非公開の設定を選ぶ画面では、ファイル名とフォルダー名を番号に置き換えて表示します。各ページの並びはファイル名順です。選択するファイルはローカルのフォルダーで確認してください。設定の内容と名前はコンソールに表示しません。

## 保存計画を使う変換

「新しい変換」では、CLI と同じ保存計画を使って次の操作を個別に実行します。

1. 「ローカルメディア」「入力 URL」「CLI の保存計画」から入力を選び、「入力を開く」を押します。URL は[第8章の対応条件](#url-input)を満たす MP4・static DASH・完結した HLS を指定できます。ファイルはサーバーまたは Desktop で許可したフォルダーから選びます。ブラウザーへのアップロードや、任意のホストパスの入力は行いません。
2. 新しい入力では、必要に応じて「鍵ファイル」と「並列処理数」を指定し、「解析する」を押します。URL の取得設定は「取得ポリシーファイル」で選びます。鍵と取得ポリシーは非公開の設定フォルダーに置き、内容を画面へ入力しません。
3. 解析後、出力を 1〜8 件追加します。出力名、形式、暗号化方式、DRM 情報、外部マニフェスト検査を指定し、新しい出力の親フォルダーとフォルダー名を選んで「計画を保存」を押します。対応する入力条件と組み合わせは保存時に確認します。
4. 「元の計画ファイルを取得」で計画を非公開に保管し、「変換する」を押します。出力済みの場所は上書きできません。出力の設定を編集した場合は、新しい計画を保存してください。
5. 変換後は「改めて検証する」で、保存した計画と既存の出力を再変換せずに照合します。後日も「CLI の保存計画」から同じ計画を開けば、解析や変換を繰り返さずに検証できます。

新規のコンソール計画は、出力が一つでも名前付き出力の v6 です。計画を保存する際に処理方式も選択します。「完全デコードも実行」は CLI の `--decode` と同じ指定です。チェックを外しても保存計画の最低条件は下がりません。結果は Rust 検査、実際のデコード、デコード未要求を分けて表示します。鍵ファイルを指定する場合、解析には v1・v2 のどちらも使えます。新規コンソール計画と名前付き出力の v2・v4・v6 計画には v2 の鍵ファイル、単一出力の v1・v3・v5 計画には v1 の鍵ファイルを使います。ファイルの書式は第17章の CLI リファレンスを参照してください。

取得した計画は元のバイト列と絶対パスを保持します。入力ファイルや出力先の情報が記録されているため、非公開で保管し、別環境へ移すためにパスを書き換えないでください。鍵や実行ファイルのパスを HTTP リクエストで渡す機能はありません。鍵ファイルの内容はブラウザーで取得しません。

URL の入力は送信時に消去し、以後は「入力 URL」とだけ表示します。認証情報がクエリだけでなくパスにも含まれる場合があるため、サーバーは送信された URL を画面へ返しません。

一つのセッションで受け付ける操作は最大 32 回です。上限に達したら計画を取得し、「この変換を閉じる」で終了してから保存計画を開き直すと、既存の出力を検証できます。開始結果が不明なときは「状態を確認」で同じ操作だけを照会します。実行中や後処理を確認できない間は閉じられず、画面や言語を切り替えても実行枠は解放しません。

## ジョブの状態と停止

開始要求への応答が届かない場合は、同じ要求とジョブの状態を確認します。「状態を確認」は照会だけを行い、変換を再送しません。画面や言語を切り替えても、実行中の処理を管理する枠は解放しません。

「停止」は、そのジョブと管理中のメディアツールの終了を要求します。応答だけで後片付けが完了したとは判断せず、最終状態とログを確認してください。終了を確認できない間は実行枠を保持し、新しい処理を開始しません。サーバー終了時も新規処理の受け付けを止め、管理中の処理が停止するまで待ちます。終了できない原因を解消してから再試行してください。強制終了では、この確認は行われません。

## 保存するファイルと秘密情報

取得した原本、保存計画、結果、ログはディスクに残ります。画面を閉じても消去されず、実行中の処理が停止したと判断することもできません。これらの保存先は非公開にしてください。入力 URL は送信後に画面から消去し、鍵や取得ポリシーのファイル内容はブラウザーへ送りません。

参照（英語）: [Web の操作とプロセス管理](https://github.com/kazuhide/drmr/blob/main/docs/usage.md)、[ローカル画面と秘密情報の扱い](https://github.com/kazuhide/drmr/blob/main/docs/security.md)。

# 14. Desktop の使い方と検証範囲

## フォルダーの選択とコンソールの起動

実験的な Desktop アプリは、ローカル Web コンソールをネイティブウィンドウで開きます。保存計画、ジョブ、外部メディアツールは Web 版と共通です。

1. File → Choose input folder… でメディアと保存計画を選べる入力フォルダーを選ぶ。
2. File → Choose output location… で出力先を選ぶ。その下に所有者専用の drmr-output を作成するか、既存のものを開く。
3. 鍵や取得ポリシーを使う場合は、File → Choose private configuration folder… で非公開の設定フォルダーを選ぶ。入力・出力と別のフォルダーにする。起動前なら Clear private configuration selection で選択を解除できる。
4. File → Check external tools… で、実際の起動環境の外部ツールを確認する。
5. File → Open console で操作画面を開く。

操作画面の「新しい変換」では第13章と同じ保存計画の手順を使います。設定フォルダーを指定しなければ、鍵や取得設定を使わない Clear の処理ができます。鍵ファイルの内容をブラウザーへアップロードしません。

「元の計画ファイルを取得」を選ぶと、OS の保存ダイアログが開きます。計画には入力ファイルと出力先の絶対パスが記録されているため、非公開フォルダーへ新しい JSON ファイルとして保存してください。入力フォルダーや drmr-output の中、それらの上位フォルダーには保存できません。既定の保存先は設定フォルダーです。未選択の場合は drmr-output と同じ階層に所有者専用の drmr-plans を作成します。権限の要件を満たさない既存フォルダーや既存ファイルへの保存は拒否し、権限の変更や上書きは行いません。

保存した計画を後からアプリで開くには、起動時に保存先のフォルダーを Private configuration folder として選び、「CLI の保存計画」を開いてください。drmr-plans に保存した場合は、そのフォルダーを選びます。設定フォルダーは入力・出力のメディアフォルダーと分けてください。

CLI の `drmr verify --plan /path/to/private/drmr-plan.json` でも検証できます。入力や出力に鍵が必要な場合は、実行時の鍵も指定します。Desktop ウィンドウで保存できる添付ファイルは計画原本だけです。

保存ダイアログをキャンセルするとファイルは作られません。ダイアログを開いたまま Quit drmr を選んだ場合は、ダイアログをキャンセルして終了してください。終了要求の後に戻った保存先は使わず、取得処理の終了と作成途中のファイルの削除を待ちます。ファイルや親フォルダーが置き換わっていた場合は、その対象を削除しません。

削除の完了を確認できない場合は、保存先と、その中にある非表示の .drmr-plan-cleanup- で始まるフォルダーを確認してください。所有を確認できないファイルは保持され、同名の別ファイルを上書きして元へ戻すこともありません。見覚えのないファイルを削除せず、元の非公開フォルダーを復元してから Quit drmr を再度選んでください。

開いた後にフォルダーを変える場合は、アプリを終了して再起動します。ウィンドウは所有する loopback のポートに移動先を限定し、別 origin や新規ウィンドウへ移動しません。レポートは同じウィンドウで開き、Open console で戻れます。Web 画面へネイティブのシェル・ファイルシステム権限を渡しません。

Quit drmr を選ぶと新規処理の受け付けを止め、サーバーと子プロセスの終了確認を待ちます。終了を確認できない場合は、ウィンドウと管理中のプロセスを保持してエラーを表示します。原因を解消してから、Quit drmr を再度選んでください。OS による強制終了では、この手順は行われません。

## 起動環境とバンドル

Finder などから起動したアプリの PATH は、ターミナルの PATH と異なる場合があります。アプリはシェルの起動ファイルを実行せず、インストール先を推測して PATH に追加することもしません。必要に応じて、外部ツールに PATH が通っているターミナルからバンドルを起動してください。

```sh
"/path/to/drmr.app/Contents/MacOS/drmr" doctor
"/path/to/drmr.app/Contents/MacOS/drmr-desktop"
```

Check external tools は、実際のアプリ環境で doctor --probe と同じバージョンと機能の検査を行います。メディアが対応条件を満たすかどうかは別に確認します。診断プロセスの終了を確認できない場合は、その管理を続けるため、アプリの正常終了やコンソールの起動を保留します。

CLI と Desktop アプリは同じバンドルに含まれます。アプリは CLI の SHA-256 を確認します。外部ツール、鍵、ダウンロード、運用設定はバンドルに含みません。build-manifest.json にはソース・ツリー・バイナリのハッシュなどを記録していますが、署名による証明ではありません。

## ソースからのビルド

Tauri の OS 別前提条件と外部ツールを先に用意します。Windows は WebView2、macOS は WKWebView、Linux は WebKitGTK を使います。

```sh
RUSTUP_TOOLCHAIN=1.95.0 python3 tools/build_desktop.py --bundle
```

上は macOS/Linux シェルの例です。Windows の PowerShell は環境変数の指定方法も異なります。

```powershell
$env:RUSTUP_TOOLCHAIN = '1.95.0'
python tools/build_desktop.py --bundle
```

`--release` は最適化ビルドの指定です。`CARGO_TARGET_DIR` で専用の出力先を選べます。既定の出力先は target/desktop です。スクリプトは同じバージョンの CLI とライセンス等の通知文を用意し、未署名の macOS .app、または Windows/Linux 向けの展開して使えるディレクトリとアーカイブを作ります。既存バンドルは上書きしません。署名、公証、インストーラーの作成はこの手順に含みません。

## 動作確認と未確認部分

ネイティブのバンドル検査では、ホストと CLI の組み合わせを確認します。ダイアログや WebView の動作確認とは別です。各 OS とネイティブ WebView は、それぞれ実行時の検証が必要です。Linux GUI の依存関係には、公開ドキュメントに記載した保守上の制限もあります。

参照（英語）: [Desktop の詳細と確認範囲](https://github.com/kazuhide/drmr/blob/main/docs/desktop.md)、[依存関係のセキュリティ方針](https://github.com/kazuhide/drmr/blob/main/docs/dependency-security.md)。

# 15. 結果・終了コード・問題の切り分け

## JSON の結果

スクリプトから読む場合は `--json` を使います。メディア検査の report schema 2 は、最低検査範囲、入力確認か原本と結果の比較か、三つの Rust 検査、実際に得たデコードの証拠を分けて記録します。例えば既定の比較結果は次の形式です。`--measure` には `--json` も必要です。

```json
{
  "schema_version": 2,
  "required_depth": "native",
  "scope": "comparison",
  "passed": true,
  "checks": {
    "stream_metadata": "pass",
    "packet_payloads_equal": true,
    "packet_timing_equal": true
  },
  "decode_evidence": {
    "status": "not-checked",
    "reason": "not-requested"
  },
  "license_playback_verified": false
}
```

`passed` は必要な検査範囲を満たしたという意味です。`not-checked` はデコード内容の一致を表しません。`--decode` による入力確認は `scope: "source-admission"` と `status: "source-decoded"`、原本と結果のデコード比較は `status: "content-compared"` と実際の `equal` を記録します。入力解析の schema 2 では `verification` にこの情報を含めます。旧 `media_verified`・`decoded_content_equal` は新しいメディアレポートには出力しません。旧 schema 1 のレポートは完全デコード済みという従来の意味を保持します。メタデータのみ・マニフェストのみのレポートは別のスキーマです。

比較完了前の処理エラーは従来の schema 1 のコマンド・エラー形式で返します。比較を終えて不一致になった場合は、schema 2 の比較情報に `command`・`status`・秘密情報を含まない `error` を付けます。時刻の不一致の例は次のとおりです。hint と last_activity は実際の英語出力です。

```json
{
  "schema_version": 2,
  "required_depth": "native",
  "scope": "comparison",
  "passed": false,
  "checks": {
    "stream_metadata": "pass",
    "packet_payloads_equal": true,
    "packet_timing_equal": false
  },
  "decode_evidence": {
    "status": "not-checked",
    "reason": "not-requested"
  },
  "license_playback_verified": false,
  "command": "verify",
  "status": "failed",
  "error": {
    "code": "verification-mismatch",
    "hint": "Review every source/result check. Do not use the result as verified media.",
    "last_activity": "verification"
  }
}
```

引数の構文エラーは処理開始前に起きるため、この JSON 形式では返しません。標準エラーにヘルプなどを表示し、終了コード 2 を返します。標準出力自体に書き込めない場合も完全な JSON は返せません。その場合は、標準エラーの `result-output-unavailable` と終了コード 1 を確認してください。

## 標準エラーへの進捗表示

全コマンド共通の `--progress` で進捗の表示方法を選びます。標準出力には最後の結果だけを出すため、既存のパイプ処理はそのまま使えます。

| 指定 | 動作 |
| --- | --- |
| `auto` | 既定。人が読む結果にはテキストの進捗を表示。--json でも標準エラーが端末なら表示し、ファイルやパイプなら省略 |
| `text` | 端末では現在の作業と完了した段階を表示。ファイルやパイプには要点を絞ったログを出力。--json の結果と併用可能 |
| `json` | 一行に一つの JSON オブジェクトを記録する JSON Lines。--json の結果と併用可能 |
| `off` | 進捗を省略。結果やエラーの出力は維持 |

auto が判定するのは標準エラーの接続先です。JSON の結果だけを `>result.json` で保存しても、標準エラーが端末なら進捗を表示します。`--json` で標準エラーを `2>errors.log` などへ送る場合は、自動の進捗表示を省略します。text、json、off の明示指定が優先するため、次の例は出力をファイルやパイプに送る場合にも使えます。

```sh
drmr analyze original.mp4 --metadata-only --json --progress text
drmr analyze original.mp4 --metadata-only --json --progress json \
  >metadata.json 2>progress.jsonl
drmr analyze original.mp4 --metadata-only --progress off
```

ターミナルでは現在の作業を一行で更新し、終わった工程と所要時間をその上に残します。ダウンロード後の検査中も、取得が済んだことを確認できます。受信量は MiB などの読みやすい単位で表示します。ファイルやパイプ、画面の更新に対応しない端末では、工程の開始・終了と、間隔を空けた途中経過を記録します。ツールの呼び出しや件数の変化を逐一記録せず、変化がない場合も経過時間を定期的に表示します。

ダウンロードは取得対象の調査が終わると、完了件数と進捗率を表示できます。TS の準備では、現在処理しているメディアプレイリストの全セグメントを数えます。残り時間の概算を表示するのは、対象の総数が決まってから一秒以上経過し、その間に二件以上の取得やセグメント処理が終わってからです。そのダウンロードやプレイリストの準備にかかる残り時間であり、コマンド全体の終了予測ではありません。すべての TS セグメントを読んだ後は、進捗率や残り時間に代えて `checking segment boundaries` と表示し、プレイリストの順序で境界の整合性を調べます。

取得対象を調べている間など、総数が不明な作業では、現在の作業と経過時間を表示します。単一の MP4 を URL から取得する場合は、取得中の受信量だけを表示し、進捗率や残り時間は表示しません。取得が終わると、件数が `1/1` になります。

取得の総件数には、取得対象の調査に使った完了済みのリクエストと、重複を除いたメディアのリソースを含めます。受信量には再試行時の通信も含むため、保存したファイルのサイズと一致するとは限りません。並列数は独立した作業に対する割り当てや上限であり、すべての段階が同じ並列数で動くわけではありません。

| 処理段階 | 確認できる作業 |
| --- | --- |
| `configuration` | 設定の読み取りと検査 |
| `acquisition` | 入力リソースの取得 |
| `inspection` | メタデータや入力構造の確認 |
| `input-preparation` | 処理に使うトラックの準備 |
| `normalization` | 対応するメディア構成への整理 |
| `packaging` | 指定された形式・暗号方式での出力 |
| `verification` | 内容、時刻、出力条件の照合 |
| `publication` | 検査済みの出力を指定先へ配置 |
| `tool-check` | doctor による外部ツールの探索・診断 |
| `server-startup` | ローカルコンソールの起動と待機 |

件数などの詳しい数値をプログラムから読む場合は `--progress json` を使います。JSON に含まれる処理区間の終了は、その区間や変換全体の成功を意味しません。複数の区間が重なったり、入れ子になったりする場合もあります。最後の作業名だけで失敗原因を断定しないでください。ヘルプや版表示に架空の進捗は出しません。`--measure` は不要で、処理時間の計測レポートとは別の表示です。

進捗には URL、鍵、認証ヘッダー、入力・出力やツールのパス、生のメタデータ、外部ツールの出力を含めません。引数の解析を通過した通常の処理では、明示的な `--progress json` により標準エラーを JSON Lines で返し、失敗・停止にも固定のエラーコードを付けます。引数の構文エラーや、serve の既存の起動エラー・接続先通知は別のテキストになる場合があります。成功は進捗だけで判断せず、最後の結果と終了コードで確認してください。

## 終了コード

| 値 | 意味 |
| --- | --- |
| 0 | 選択したコマンドの確認・処理に成功。何を検証したかはコマンドによる |
| 1 | 処理、比較、必要な診断、終了確認などの失敗 |
| 2 | 引数・設定の不正 |
| 130 | 協調停止に対応する CLI で、管理中の子プロセスの終了確認まで完了 |

`doctor` の 0 は選択した診断の成功、`analyze --metadata-only` の 0 はメタデータの読み取り成功を示します。コンテンツ全体の検証に成功したという意味ではありません。

## よくある失敗

| 症状・コード | 確認すること |
| --- | --- |
| 必須ツールが見つからない | 実際の起動環境の PATH、実行権限、Desktop の Check external tools を確認する |
| `remote-metadata-not-supported` | 宣言情報が読み取り範囲にあるか確認する。URL の MP4 は HTTP Range 指定に従う配信元が必要 |
| バージョン・機能検査が失敗 | 版だけでなく、そのビルドに必要なオプションがあるか確認する |
| 鍵設定が拒否される | 所有者、600 または ACL の権限、リンク数、JSON の重複キー、不明フィールド、スキーマを確認する |
| `output-spec-invalid` | 出力一覧の schema_version が 1 か、ID が重複せず安全な名前か、形式・暗号・DRM の組み合わせが対応範囲内か確認する |
| `execution-plan-output-keys-mismatch` / `execution-plan-drm-mismatch` | version 2 の鍵ファイルの暗号化出力 ID と DRM 選択を計画に合わせる |
| `manifest-validator-unavailable` / `manifest-validator-not-configured` | Apple/DASH-IF の依存ツールと実行ファイルの場所を確認する。必須検査の省略では成功にできない |
| `input-key-required` | 原本の各 KID または正確な HLS 鍵 URI に対応した入力鍵を設定する |
| `output-key-required` / `output-key-not-used` | 暗号化出力に必要な鍵があるか、clear 出力に不要な output が残っていないか確認する |
| `presentation-window-not-supported` | 編集情報、開始位置・終端、通常速度、正確なクロック換算などが対応条件を満たすか確認する。処理を通すために原本を切り詰めない |
| `progressive-cenc-profile-not-supported` / `progressive-cbcs-profile-not-supported` | 暗号配置、IV、補助情報、入力 PSSH などが限定プロファイル内か確認する |
| `unsupported-packet-timing` | 全 PTS・DTS・継続時間を確認する。--decode ではツール版と出力も確認し、欠落値を推測しない |
| `round-trip-verification-failed` | 原本と結果のメタデータ、全符号化サンプル、時刻、要求したデコードの証拠を確認する |
| `resource-budget` | 取得できた CPU・メモリの情報、入力のサイズと寸法を確認する。ワーカー上限を増やしても解決するとは限らない |
| `media-output-limit` / `media-probe-output-limit` | 捕捉上限やレコード上限を超えていないか確認する。部分結果を成功扱いしない |
| `cleanup-unconfirmed` / `media-cleanup-unconfirmed` | 子プロセスやロックの状態を確認する。停止や処理が成功したとは扱わず、子プロセスの管理を続けて原因を解消する |

変換を繰り返す場合は、新しい出力名を選びます。URL からの取得が完了していれば、保存した `input.mp4`・`input.mpd`・`input.m3u8` を再利用できます。既存の `--download-dir` を再取得先には指定しません。作業領域を手動で削除する前に、その領域を使う子プロセスが終了したことを確認してください。

問題を報告する際は、コマンド名、固定のエラーコード、バージョン、入力と出力の対応条件、秘密情報を除いた最小限の再現手順を中心に共有してください。レポート内の鍵などがマスクされていても、原本 URL、パス、メディア名、出力シグナリングに秘密情報が残っている場合があります。

# 16. 試験結果の読み方と再現方法

この章の過去の完全デコードを伴う結果は、その検査範囲の記録です。対応する回帰試験では現在 `--decode` を明示します。後のビルドの Rust 標準検査まで動作確認済みという意味ではありません。

## 短い公開アセットによる回帰試験

主なメディア試験には、出典と権利を明記した Big Buck Bunny の十秒アセットを使います。H.264 の映像パケット 240 個と AAC 音声を含み、映像は二秒を目標に五つのセグメントへ分割します。音声はフレーム境界で分割するため、すべてのセグメントが厳密に二秒になるわけではありません。

| 試験 | 対象と確認内容 |
| --- | --- |
| 暗号方式の 16 経路 | clear/cenc/cbcs DASH と AES-128 fMP4 HLS の原本から四つの出力を作る。原本と結果に別々の鍵を使い、変換後に改めて verify を行う |
| TS→DASH の 27 経路 | clear/AES-128/SAMPLE-AES 入力 × 映像のみ・映像音声・音声のみ × clear/cenc/cbcs DASH。原本の全データ・時刻と最終宣言を確認 |
| 音声のみ TS→fMP4 HLS の 9 経路 | clear/AES-128/SAMPLE-AES 入力から clear/CBCS/AES-128 の fMP4 HLS へ変換。原本の全パケット・時刻・デコード音声を確認 |
| TS 出力の 27 経路 | 同じ三つの入力暗号方式と三つのトラック構成から clear/AES-128/SAMPLE-AES TS を作り、内容全体を比較する |
| 有限音声範囲の 54 経路 | clear/cenc/cbcs MP4 × 音声のみ・映像音声 × MP4/DASH/fMP4 HLS の九出力。音声全体と各ファイルの正確な再生範囲を確認 |
| 有限 MP4 の CLI 27 経路 | 三つの入力暗号方式の映像音声 MP4 から九出力を作り、実際の CLI で独立検証する |
| HLS の URL 入力 | 対応する clear／暗号化 TS・fMP4 を保持し、入力鍵の URI 対応と CBCS 変換を確認。配信元停止後も保存原本で再実行する |
| MP4・DASH の URL 入力の6ケース | 各形式で clear/cenc/cbcs を取得。認証とクエリ指定を必要とする配信元を使い、保存後はオフラインで計画・変換・独立検証する |
| DRM を明示した五つの出力 | CBCS DASH の Widevine/PlayReady 併用・各単独と、FairPlay の fMP4/TS HLS。コンテンツ全体を比較し、実際のマニフェストと初期化ファイルの宣言も再検証する |
| 名前を付けた複数の暗号化出力 | 一つの clear TS から DASH、fMP4 HLS、SAMPLE-AES TS、AES-128 TS を別の出力鍵で作る。単一 TS 出力の保存計画も別途実行・検証する |

CLI の 27 経路は、54 種類のうち映像と音声を持つ組み合わせを、実際のコマンドでもう一度確認する試験です。

MP4・DASH の URL 入力は、十秒の素材を使った6ケースすべてに成功しました。MP4 は映像1トラック、DASH は映像と音声の別トラックで、セグメントは二秒を目標にしています。各ケースで URL を解析した後に配信サーバーを停止し、保存したローカルファイルから計画を作成して `convert --plan` と別途の `verify --plan` を実行します。clear 入力は CBCS DASH へ、cenc/cbcs 入力は clear DASH へ変換します。パケット内容、時刻、デコード内容全体、原本のハッシュ、出力条件が一致し、元のメディアと原本マニフェストも変更されていません。標準出力・標準エラー・保存計画に、渡した認証情報や鍵が含まれないことも確認します。再実行するコマンドは `cargo test --locked -p drmr --test url_formats_media -- --nocapture` です。DRM 再生や、差し替え用 URL を使った全45例の実行結果を示す試験ではありません。

暗号化 MP4 の公開テストデータでは、映像と音声に異なる KID と鍵を使います。GPAC は一部のテストデータの事前生成にだけ使用します。CLI の実行時や CI の試験時には必要ありません。

時刻や終端を一サンプル変える、鍵を省略・交換する、ペイロード・再生フラグ・表示寸法・暗号補助情報・マニフェストを改変するなど、異常な入力や出力を拒否する試験もあります。正常に処理できることに加え、改変した結果を拒否できることも合格条件です。

DRM を明示した出力の試験では、許可する上限サイズの Widevine PSSH、DRM・KID・IV の改変、参照先とは別の場所にある同名ファイル、原本の変更も確認します。通常の fMP4 HLS は version 5 を拒否し、version 7 を受理します。外部検査用アダプターの試験では、不完全なレポート、一時コピーの変更、キャンセル、子プロセスの回収も扱います。実際に Apple・DASH-IF の検査を行うには、別途ツールの導入が必要です。通常のアセット試験には、ライセンス取得や実機再生の確認は含みません。

## OS と検証範囲

最低要件の Rust 1.95 と最新安定版を使い、Linux、macOS、Windows の CI で検査します。ネイティブ Windows では通常ユーザーで実行し、権限、Ctrl+C/Ctrl+Break、子孫プロセス、ロック、作業領域を確認します。Docker では Linux AMD64 でメディア処理と Web の HTTP 通信・停止・終了を確認します。Desktop の各 OS 向けビルドと、ネイティブ GUI での再生試験は別です。

## 三つの出力とマニフェストの確認結果

以下はソース [3f8a9a89](https://github.com/kazuhide/drmr/commit/3f8a9a89d0fc84e9699939a193baf823cafb6127) を対象に、2026年10月21日（日本時間）にローカルで完了した結果です。十秒の clear HLS/TS アセットと二秒のセグメント目標を用い、第6章の手順を実行しました。CLI は macOS ARM64、Apple mediastreamvalidator 1.25.36 は同じ環境でプレイリスト専用モードを使用しました。DASH は公式 DASH-IF v3.1.0 の XML スキーマと Schematron 規則（revision `bdf0c27a8f198da7a4bf81634bb01faf7c7c3c06`）を、ネットワーク接続を無効にした Linux ARM64 コンテナで実行しました。

| 最終出力 | 暗号と DRM の宣言 | マニフェストの実測結果 |
| --- | --- | --- |
| DASH fMP4 | CBCS、Widevine と PlayReady | Rust の内蔵検査、MPD の XML スキーマ検査、Schematron 検査が成功 |
| HLS fMP4 | CBCS、FairPlay | 内蔵検査が成功。Apple ツールで三つのプレイリストすべてを検査。メディアプレイリストの宣言は version 6 で、最低値 6 を満たす |
| HLS TS | SAMPLE-AES、FairPlay | 内蔵検査が成功。Apple ツールで三つのプレイリストすべてを検査。メディアプレイリストの宣言は version 6 で、最低値 5 を満たす |

URL から原本を取得して保存した後、ローカルの配信サーバーを停止してから計画を保存しました。変換時の検証と、別途実行した `verify --plan` の両方で、メタデータ、全符号化パケット、時刻、デコード内容全体、現在の DRM 宣言の検査に成功しました。MPD または M3U8 だけをコピーし、メディア・初期化ファイル・鍵を置かない状態でも、マニフェストのみの検証に成功しました。

内蔵検査には HLS の基本プロファイルと static DASH のプロファイル（revision `2026-10-21.1`）を使用しました。三つのマニフェストのみの検査は、実行ファイルの検索パスを空にしても成功し、外部の検証ツールに依存せず動作することを確認しました。各レポートにエラー、診断の省略、対象範囲内の未検査規則はありませんでした。メディアや配信条件など対象範囲外の項目は別途確認する必要があり、Apple 向け制作要件すべてへの適合を示す結果ではありません。

fMP4 のメディアプレイリストを version 5 に変えると検査は失敗し、version 7 では成功しました。version 6 との完全一致ではなく、使っている機能に必要な最低値で判定しています。保存した原本は変更されていません。バージョン試験で変更した出力は復元し、この実行で作ったコンテナも残っていません。これは一つのアセットと実行環境に対する確認であり、ライセンス取得、DRM 端末での再生、任意の入力での動作を保証するものではありません。

## 音声のみ TS→fMP4 HLS の確認結果

以下はソース [f0096539](https://github.com/kazuhide/drmr/commit/f0096539e698d66cc77affcd7ea891c793aa87bf) を対象に、2026年10月20日（日本時間）に完了した CI の結果です。最低要件は Rust 1.95 以上です。1.99.1 はこの実行で観測した安定版であり、固定の必須版ではありません。

| 環境と実行結果へのリンク | 実際に使用した Rust | 音声 HLS の組み合わせ | TS 回帰試験 |
| --- | --- | --- | --- |
| [Linux AMD64 / 最低版](https://github.com/kazuhide/drmr/actions/runs/39327394237/job/118317107508) | `1.95.0` | 9/9 | 34/34 |
| [Linux AMD64 / 安定版](https://github.com/kazuhide/drmr/actions/runs/39327394237/job/118317106710) | `1.99.1` | 9/9 | 34/34 |
| [macOS ARM64 / 最低版](https://github.com/kazuhide/drmr/actions/runs/39327394237/job/118317107104) | `1.95.0` | 9/9 | 34/34 |
| [macOS ARM64 / 安定版](https://github.com/kazuhide/drmr/actions/runs/39327394237/job/118317107079) | `1.99.1` | 9/9 | 34/34 |
| [Windows AMD64 / 最低版](https://github.com/kazuhide/drmr/actions/runs/39327394310/job/118316974281) | `1.95.0` | 9/9 | 34/34 |
| [Windows AMD64 / 安定版](https://github.com/kazuhide/drmr/actions/runs/39327394310/job/118316974370) | `1.99.1` | 9/9 | 34/34 |

各環境で同じ九つの暗号方式の組み合わせを検査しました。34件は `conversion_ts` の11件と `conversion_ts_output` の23件の合計で、新しい音声 HLS の回帰試験5件を含みます。九つの対応経路を六つの環境で確認した結果です。

原本の AAC 470フレームと、チャンネルごとに481,280個のデコード済みサンプルをすべて確認しました。最初の提示時刻7/5秒と再生終端857/75秒も維持しています。各出力で変換中の四つの比較と、元の TS を使った別の検証の両方に成功し、原本ファイルも変わっていません。デコード済みデータのハッシュは、各ジョブ内の同じデコード環境で照合します。ツールのビルドが異なる場合に、同じハッシュになることは前提としていません。

[Linux AMD64 の Docker 試験](https://github.com/kazuhide/drmr/actions/runs/39327394297/job/118317216488)では、clear の音声 TS から CBCS の fMP4 HLS を作る代表一経路と、元の TS に対する別の検証が成功しました。Docker で音声 HLS の全九経路を検査したという意味ではありません。これらの結果には DRM ライセンス取得や実機再生の確認は含みません。

## 実行結果の確認方法

CI のログと成果物には、実行したソースの識別値、コンパイラーと外部ツールのバージョン、検証結果を記録します。利用するバージョンに対応する実行結果を確認してください。このガイドの記述だけで、以降のバージョンや任意の入力・機器での動作を保証するものではありません。

有限音声の 54 種類は、音声のみ 27 種類と映像音声 27 種類です。実 CLI の 27 経路は後者の再確認で、追加の独立したプロファイルではありません。16 経路の試験は事前にフラグメント化した原本を基準にし、有限音声の試験は元のプログレッシブ MP4 自身の再生範囲を基準にします。原本やデコード方法が異なる試験のサンプル数を混同しません。

Desktop のビルド成功は GUI や再生の動作確認にはなりません。Docker の HTTP 試験も、ブラウザー画面を確認した結果ではありません。既知の鍵でコンテンツ全体が一致していても、DRM ライセンス取得や実機再生を確認したことにはなりません。

## 代表的な再現コマンド

開発用ソースとメディアツールを揃えた環境で、リポジトリのルートから実行します。

```sh
cargo test --locked -p drmr --test bbb_media -- --nocapture
cargo test --locked -p drmr-core --test conversion_ts_output -- --nocapture
cargo test --locked -p drmr-core --test conversion_ts_output \
  audio_hls -- --nocapture
cargo test --locked -p drmr --test conversion_ts_dash -- --nocapture
cargo test --locked -p drmr-core --test conversion_finite_outputs \
  -- --nocapture
cargo test --locked -p drmr --test conversion_finite_outputs -- --nocapture
cargo test --locked -p drmr-core --test conversion_multi_drm \
  --test conversion_large_signaling -- --nocapture
cargo test --locked -p drmr-core --test conversion_bundle_encryption \
  --test conversion_bundle_publication -- --nocapture
```

16 経路を確認する bbb_media の回帰試験では、`DRMR_CONVERSION_REPORT` にリポジトリ外の新しいファイルのパスを設定すると、JSON の結果を保存できます。テストデータのハッシュ、経路、各比較結果、時間、サンプリングしたメモリ使用量、ツールのバージョン、測定上の制約を確認してください。他の TS・有限範囲試験は、この変数ではレポートを作成しません。それぞれのテストログと CI 成果物を使ってください。

長時間のパケット列を検証するため、32×18 の小さな派生映像を 417 回繰り返し、4,170 秒・100,080 パケットを変換して独立検証する別の試験もあります。

```sh
cargo test --locked -p drmr --test long_conversion
```

この回帰試験では、長い時刻列を省略せずに読み取れることを確認します。高解像度や長尺コンテンツの処理速度、メモリ使用量全体を測るベンチマークではありません。十秒の試験も、任意の入力への対応や DRM 実機再生を保証するものではありません。

参照（英語）: [アセットの出典・ライセンス](https://github.com/kazuhide/drmr/blob/main/crates/drmr/tests/fixtures/bbb/README.md)、[暗号化 fixture](https://github.com/kazuhide/drmr/blob/main/crates/drmr/tests/fixtures/bbb/CENC.md)、[処理と試験の詳細](https://github.com/kazuhide/drmr/blob/main/docs/media-processing.md)。

# 17. CLI オプション一覧

この章に公開 CLI の引数とオプションをまとめます。すべてのコマンドで `-h` / `--help` を使えるほか、`drmr help COMMAND` でもヘルプを確認できます。CLI のバージョンは `-V` / `--version` または `drmr version` で表示します。子プロセスの管理に使う内部用の起動引数は、利用者向けのインターフェースに含めません。

## 全コマンド共通の進捗オプション

| オプション | 意味・既定値 |
| --- | --- |
| `--progress MODE` | auto、text、json、off。既定は auto。進捗は標準エラー、結果は標準出力。具体例と JSON Lines の適用範囲は第15章 |

## FILE・PATH に指定するもの

`FILE` や `PATH` は、その文字列をそのまま入力する指定ではありません。使うファイルのパスに置き換えます。相対パスはコマンドを実行するディレクトリを基準にします。パスに空白がある場合は引用符で囲んでください。JSON の中身をオプションの直後に直接書く方式ではありません。

同じ `FILE` 表記でも、設定 JSON、生成済みの計画、実行プログラムでは渡すものが異なります。代表的な指定例と、内容の説明は次のとおりです。

| 指定例 | 用意するもの・詳しい説明 |
| --- | --- |
| `--fetch-policy config/fetch.json` | URL の取得条件を書く JSON。第8章の[取得ポリシーの書式と使用例](https://kazuhide.github.io/drmr/ja/guide/#url-input)を参照。追加 origin・認証ヘッダー・取得量などを設定する |
| `--keys-file config/keys.json` | 鍵を読み込む JSON。第5章の[入力鍵・出力鍵の構造例と保護方法](https://kazuhide.github.io/drmr/ja/guide/#key-configuration)を参照。通常の単一出力は version 1。--outputs または名前付き出力の version 2・4・6 の計画を使う場合は、出力が一つでも version 2。出力一覧や取得ポリシーとは別ファイル |
| `--outputs config/outputs.json` | 利用者が作る version 1 の出力一覧 JSON。第6章の[複数出力の設定例](https://kazuhide.github.io/drmr/ja/guide/#inspect-plan)を参照。出力 ID・形式・暗号方式などを指定し、鍵は含めない |
| `--plan conversion.plan.json` | `plan --save conversion.plan.json` で生成した実行計画 JSON。第6章の[計画の保存と実行](https://kazuhide.github.io/drmr/ja/guide/#inspect-plan)を参照。鍵 JSON や出力一覧を代わりに指定することはできない |
| `--packager-bin /opt/media-tools/packager` | インストール済みの Shaka Packager 実行ファイル。設定 JSON やインストーラーではない。第4章の[依存ツールの準備](https://kazuhide.github.io/drmr/ja/guide/#installation)を参照 |
| `--hls-validator-bin /opt/media-tools/mediastreamvalidator` | macOS に用意した Apple の検証プログラム。第9章の[外部マニフェスト検査の準備](https://kazuhide.github.io/drmr/ja/guide/#verification)を参照 |
| `--dash-validator-bin /opt/media-tools/php` | ローカルの DASH-IF アダプターを動かす PHP 実行ファイル。MPD や DASH-IF のディレクトリではない。第9章の[検査ツールと DRMR_DASH_IF_ROOT](https://kazuhide.github.io/drmr/ja/guide/#verification)を参照 |

通常の `analyze` は、どちらの版の鍵 JSON からも入力鍵を読み込めます。出力設定がある場合も、解析には入力鍵だけを使います。`--metadata-only` では鍵ファイルを指定できません。

`--packager-bin` の既定値 `packager` は、起動環境の PATH から実行ファイルを探す指定です。別の場所に配置した場合は、その実行ファイルのパスを渡します。例えば macOS/Linux では次のように指定します。入力ファイル、出力先、鍵設定、実行ファイルの場所は自身の環境に合わせて置き換えてください。

```sh
drmr convert original.mp4 --to cbcs --format mp4 \
  --output protected.mp4 --keys-file config/keys.json \
  --packager-bin "/opt/media-tools/packager"
```

Windows では、例えば `--packager-bin 'C:\media-tools\packager.exe'` と指定します。実行ファイル自体のパスを指定してください。オプションの値にシェルコマンド全体や追加引数は含めません。CLI アーカイブには Shaka Packager は同梱されていません。

`NEW_FILE`・`NEW_PATH`・`NEW_DIR` は、これから作る保存先です。読み込む `FILE` とは異なり、既存の出力先を上書きするための指定ではありません。秘密情報を含むファイルは、鍵設定の保護手順に従って作成してください。

## doctor

| オプション | 意味・既定値 |
| --- | --- |
| `--json` | 一つの機械可読 JSON レポート |
| `--probe` | バージョンとヘルプに記載されたオプションを、各呼び出し三秒の上限で検査。既定は実行ファイルの探索のみ |
| `--route PROFILE` | 確認するツールの組み合わせ。既定は all |

PROFILE に指定できるのは `all`、`progressive-mp4`、`fragmented-mp4`、`dash-presentation`、`fmp4-hls-presentation`、`mpeg-ts-hls`、`mpeg-ts-sample-aes-hls`、`mpeg-ts-hls-ts` です。変換結果に表示される処理経路名を、そのまま doctor に指定できるとは限りません。TS→DASH を含めてツール一式を診断する場合は all を使えます。

## analyze

| 引数・オプション | 意味・既定値 |
| --- | --- |
| `INPUT` | ローカル MP4・DASH・HLS、または HTTP(S) URL。原本全体の URL 取得には[対応条件](#url-input)がある |
| `--metadata-only` | 鍵や外部メディアツールを使わず、宣言情報を読む。原本の保存やコンテンツ全体の検証はしない。第6章を参照 |
| `--download-dir NEW_DIR` | 原本全体を取得する URL 解析に必須。新規の原本保存先。--metadata-only とは併用不可 |
| `--fetch-policy FILE` | 例 config/fetch.json。URL 専用の取得条件 JSON。--metadata-only と併用可能。[書式・権限・使用例](https://kazuhide.github.io/drmr/ja/guide/#url-input) |
| `--keys-file FILE` | 原本の復号に必要な入力鍵の JSON。--metadata-only とは併用不可。[構造例と保護方法](https://kazuhide.github.io/drmr/ja/guide/#key-configuration) |
| `--packager-bin FILE` | 互換性のための Shaka Packager 指定。Rust の原本検査では起動しない。既定 packager。--metadata-only とは併用不可。[配置と最低要件](https://kazuhide.github.io/drmr/ja/guide/#installation) |
| `--max-workers N` | 原本全体の検査の同時作業上限 1-64。既定は CPU・メモリから自動選択。--metadata-only とは併用不可 |
| `--decode` | 原本全体のデコードも行う。--metadata-only とは併用不可 |
| `--json` | --metadata-only はメタデータレポート、それ以外は入力解析レポート。どちらも変換結果の検証ではない |
| `--measure` | 数値計測を追加。--json が必要。--metadata-only とは併用不可 |

ローカル入力に取得用オプションを付けると拒否します。URL のメタデータ確認に取得先は不要ですが、原本全体の解析には新規の取得先が必要です。

## plan

| 引数・オプション | 意味 |
| --- | --- |
| `INPUT` | 変換先の候補と条件を確認するローカル入力 |
| `--to TARGET` | INPUT を使い --outputs がない場合に必須。clear、cenc、cbcs、hls-aes128、hls-sample-aes |
| `--format FORMAT` | mp4、dash、hls、hls-ts。--save なしでは複数候補、単一出力の保存計画では一形式。省略時は MP4/DASH/HLS に従い、HLS は fMP4。--outputs とは併用不可 |
| `--processing MODE` | auto・segments・stitch。--save が必要。[処理方法](https://kazuhide.github.io/drmr/ja/guide/#verification) |
| `--outputs FILE` | version 1・2 の[出力一覧 JSON の構造例](https://kazuhide.github.io/drmr/ja/guide/#inspect-plan)。--save が必要。--to・--format を置き換え、version 6 の実行計画を作る |
| `--decode` | 完全デコードを計画の最低条件にする。--save が必要 |
| `--json` | 実行せず、バージョン付き計画を JSON で表示 |
| `--save NEW_FILE` | 原本を確認して計画 JSON を新規保存。例 `conversion.plan.json`。INPUT と --output に加え、--to または --outputs が必要。[保存例](https://kazuhide.github.io/drmr/ja/guide/#inspect-plan) |
| `--output NEW_PATH` | 計画に記録する新規出力先。--save なしでは拒否 |
| `--keys-file FILE` | 原本確認の入力鍵 JSON。単一出力は version 1、--outputs は version 2。--save 時のみ。[鍵の構造例](https://kazuhide.github.io/drmr/ja/guide/#key-configuration) |
| `--packager-bin FILE` | 互換性のための Shaka Packager 指定。Rust の原本検査では起動しない。--save 時のみ。既定 packager。[準備方法](https://kazuhide.github.io/drmr/ja/guide/#installation) |
| `--max-workers N` | 原本確認の同時作業上限 1-64。--save 時のみ |
| `--measure` | 標準出力に計測を追加。--json と --save が必要。保存ファイル自体は実行用の計画 |

## convert

| 引数・オプション | 意味・既定値 |
| --- | --- |
| `INPUT` | 対応するローカル原本、または取得条件を満たす MP4・static DASH・完結した HLS の URL。暗号化入力には入力鍵が必要。--plan 時は省略。[URL の対応条件](#url-input) |
| `--to TARGET` | --plan・--outputs がない場合に必須。出力保護方式 |
| `--format FORMAT` | 一つの出力形式。省略時は MP4/DASH/HLS に従い、HLS は TS 入力でも fMP4 |
| `--processing MODE` | auto・segments・stitch。--plan とは併用不可。[処理方法](https://kazuhide.github.io/drmr/ja/guide/#verification) |
| `--outputs FILE` | version 1・2 の[出力一覧 JSON](https://kazuhide.github.io/drmr/ja/guide/#inspect-plan)。すべての結果を一つの新規出力ルートに作る。--to・--format・--plan とは併用不可 |
| `--output NEW_PATH` | --plan がない場合に必須。新規ファイルまたは新規ディレクトリ |
| `--plan FILE` | version 1・2・3・4・5・6 の[生成済み計画 JSON](https://kazuhide.github.io/drmr/ja/guide/#inspect-plan)を実行。原本・出力条件の上書きは拒否 |
| `--keys-file FILE` | 所有者専用の鍵 JSON。通常は version 1。--outputs または名前付き出力の version 2・4・6 の計画では、出力が一つでも version 2。[鍵と DRM 情報の構造例](https://kazuhide.github.io/drmr/ja/guide/#key-configuration) |
| `--download-dir NEW_DIR` | URL 入力に必須。取得した原本の保存用に新しく作るディレクトリ |
| `--fetch-policy FILE` | 例 `config/fetch.json`。アクセス権を制限した URL 取得用 JSON。[設定項目と例](https://kazuhide.github.io/drmr/ja/guide/#url-input) |
| `--packager-bin FILE` | Shaka Packager の実行ファイル。既定 packager。[準備方法](https://kazuhide.github.io/drmr/ja/guide/#installation) |
| `--hls-validator-bin FILE` | 要求した HLS 検査用の Apple 実行ファイル。既定 mediastreamvalidator。[検査ツールの準備](https://kazuhide.github.io/drmr/ja/guide/#verification) |
| `--dash-validator-bin FILE` | ローカル DASH-IF アダプターを実行する PHP 実行ファイル。既定 php。[配置先と設定](https://kazuhide.github.io/drmr/ja/guide/#verification) |
| `--max-workers N` | メディアの同時処理数の上限 1-64。既定は CPU・メモリから自動選択 |
| `--decode` | デコード内容全体の比較を追加する。保存計画の最低条件を強められる |
| `--json` | スキーマのバージョンを含む一つの結果。auto は標準エラーが端末の場合だけ進捗を表示。--progress text/json/off の明示指定が優先 |
| `--measure` | 数値の計測情報を追加。--json が必要 |

汎用 CLI に ffmpeg-bin/ffprobe-bin の独自フラグはありません。FFmpeg/ffprobe は起動環境の PATH を用います。

`--plan` は INPUT、--to、--format、--processing、--outputs、--output、--download-dir、--fetch-policy と併用できません。鍵・検証ツールの場所・並列数・レポート指定は実行時の設定として渡せます。各出力に保存したマニフェスト検証方針を実行時に上書きすることはできません。

## verify

| 引数・オプション | 意味・既定値 |
| --- | --- |
| `OUTPUT` | 比較する結果の MP4、MPD、M3U8。--plan 時は省略 |
| `--source INPUT` | --plan・--manifest-only がない場合に必須。元の入力。結果から原本を推測しない |
| `--plan FILE` | [生成済み計画 JSON](https://kazuhide.github.io/drmr/ja/guide/#inspect-plan)に記録した原本と出力条件を照合し、コンテンツ全体の一致を確認。OUTPUT・--source とは併用不可 |
| `--manifest-only` | Rust の内蔵 Linter による HLS/DASH 検査。media_verified は false。--plan・--source・--keys-file とは併用不可 |
| `--external-validator` | Apple／DASH-IF の検査も追加する。--manifest-only が必要 |
| `--manifest-profile PROFILE` | 内蔵検査のプロファイル。--manifest-only が必要。既定 base、apple-vod は HLS のみ |
| `--keys-file FILE` | 原本と結果の鍵 JSON。名前付き出力の version 2・4・6 の計画では、出力が一つでも version 2。それ以外は version 1。[鍵の構造例](https://kazuhide.github.io/drmr/ja/guide/#key-configuration) |
| `--packager-bin FILE` | Shaka Packager の実行ファイル。既定 packager。[準備方法](https://kazuhide.github.io/drmr/ja/guide/#installation) |
| `--hls-validator-bin FILE` | Apple の実行ファイル。既定 mediastreamvalidator。[検査ツールの準備](https://kazuhide.github.io/drmr/ja/guide/#verification) |
| `--dash-validator-bin FILE` | ローカル DASH-IF アダプターを実行する PHP 実行ファイル。既定 php。[配置先と設定](https://kazuhide.github.io/drmr/ja/guide/#verification) |
| `--max-workers N` | 上限 1-64。利用可能な資源に応じて実際の並列数は減る場合がある |
| `--decode` | デコード内容全体の比較を追加する。--manifest-only とは併用不可。保存計画の最低条件を強められる |
| `--json` | 一つの独立検証レポート |
| `--measure` | 計測を追加。--json が必要 |

verify は検証用のコマンドです。target や format を指定して変換結果を作り直すことはできません。

単一出力の計画検証では plan_id、source_unchanged、output_matches_plan を返し、コンテンツ全体の比較結果を verification の下に入れます。成功には source_unchanged、output_matches_plan、verification.passed がすべて true である必要があります。PlayReady 情報を明示した場合は、現在の出力とも照合します。名前付き出力の結果は各出力のメディア・暗号方式・シグナリング・外部検査を一覧で返し、全体の passed も示します。内容が一致していても、それだけで計画した暗号方式や DRM 宣言を確認できるわけではありません。どちらのモードもライセンス取得を伴う再生の確認は行いません。

## version

`drmr version` または `drmr --version` で製品のバージョンを表示します。個別のオプションはありません。

## serve

| オプション・環境 | 意味・既定値 |
| --- | --- |
| `--static-dir DIR` | 組み込み画面の代わりに配信する信頼済み UI ディレクトリ |
| `--cli FILE` | ジョブを実行する同じ版の drmr 実行ファイル。設定 JSON ではない。[Web の起動方法](https://kazuhide.github.io/drmr/ja/guide/#web-console) |
| `DRMR_INPUT_ROOT` | 必須。メディアと保存計画を選べる入力フォルダー |
| `DRMR_OUTPUT_ROOT` | 必須。結果・取得した原本・ログの保存先 |
| `DRMR_BIND_HOST` | 既定は loopback。リモート公開を想定しない |
| `DRMR_BIND_PORT` | 既定 8080 |
| `DRMR_MAX_WORKERS` | 保存計画の各処理に使う並列数の上限 |
| `DRMR_COMMON_CONFIG_ROOT` | 鍵と取得ポリシーを置く非公開フォルダー。入力・出力と分ける。[設定の選び方](https://kazuhide.github.io/drmr/ja/guide/#web-console) |

# 18. 資料とこのガイドの更新

## バージョン別の公開資料

以下のリンク先は公開 main の資料です。利用中のバージョンが異なる場合は、該当するコミットか同梱のドキュメントを確認してください。

- [README・導入](https://github.com/kazuhide/drmr/blob/main/README.md)
- [使い方と設定](https://github.com/kazuhide/drmr/blob/main/docs/usage.md)
- [入出力対応表](https://github.com/kazuhide/drmr/blob/main/docs/conversion-support.md)
- [処理方法・時刻・コンテンツ全体の検証](https://github.com/kazuhide/drmr/blob/main/docs/media-processing.md)
- [処理経路と解析・計画で確認できること](https://github.com/kazuhide/drmr/blob/main/docs/processing-routes.md)
- [複数出力と DRM 設定](https://github.com/kazuhide/drmr/blob/main/docs/multiple-outputs.md)
- [外部ツールによるマニフェスト検証](https://github.com/kazuhide/drmr/blob/main/docs/manifest-validation.md)
- [必要なバージョンと依存関係](https://github.com/kazuhide/drmr/blob/main/docs/requirements.md)
- [鍵とローカルセキュリティ](https://github.com/kazuhide/drmr/blob/main/docs/security.md)
- [資源制限と測定](https://github.com/kazuhide/drmr/blob/main/docs/resources.md)
- [Docker](https://github.com/kazuhide/drmr/blob/main/docs/docker.md)
- [Windows](https://github.com/kazuhide/drmr/blob/main/docs/windows.md)
- [Desktop](https://github.com/kazuhide/drmr/blob/main/docs/desktop.md)
- [依存関係のセキュリティ](https://github.com/kazuhide/drmr/blob/main/docs/dependency-security.md)

## 規格・一次資料

形式の一般的な説明と、drmr が実装・検証したプロファイルは別です。次の資料だけから、drmr や特定プレーヤーの対応を推定しないでください。

- [W3C: ISO Common Encryption の MP4 ストリーム説明](https://www.w3.org/TR/eme-stream-mp4/)
- [RFC 8216: HLS のメディア形式・暗号化宣言](https://www.rfc-editor.org/rfc/rfc8216.html)
- [Apple: HLS Sample Encryption](https://developer.apple.com/library/archive/documentation/AudioVideo/Conceptual/HLS_Sample_Encryption/)
- [W3C: MP4 初期化セグメントと編集情報](https://www.w3.org/TR/mse-byte-stream-format-isobmff/)

ホームページにも入出力の対応表を掲載しています。サイトとこのガイドは公開 main を基準に更新します。

このガイドは、現在の実装、ヘルプ、公開ドキュメント、動作確認の結果に基づき、使い方と対応条件をまとめたものです。本文中のパス、example ドメイン、山括弧で示した鍵の値は説明用です。実行する前に、自分のコンテンツと環境に合うパス、権限、設定に置き換えてください。
