---
title: ユーザーガイド
updated: 2026-10-21
version: 0.1.0
---

# 1. はじめに

drmr は映像・音声の符号化データを維持しながら、ファイルの格納形式、配信形式、暗号化方式を変更するツールです。復号に必要な鍵を持つ技術者が、管理下のオンデマンドコンテンツを解析・変換・検証する用途を想定しています。画質を変更するエンコーダーや DRM ライセンスを取得するクライアントの機能はありません。

このガイドでは drmr 0.1.0 の操作方法と対応条件を説明します。更新日は 2026-10-21 です。公開ソースとヘルプに基づいているため、別のバージョンを利用する場合は同梱のドキュメントも確認してください。リポジトリ内の関連ドキュメントは英語で記載しています。

## 目的別のコマンド

| 目的 | コマンド | わかること・実行すること |
| --- | --- | --- |
| 外部ツールを探す | `doctor` | 起動環境の PATH に必要な実行ファイルがあるか |
| ツールのバージョンと機能を確認する | `doctor --probe` | バージョンとヘルプに必要なオプションがあるか。メディア処理はしない |
| 入力の種類と暗号化方式の宣言を確認する | `inspect` | ローカル MP4・DASH・HLS のメタデータ。復号やサンプル自体の検証はしない |
| 入力データが対応条件を満たすか確認する | `analyze` | 原本全体の解析と入力条件の確認。URL 入力は指定先に保存する |
| 変換先の候補と条件を確認する | `plan` | 指定した形式・暗号方式の組み合わせと実行時に必要な条件 |
| 変換計画を保存する | `plan --save` | 原本のハッシュと出力条件を記録する。複数出力の場合は DRM の選択とマニフェスト検証方針も含む |
| コンテンツを変換する | `convert` | 入力データの条件を確認し、変換と照合を終えてから指定した場所に出力する |
| 原本と結果を改めて比べる | `verify` | 原本を独立に読み直し、内容・復号結果・時刻などを照合する |
| 完成したマニフェストだけを調べる | `verify --manifest-only` | Rust の内蔵 Linter で HLS/DASH の宣言を検査する。--external-validator で外部検査も追加できる。メディアの比較や復号は行わない |

原本を確認してから変換計画を保存する場合は、`analyze`、`plan --save`、`convert --plan`、`verify --plan` の順に使います。メタデータだけを短時間で確認するには `inspect` を使います。計画を保存せずに `convert INPUT` を実行することもできます。HLS の汎用変換でも、事前に `analyze-hls` を実行する必要はありません。

## 最短の実行例

最初の動作確認には、対応条件を満たす非暗号化のローカル MP4 を使います。出力先の親ディレクトリをあらかじめ作成し、出力ファイルには既存のファイルと重複しない名前を指定してください。原本は変更されません。

```sh
drmr --version
drmr doctor --probe --route progressive-mp4
drmr inspect original.mp4 --json
drmr analyze original.mp4 --json
drmr plan original.mp4 --to clear --format mp4 \
  --output clear.mp4 --save clear.plan.json --json
drmr convert --plan clear.plan.json --json
drmr verify --plan clear.plan.json --json
```

入力が暗号化されている場合や出力を暗号化する場合は、第5章の手順でアクセス権を制限した鍵設定ファイルを渡します。この例でも、入力が対応範囲外なら処理は拒否されます。入力の確認や計画の保存に成功しても、その後の変換が成功するとは限りません。実行時に原本、鍵、ツール、指定した出力条件を改めて確認します。

主な操作手段は CLI です。依存ツールもまとめて実行したい場合は Docker を使えます。ローカル Web コンソールと Desktop は実験的な操作画面であり、CLI の全機能には対応していません。

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

これらの違いは W3C の MP4 暗号化説明、RFC 8216 の暗号化宣言、Apple の SAMPLE-AES 説明にも記載されています。一次資料のリンクは第20章にまとめています。

## DRM システムと暗号方式の違い

Widevine、PlayReady、FairPlay は、鍵の取得や利用条件、プレーヤーとの連携を含む DRM システムです。drmr は既知の鍵を使ってメディアを変換し、対応する経路では必要なシグナリングを格納します。シグナリングとは、プレーヤーが DRM システムや鍵の場所を判断するための付加情報です。

複数の出力を作る場合も、共通の `plan`、`convert`、`verify` を使います。出力一覧で、Widevine/PlayReady 向けの DASH、FairPlay 向けの HLS をそれぞれ指定できます。PSSH は Protection System Specific Header の略で、DRM の初期化情報を格納するデータです。鍵設定と出力一覧の具体例は第5・6章に示します。従来の単一出力 PlayReady 設定と HLS 保存経路も引き続き使えます。

暗号方式の変換、シグナリングの整合性確認、ライセンス取得、実機再生はそれぞれ別の確認事項です。メディアの検証に成功しても、対象機器でそのコンテナ形式・コーデック・暗号方式・DRM の組み合わせを再生できるとは限りません。サービスで配信する前に、実際に使うライセンスサーバーと機器で確認してください。

`cens` と `cbc1` は解析で識別できる場合がありますが、現在の変換先ではありません。対応表の「対象外」は drmr の現在の実装・検証範囲を表し、その形式で技術的に実現不可能という意味ではありません。

# 3. 入力と出力の対応表

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

出力ごとに DRM を指定する変換は、対応条件を満たす clear HLS/TS と従来の条件付き clear MP4 を入力にできます。このプロファイルでは、既存の DRM 初期化情報を含む入力や、それ以外の構成の入力は拒否します。DRM 情報を明示的に指定しない変換には、引き続き上表の対応範囲が適用されます。

| 出力一覧の format と target | DRM システム | このプロファイルの入力 |
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

TS 入力の対象は配信内容が確定した HLS です。番組構成が一つに固定されている必要があります。プログレッシブ H.264 映像を一つ含み、48 kHz AAC-LC 音声も一つまで含められます。同じ一番組・48 kHz AAC-LC の条件なら、音声のみの入力から DASH、fMP4 HLS、TS を出力することもできます。

TS パケットは 188 バイトです。映像の復号設定を表す SPS/PPS は途中で変更できません。AAC は 7 バイトの ADTS ヘッダーを使い、チェックサムを持たず、一単位に音声フレームを一つ含む必要があります。複数番組、設定変更、壊れたパケット、時刻カウンターの周回、不連続、インターレース映像、44.1 kHz AAC、未対応の記述子などは拒否します。

SAMPLE-AES 入力では暗号方式を途中で変更できません。鍵形式は `identity` または `com.apple.streamingkeydelivery` の一つで、指定する形式版は 1 です。異なる鍵形式の同時宣言は拒否します。非ゼロの音声初期遅延も対象外です。

TS 出力で映像と音声がある場合は、それぞれ別の TS プレイリストと、それを関連付けるマスタープレイリストを作ります。一つの TS セグメント内に両方を多重化する出力は、現在の実装では作りません。

## 共通の対象外と上限

ローカル DASH は静的な一つの Period、連続した有限の SegmentTimeline、対応する相対参照を持つ必要があります。ライブ、動的 DASH、複数 Period、バイト範囲、不連続、保持できないイベントやレンディション情報などは対象外です。汎用変換で字幕を一律に保持することもできません。対応する字幕には別の HLS スナップショット経路があります。

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
| `ffmpeg` / `ffprobe` | 9.0.1 | メディアの読み取りと復号、復号後のデータ・パケット・時刻の確認 |
| `packager` | Shaka Packager 3.8.0 | MP4 サンプル暗号化、格納形式の変更、セグメント化 |
| `mp4dump` | Bento4 1.6.0、build 641 | 準備済み DASH などの MP4 構造確認 |

必要な実行ファイルを信頼できる配布元から入手し、シェルがプログラムを検索する PATH に追加してください。CLI アーカイブと Desktop バンドルには、これらのメディアツールは含まれません。Docker イメージには含まれます。

最低要件より新しいバージョンでも、必要なオプションを備えていないビルドや、全パケットの時刻を出力できないビルドは使えません。Bento4 は起動時のバージョン表示に build 番号を含めません。`doctor --probe` の `build-unverified` だけでは build 641 以上と判断できないため、配布元でも確認してください。

## doctor の使い分け

```sh
drmr doctor
drmr doctor --probe --route all --json
drmr doctor --probe --route dash-presentation
```

既定の `doctor` は実行ファイルの探索だけを行い、外部ツールを起動しません。`--probe` はバージョンとヘルプを、各呼び出し 3 秒、標準出力・標準エラー各 1 MiB の上限で確認します。生の出力はレポートに含めません。メディア、鍵、ライセンス、再生機器は検証しません。

`--route` では、どの処理経路に必要なツールを確認するかを選びます。`progressive-mp4` では FFmpeg と ffprobe、`prepared-dash` と `all` では Bento4 も必要です。他の処理経路では必要に応じて Shaka Packager を確認します。選んだ経路で使わないツールの検査は任意です。ただし、任意の検査でも起動したプロセスの終了を確認できない場合は、準備完了と判定しません。

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

ファイルの版番号は役割ごとに異なります。出力一覧の JSON は version 1、複数出力の鍵ファイルと実行用計画は version 2 です。従来の単一出力の鍵ファイルと計画は version 1 のままです。鍵ファイルが共有入力鍵を持てることは、明示的な複数 DRM 出力で暗号化入力を受け入れるという意味ではありません。

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

一部のバッファを消去しても、Secure Enclave などの隔離環境で処理するわけではありません。外部プロセス、ブラウザー、メモリ管理機構が作るコピー、スワップ、クラッシュダンプ、バックアップまで一括で消去する保証はありません。ファイルを削除しても、ストレージやスナップショットからデータが完全に消えるとは限りません。

ライセンス URL、HLS 鍵 URI、PSSH に含めた URL は、意図して出力メディアへ書かれる場合があります。Base64 や十六進表現は暗号化ではありません。公開する予定のない認証トークンをこれらの値へ含めないでください。入力 URL、原本プレイリスト、保存したダウンロードにも署名や認証情報が含まれることがあります。

信頼できる OS アカウント、外部メディアツール、ブラウザーを使い、作業ディレクトリも鍵と同様に保護してください。ログを共有する前に、マスクの対象外となる名前や外部ツールの出力も確認してください。Rust API が返す外部ツールの出力バッファには秘密情報が残る可能性があるため、そのままログに記録しないでください。

参照（英語）: [鍵とローカルセキュリティ](https://github.com/kazuhide/drmr/blob/main/docs/security.md)。

# 6. inspect と plan

## inspect による宣言と初期化情報の確認

```sh
drmr inspect media.mp4
drmr inspect source/manifest.mpd --json
drmr inspect source/master.m3u8 --json
```

入力形式は内容から判定します。JSON は `schema_version: 1` を持ち、形式、トラック・サンプル記述ごとの保護方式、既知の KID、DRM システム名、`findings` の説明トークンなどを返します。コンテンツキー、IV、ライセンス URL、鍵 URL は含めません。

MP4 では初期化メタデータを読み、メディアのペイロード全体は走査しません。DASH/HLS では宣言情報を読みます。いずれも `media_verified: false` であり、全サンプルの復号・デコードや再生を確認するものではありません。未対応または不完全な暗号情報は `unknown` のままです。HLS の SAMPLE-AES 宣言だけで MP4 の暗号方式を断定することはありません。

HLS の子プレイリストは、入力ディレクトリ配下のローカル参照だけをたどります。リモート、欠落、循環、ツリー外参照は解決しません。プレイリスト・メディア・鍵をネットワークから取得しません。

解析の主な上限は、一マニフェスト 4 MiB、合計 16 MiB、32 プレイリスト、128 参照、トラックごとに 64 KID、MP4 の movie メタデータ 16 MiB です。変換本体や URL 取得の上限とは別です。

## analyze による入力データ全体の確認

```sh
drmr analyze source/master.m3u8 --json
drmr analyze encrypted.mp4 --keys-file config/keys.json --json
```

`analyze` は変換と同じ処理エンジンで原本全体を読み、符号化済みパケット、復号後の内容、時刻を確認します。`input_admitted: true` は、確認した入力が対応条件を満たしていることを示します。出力形式の候補が表示されても、指定する暗号方式との組み合わせは計画時に確認します。変換結果や DRM 再生の動作確認を済ませたという意味ではありません。ローカル MP4・DASH・HLS が対象です。URL 入力の保存手順は第8章に示します。

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

鍵、署名付き入力 URL、認証ヘッダー、ライセンス URL、HLS 鍵 URI、実行プログラムの引数は計画に保存しません。鍵は実行・検証時に `--keys-file` で渡します。version 1 の単一出力計画では、DRM シグナリングの選択も実行時の設定です。version 2 では DRM システムの選択とマニフェスト検証方針を計画に固定し、鍵やアドレスは引き続き別の設定で渡します。`verify --plan` は原本のハッシュと実際の出力の形式・暗号方式を照合し、原本と結果のコンテンツ全体および時刻が一致するか確認します。ライセンス取得や実機再生は別途検証が必要です。

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

この version 2 の計画は、一つの原本ファイル集合と、各出力の形式・暗号方式・DRM 選択・外部マニフェスト検証方針を記録します。原本のハッシュと出力ルートも固定します。`plan` の `--outputs` には `--save` が必要で、`--to`・`--format` とは併用しません。

`conformance: "required"` を指定すると、変換時に加え、後から `verify --plan` を実行する際にも外部マニフェスト検査が必須になります。`"none"` を指定するかフィールドを省略すると、外部検査は要求しません。どちらの場合も内蔵 Linter の検査とコンテンツ全体の検証を行います。第9章のツールを実行前に準備してください。計画の保存に成功しても、そのツールが使えることまでは確認できません。

従来の操作との互換性のために残している `plan --mpd MPD` は、準備済み DASH のレンディションとセグメント数を読む別のモードです。汎用の `plan INPUT --to ...` とは必要な入力ファイルの配置が異なります。具体例は「準備済み DASH とバッチ処理」を参照してください。

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

Shaka Packager で音声トラック全体を分離し、新しいセグメントを作ります。FFmpeg/FFprobe は解析、復号、ハッシュの照合に使います。原本の PES の順序、全 AAC フレーム、絶対時刻での PTS・DTS・継続時間を維持し、照合の基準にします。二秒のセグメント目標に合わせて AAC フレームを切り落とすことはしません。回帰試験に使う TS 入力は 470 フレームで、一チャンネルあたり 481,280 サンプルを保持します。再生範囲を 480,000 サンプルに指定したプログレッシブ MP4 原本とは、再生する範囲が異なります。

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

対応する clear MP4 から CBCS DASH を作る場合に限り、次のように signaling を持つ出力設定を使えます。鍵はご自身の値へ置き換えてください。

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

URL から原本を取得して解析し、変換計画を保存してから実行するには、次の四つのコマンドを使います。第5章の鍵設定 `config/keys.json` は原本と出力のディレクトリの外に用意してください。入力が暗号化されている場合は入力鍵を、CBCS 出力には出力鍵を設定します。

```sh
drmr analyze "https://example.com/vod/master.m3u8" \
  --download-dir ./saved-source --keys-file config/keys.json --json
drmr plan ./saved-source/input.m3u8 --to cbcs --format dash \
  --output ./converted-dash --save ./conversion.plan.json \
  --keys-file config/keys.json --json
drmr convert --plan ./conversion.plan.json --keys-file config/keys.json --json
drmr verify --plan ./conversion.plan.json --keys-file config/keys.json --json
```

URL 入力では、`--download-dir` に新しい保存先を指定します。親ディレクトリはあらかじめ作成してください。計画ファイルと出力先にも、原本とは別の新しい場所を選びます。取得済みのファイルを上書きしたり、既存のディレクトリを自動的に再取得先として使ったりすることはありません。ローカル入力では `--download-dir` や `--fetch-policy` を指定しません。

配信内容が確定し、対応条件を満たす HLS（TS/fMP4）を取得できます。clear に加え、AES-128 の TS/fMP4、SAMPLE-AES の TS、CENC/CBCS の fMP4 に対応します。取得したデータには、ローカル入力と同じコーデック・トラック・暗号・時刻の条件を適用します。リモート DASH、単独 MP4、ライブ、バイト範囲による参照は現在の取得機能の対象外です。

AES-128 と SAMPLE-AES TS の `input.hls_keys[].uri` は、リダイレクト後のプレイリスト URL を基準に解決した絶対 URI と完全一致させます。例えば `https://example.com/vod/media.m3u8` 内の `keys/key.bin` は `https://example.com/vod/keys/key.bin` になります。別のディレクトリにある同じ相対名を、同じ鍵と決めつけません。絶対 URI の `skd:` はそのまま識別子として扱います。サンプル暗号化された fMP4 は、従来どおり `input.keys` に KID と鍵を指定します。鍵 URI やライセンスへのリクエストは行いません。clear 入力から clear を作る場合は鍵ファイルを省略できます。

元のプレイリストとリソースを保存し、ローカル参照にした `saved-source/input.m3u8` を作ります。原本プレイリストのバイト列は別に保持し、ローカル版ではメディア参照と鍵識別 URI の解決だけを書き換えます。取得中は、暗号化済みのメディアや初期化ファイルを復号しません。時刻、シーケンス番号、音声の関連付けなどを解析結果から作り直す方式ではありません。

配信元への接続が必要なのは最初の `analyze` だけです。その後は保存した原本で計画・変換・検証を進めます。後の解析や変換が失敗しても、取得が完了した原本は残ります。一回の操作で取得から変換まで進めたい場合は、従来どおり `convert URL --download-dir NEW_DIR` も使えます。

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

一度保存した原本をすべての出力に使います。URL 取得は他の対応 HLS 入力も扱えますが、出力ごとに DRM を明示する変換には、第3章の表に示した条件を満たす clear 入力が必要です。URL から取得できる形式すべてを、この DRM 変換の入力に使えるわけではありません。

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

既定では、HTTPS かつ入力 URL と同じ origin への接続だけを許可します。origin はスキーム、ホスト、ポートの組み合わせです。別 origin の参照や認証が必要な場合は、所有者だけがアクセスできる JSON ファイルに設定し、`--fetch-policy` で指定します。`analyze URL` と直接の `convert URL` の両方で使えます。

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

第5章と同様に `config` ディレクトリを保護し、JSON を保存した後にファイルの権限を設定します。macOS/Linux の指定例です。Windows では第14章の所有者専用 ACL を使います。

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

取得処理の実装上の上限は、一プレイリストあたり 4 MiB、プレイリストとメディアの合計リソース数が 100,000、取得計画に使うテキストと URL メタデータが 16 MiB です。リソース本体は所有者だけがアクセスできるファイルに逐次保存します。取得ポリシーを緩めても、メディアの対応条件や上限は変わりません。

成功 JSON の `download` には保存先の表示パス、エントリーポイントと相対パス、`retained: true` が入ります。入力 URL とリクエストヘッダーは含めません。一方、保存した原本には署名 URL などが残り得るため、保存先を保護してください。

# 9. 検証の意味と処理の単位

## 四つの照合

`verify` は結果と原本をそれぞれ独立に読み、次を確認します。DASH/HLS では、出力ディレクトリだけでなく実際の `manifest.mpd` または `master.m3u8` を指定してください。

| レポートの項目 | 確認する内容 |
| --- | --- |
| `stream_metadata` | コーデック設定、映像寸法・画素形式、音声レート・チャンネル、色、回転、言語など。成功値は `pass` |
| `packet_payloads_equal` | 順序と個数を含む符号化済みデータの一致。コンテナ形式が異なる場合は、対応を確認済みの符号化単位で比較する |
| `packet_timing_equal` | 全パケットの表示時刻・復号時刻・継続時間、対応する場合は正確な再生範囲 |
| `decoded_content_equal` | デコードした映像・音声全体の一致。対応する有限音声では、デコード内容全体と各ファイルの再生範囲の両方 |

PTS は presentation timestamp で、表示時刻を表します。DTS は decode timestamp で、復号時刻を表します。B ピクチャーなど、表示順と復号順が異なる映像では両者がずれます。音声と映像の同期関係も確認するため、絶対時刻で比較します。

`packet_timing_equal` には再生範囲の比較も含まれます。たとえば全映像パケットの時刻と復号画像が一致していても、原本の有限な編集範囲が結果に保持されていなければ不一致です。変換前の入力準備でも、元の開始位置・長さを正確に保持する必要があります。

全体の `passed` と `media_verified` に加え、個々の比較結果も確認してください。`license_playback_verified` は false です。ファイルの存在、暗号化方式の宣言、一度再生できたという結果だけでは、この四つの照合の代わりにはなりません。

## AAC と二秒セグメント

一般的な AAC-LC フレームは 1,024 サンプルです。48,000 サンプル/秒では約 21.333 ミリ秒になり、二秒は 93.75 フレームです。フレームの途中で符号化データを切れないため、音声の境界や個数は二秒の映像セグメントと一致しないことがあります。

整数の `EXT-X-TARGETDURATION` は、各区間の長さを最も近い整数に丸めた値以上である必要があります。drmr は生成した区間の長さを切り上げて余裕のある上限を求め、映像と音声のうち最大の値をすべてのメディアプレイリストに設定します。例えば映像が 2、音声が 3 なら、両方を 3 に揃えます。符号化したセグメントと `EXTINF` の長さは変えません。別途行う検証でも最終ファイルの宣言を読み、上限が揃わない出力を拒否します。RFC 8216 には VOD の字幕と I-frame プレイリストに関する例外がありますが、通常の映像・音声に別々の上限を設定できるという意味ではありません。

この差は、音声の欠落・重複・すき間・重なり・累積ずれを許す理由にはなりません。「一フレーム以内なら同じ」という許容差は使わず、すべてのパケット時刻と全サンプルを確認します。

十秒の試験 MP4 は 470 AAC フレームを含み、すべてをデコードすると一チャンネルあたり 481,280 サンプルになります。先頭の初期遅延 1,024 サンプルと末尾の 256 サンプルを除くため、実際の再生範囲は一チャンネルあたり 480,000 サンプルです。TS への変換でパディングも再生される構成になっている場合は、その TS 自体の再生範囲を基準にします。元の MP4 に合わせて短くすることはありません。

## 有限再生範囲を別に照合する理由

圧縮パケットが同じでも、MP4 の編集情報が消えると、再生すべきでない先頭や末尾が聞こえる場合があります。FFmpeg 9.0.2 の一部のフラグメント化 MP4 読み取りでは、有限の末尾切り詰めが通常デコードに反映されません。このため、単に同じツールで普通に復号して比較するだけでは足りません。

対応する有限 AAC の処理では、編集情報の自動適用やサンプルの破棄をせずに原本と結果をすべてデコードし、内容と個数を比較します。その後、各ファイルの情報から確認した開始位置と長さを一度だけ適用し、再生される内容と個数も比較します。さらに、プログレッシブ MP4 原本を通常どおりデコードした結果が、原本自身の再生範囲と一致することも確認します。変換結果に合わせて原本を切り詰めることはありません。

比較にはデコーダーの 32 ビット浮動小数点出力を使い、16 ビット整数への変換で生じる追加の丸めを避けます。ハッシュの計算と再生範囲の選択は固定サイズのバッファで逐次処理するため、非圧縮音声全体をメモリに保持する必要はありません。ファイル内の再生設定、音量、速度、有効フラグ、映像の表示寸法も確認します。ただし、すべてのプレーヤーで同じ編集情報が適用されることを保証するものではありません。

## マニフェストに記載する時刻の照合

TS の時刻はゼロ以外から始まることが多く、音声が最初の映像より先に始まる場合もあります。TS→DASH では、原本の全トラックの中で最も早い表示時刻を共通の始点にします。DASH の presentation-time offset を使って配信時刻に対応させ、トラックごとに個別にゼロへずらすことはしません。

パケットとセグメントの時刻は整数や有理数で正確に扱います。TS→DASH の最終 Period や、有限再生範囲を持つ DASH/HLS の再生時間を小数文字列で記載する際は、正確な終端を小数九桁で切り上げます。差は 1 ナノ秒未満です。これはマニフェストへの記載方法の規則であり、パケット時刻に誤差を認めるものではありません。記載された値がこの計算結果と異なれば拒否します。

有限編集を持たない通常の HLS では、EXTINF の小数表現に別の丸め規則を使います。その幅は、メディアクロックの一刻みと約一ミリ秒のうち大きい方です。この規則でも、全パケットの時刻は厳密に比較します。有限音声の一サンプルの違いを、この丸め幅を理由に受け入れることはありません。

## DRM 情報とマニフェスト検査

version 2 の `verify --plan` は現在の各出力を読み直し、一つの保存原本と独立に比較します。結果では次の確認事項を分けて示します。

| 結果項目 | 確認する内容 |
| --- | --- |
| `verification` | 符号化・復号した内容全体、メタデータ、時刻の一致 |
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

| 経路 | 主な処理単位 |
| --- | --- |
| 汎用 `convert` | 原本トラックの初期化情報と断片を順に専用ディスク領域へ組み立て、必要な復号・格納形式変更・再セグメント化・暗号化を行う |
| TS を読む汎用変換 | 原本 TS の構成・連続性・時刻を先に検査。SAMPLE-AES は Rust で符号化フレームを復号し、完全トラックとして新たにパッケージする |
| 準備済み DASH の `run` | 通常は全レンディションを復号して比較用のデータを作り、再セグメント化する。条件を満たす AAC では、元の境界を保持してセグメント別に処理する場合もある |
| HLS スナップショットの `package-hls` | 初期化情報とともに各セグメントを処理・暗号化する。再生内容全体を検証するために、セグメントを結合する処理もある |

汎用 `convert` は原本のセグメントを単に一つずつ再暗号化するのではなく、映像・音声の符号化データと正確な再生時刻を保持しながら、二秒を目標に再分割します。そのため、出力のセグメント境界が入力と同じになるとは限りません。HLS AES-128 を使う場合は、再分割後の出力リソースごとに暗号化します。

作業用ディスクには原本のほか、比較用の復号済みデータや変換結果の候補も保存するため、元データと同じ空き容量だけでは足りない場合があります。必要な容量は処理経路によって異なります。十分な空き容量と適切なアクセス権を確保し、原本と鍵は独立検証が終わるまで保持してください。

複数出力では、各出力のコピーを作成してバイト列を照合し、出力ルート全体を配置する直前まで保持します。独立した計画検証でも、各結果のコピーを最後の整合性確認まで保持します。各出力を別々に作成することもあり、必要な作業用ディスクの容量はコンテンツの長さと出力数に応じて増えます。コンテンツ全体をメモリに保持する方式ではなく、ディスクへ逐次保存します。空き容量を確保し、一時領域の削除が終わるまで待ってください。

参照（英語）: [処理方法とコンテンツ全体の検証](https://github.com/kazuhide/drmr/blob/main/docs/media-processing.md)。

# 10. HLS スナップショット経路

## 汎用 convert との違い

複数の DRM 出力を作る場合は、共通の `analyze`、`plan --outputs`、`convert --plan`、`verify --plan` を使います。`analyze-hls` と `package-hls` は、保存した HLS の解析結果を使う従来の操作との互換性のために残しています。`package-hls` という名前も以前からのもので、HLS に加えて DASH も出力できます。新しく複数出力の変換を始める場合は共通 CLI を推奨します。

`analyze-hls` は clear HLS を取得またはコピーし、メディアを調べ、ファイルと `analysis.json` を保存します。以下はこの経路固有の設定と使い方です。共通変換の別名ではありません。

この経路では、対応条件を満たす WebVTT や MP4 内 TTML 字幕など、汎用 `convert` が扱わない入力も処理できます。ただし、解析元は配信内容が確定した clear HLS に限ります。暗号化入力、ライブ、不連続、未対応のトラックや字幕は拒否します。汎用変換とは対応範囲が異なる点に注意してください。

## ローカル HLS の保存

所有者専用の `source.json` を用意します。次のパスは実際の絶対パスへ置き換えてください。

```json
{
  "url": "/absolute/path/media/master.m3u8",
  "policy": {
    "allowed_local_roots": ["/absolute/path/media"]
  }
}
```

```sh
chmod 600 source.json
drmr analyze-hls --source-config source.json \
  --asset-id sample --output output
drmr verify-snapshot --files output/sample/clear/source
```

出力は `output/sample/clear/source/` に保存されます。既存スナップショットは上書きしません。別の asset-id または出力場所を選んでください。HTTPS の場合は `url` と `policy.allowed_origins` を指定し、プレイリスト・リソース・リダイレクトの全 origin を明示します。

## package-hls によるシグナリング付き出力の生成

`DRM_KID`、`DRM_KEY`、`DRM_IV` は、秘密情報の管理手順に従って環境変数に設定します。どれも 16 バイトを 32 桁の十六進数で表します。鍵の値を含むシェルコマンドを履歴に残さないでください。この環境変数による指定方法は、汎用 `convert` の鍵設定とは異なります。

`drm.json` は暗号鍵ではなくシグナリングの設定です。

```json
{
  "widevine_pssh": "<hex-or-base64-widevine-initialization-data>",
  "playready_la_url": "https://license.example.invalid/acquire",
  "fairplay_uri": "skd://content-identifier"
}
```

```sh
chmod 600 drm.json
drmr package-hls --analysis output/sample/clear/source/analysis.json \
  --drm-config drm.json
```

Widevine PSSH は DRM 初期化情報で、完全な box または対応するペイロードを十六進か Base64 で渡します。含まれる識別子は `DRM_KID` と一致する必要があります。独自のダミー値で置き換えても有効なライセンスは作れません。

音声選択が必要な場合は、解析されたトラック ID を `audio_source_id` として所有者専用 JSON に保存し、`--selection` に渡します。`analysis_id`、`snapshot_digest` も指定して古い選択を検出できます。

`verify-snapshot` は、既定では記録の内容から計算したダイジェストを照合します。`--files` を付けると、保存したプレイリストとセグメントのハッシュも再計算します。原本と変換結果の映像・音声・時刻をすべて比較する `verify OUTPUT --source INPUT` の代わりにはなりません。終了コードは 0 が一致、1 が変更あり、2 が不正なスナップショットを示します。

参照（英語）: [保存 HLS の使い方](https://github.com/kazuhide/drmr/blob/main/docs/usage.md)。

# 11. 準備済み DASH とバッチ処理

## run 用の入力レイアウト

`run` には、元の URL の構造に対応したディレクトリ配置でダウンロード済みのファイルが必要です。任意の MPD を単独で置くだけでは使えません。

```text
sample/
  download/
    manifest.example.test/sample/dash/manifest.mpd
    manifest.example.test/sample/hls/manifest.m3u8
    cdn.example.test/sample/video/v0/init.mp4
    cdn.example.test/sample/video/v0/seg_1.m4s
    cdn.example.test/sample/video/v0/seg_2.m4s
```

MPD には BaseURL、MP4 の adaptation set、representation ごとの SegmentTemplate が必要です。テンプレートは `initialization`、`media`、`timescale`、`duration` を持ち、メディア名は `$Number$` と数値セグメント名を使います。上の例では、BaseURL が `https://cdn.example.test/sample/` なら、`video/v0/init.mp4` は示した位置へ対応します。

`DRM_KID`、`DRM_KEY`、`DRM_IV` を環境へ準備したうえで実行します。

```sh
drmr plan --mpd sample/download/manifest.example.test/sample/dash/manifest.mpd
drmr run \
  --mpd sample/download/manifest.example.test/sample/dash/manifest.mpd \
  --hls-master \
    sample/download/manifest.example.test/sample/hls/manifest.m3u8 \
  --output output --asset-id sample --log-format jsonl
```

結果は `output/sample/` の下に保存します。`--packager-bin` には実行ファイルか packager のあるディレクトリを指定します。`--no-round-trip`、`--no-manifests`、`--limit-renditions` はデバッグ用であり、検査や出力の一部を省略します。すべての処理と検証を終えた結果として扱わないでください。

## batch の作業リスト

`batch` は再開ログを使いながら、複数のアセットを一つずつ処理します。対象は準備済み DASH と clear-HLS のワークフローです。汎用 `convert` の任意のコマンド列を実行する機能ではありません。

作業リストは JSON Lines、つまり一行に一つの JSON オブジェクトです。最初の行は文字列の schema_version を持つ `{"schema_version":"1"}`、以後は次の四フィールドだけを持ちます。

| フィールド | 内容 |
| --- | --- |
| `asset_id` | アセット識別子 |
| `kind` | `dash-cenc` または `hls-clear` |
| `dash_manifest` | ストレージ基点からの相対オブジェクトキー。使わない場合は null |
| `hls_master` | ストレージ基点からの相対オブジェクトキー。使わない場合は null |

例えば、次の内容を `asset.json` に保存します。これは一件のレコードを読みやすく書いたもので、作業リストへ入れる際は一行にします。

```json
{
  "asset_id": "sample",
  "kind": "dash-cenc",
  "dash_manifest":
    "sample/download/manifest.example.test/sample/dash/manifest.mpd",
  "hls_master":
    "sample/download/manifest.example.test/sample/hls/manifest.m3u8"
}
```

`canonical` は JSON を安定したキー順・書式の一行にできるため、このレコードの整形にも使えます。次は新しい作業リストを作る例です。

```sh
printf '%s\n' '{"schema_version":"1"}' > work-list.jsonl
{
  drmr canonical asset.json && printf '\n'
} >> work-list.jsonl
drmr batch --work-list work-list.jsonl --output output \
  --resume-log resume.jsonl --plan
```

`canonical` の出力末尾には改行がないため、レコードを追加するたびに上記の例のように改行も出力します。このコマンドは入力した JSON の値をそのまま出力し、秘密情報を除去しません。鍵ファイルや認証情報をログに表示しないよう注意してください。

## 鍵と再開

既定の鍵提供元は環境変数です。アセットごとの保護された JSONL を使う場合は次の形式を指定します。鍵ファイルにはスキーマヘッダーはなく、各行が `asset_id`、`kid`、`key`、`iv` だけを持ちます。汎用 `--keys-file` の形式とは違います。

```sh
drmr batch --work-list work-list.jsonl --output output \
  --resume-log resume.jsonl --storage local --storage-root ./catalog \
  --key-provider file --key-file ./private/keys.jsonl
```

鍵ファイルを所有者専用にし、出力ツリーの外に置きます。HLS 向けの設定群には `--hls-config-dir` を使います。アセットごとの設定と入力は、対応するワークフローの条件を満たす必要があります。

失敗した項目を再試行するには `--retry-failed` を指定します。アセット1件あたりの試行回数の上限は `--max-attempts` で設定し、既定値は 3 です。`--per-asset-timeout-s` の既定値は 3600 秒、`--heartbeat-s` の既定値は 60 秒です。再開しても、それまでに行った処理を元に戻すわけではありません。ログと既存出力を確認してから再処理してください。

`--storage s3` にも対応していますが、このバージョンで使える region は `ap-northeast-1` に限ります。実行時には bucket の場所を確認します。認証には AWS の環境変数と標準プロファイルを使います。`--bucket`、`--region`、`--output-prefix` はストレージの設定です。このガイドのメディア検証結果には、実際の S3 アカウントでの動作確認は含みません。

参照（英語）: [準備済み入力と batch](https://github.com/kazuhide/drmr/blob/main/docs/usage.md)、[batch の CLI 定義](https://github.com/kazuhide/drmr/blob/main/crates/drmr-core/src/batch_cli.rs)。

# 12. 並列処理・メモリ・停止

## 並列数の自動調整と --max-workers

`convert` と `verify` は、利用可能な CPU とメモリ、映像の寸法、リソースのサイズ、トラック数から並列数を自動で調整します。`--max-workers` には 1 から 64 の範囲で上限を指定できますが、実際にはそれより少ないワーカーを使う場合があります。省略時に選ぶ推奨数は最大 32 です。

```sh
drmr convert source.mpd --to cbcs --format dash \
  --output result --keys-file config/keys.json --max-workers 2
drmr verify result/manifest.mpd --source source.mpd \
  --keys-file config/keys.json --max-workers 1 --json --measure
```

ワーカー数を変えても、内容と時刻の検証は省略しません。トラック処理、セグメント処理、原本解析では、同じプロセス内で外部処理に使う CPU・メモリの予約枠を共有します。`batch` のアセット自体は一つずつ処理します。別々の CLI プロセス間では予約枠を共有しません。

内蔵のマニフェスト検査も同じ予約枠を使います。小さい入力は呼び出し元で処理します。大きな HLS は文書ごとに、大きな DASH は一度解析したツリー内の独立した検査ごとに並列処理できます。処理用のプールを入れ子にせず、子プレイリストの解析結果や継承したタイムラインを再利用します。マニフェストの保持と検査に必要なメモリを予約し、ファイルの識別情報と内容も照合します。バイト数、ノード数、参照数、診断数などの上限に達した場合は不完全な検査として報告し、合格にはしません。

## 資源見積もりの範囲

Linux では、読み取り可能な CPU quota、メモリの cgroup 上限、親 cgroup の上限も見積もりに使います。cgroup は、コンテナやサービスが使う OS の資源を制御する仕組みです。資源情報を取得できない場合は、512 MiB の枠で子処理を一つ実行する前提で見積もります。一つの処理もこの枠に収まらないと判断した場合は、`resource-budget` で失敗します。

利用可能なメモリがわかる場合は、その四分の一を基準に、最低 256 MiB の予約枠を確保します。トラックごとの保持データも見積もりに含めます。子処理については基本の使用量に加え、画素数、入力の処理量、取得する出力の上限を考慮します。これは処理を開始できるかを判断するための見積もりであり、OS によるメモリ制限や最大使用量の保証ではありません。

FFmpeg のデコーダー・エンコーダー・フィルターと FFprobe のデコードには、それぞれスレッド数 1 を設定します。補助スレッドも含めて一つになるわけではありません。音声ハッシュの計算とパケット時刻の照合は逐次処理します。ただし、トラックやセグメントのメタデータは保持するため、メモリ使用量全体が常に一定になるわけではありません。

外部処理から取得する標準出力の上限は既定で 16 MiB、標準エラーは 1 MiB です。上限を超えた場合は `output-limit` などで失敗し、途中で切れたデータを使って検証を成功とすることはありません。待機中も制限時間と停止要求を確認します。

## Ctrl+C と終了確認

macOS/Linux の汎用 `convert`・`verify` は SIGINT、SIGTERM、Windows は Ctrl+C と Ctrl+Break で協調停止します。予約待ちと新規処理を止め、所有する子プロセスを終了し、後片付けを確認します。公開前に停止要求を観測した場合は結果を配置しません。

終了コード 130 は、処理の停止と子プロセスの終了確認が完了したことを示します。`cleanup-unconfirmed` や `media-cleanup-unconfirmed` は、停止要求による終了であっても失敗として扱います。強制 kill や OS による強制終了では協調停止の処理を行えず、一時領域が残ることがあります。

Unix では drmr が作成したプロセスグループで、Windows では job object で子孫プロセスを管理します。終了要求を送っただけでは、子孫プロセスがファイルを閉じたと判断しません。Unix で直接の子プロセスが終了した後のグループ確認と、Windows で job の終了通知を待つ処理には、それぞれ別の 30 秒の制限時間があります。ただし、ファイル I/O、名前解決、OS のスケジューリングも含めた全体の終了時刻を厳密に保証するものではありません。

## 計測値の読み方

`--measure` は `--json` と併用します。メディア検証の条件は変えず、数値を追加します。フェーズは重なったり入れ子になったりするため、フェーズ時間を単純合計して全体時間とみなさないでください。

短い十秒アセットの時間は、起動処理やキャッシュの影響を受けます。メモリのサンプリング値は真の最大値ではありません。比較する場合は、原本・ツール版・ビルド設定・ワーカー上限・ホスト負荷を揃え、まず同じ完全検証が通ることを確認してください。

内蔵ライブラリの検査速度を測るため、ソース [`c1458e7d`](https://github.com/kazuhide/drmr/commit/c1458e7deea5b31118073c682d799f60c8f4e8b5) を Rust 1.99 の release ビルドで実行しました。環境は論理 CPU 10 個、メモリ 32 GiB の macOS ARM64 です。2026年10月21日の測定では、マスタープレイリストと、各1000セグメントのメディアプレイリスト64本を検査する時間が、逐次処理で 14.50 ms、自動選択された10ワーカーで 3.37 ms でした。どちらの検査レポートも一致しました。小さい入力は1ワーカーで処理します。

30000項目の DASH タイムラインでは逐次処理が 5.22 ms、自動選択が 5.27 ms となり、並列化で速くならない入力もありました。これらは実行順を変えて三回測った中央値で、変換時間ではなくライブラリによる検査全体の時間です。測定には作成した検査用データを使いました。同時に実行していた本プロジェクトのメディア試験は停止しましたが、ホスト上の他の処理までは分離していません。追加のメモリ確保量とプロセスのメモリ使用量は別々に測定しており、詳細はリンク先に記載しています。

参照（英語）: [資源制限と測定方法](https://github.com/kazuhide/drmr/blob/main/docs/resources.md)。

# 13. Docker による実行

## イメージのビルド

Docker イメージは CLI、FFmpeg/ffprobe、Bento4 の mp4dump、Shaka Packager を含みます。ビルドには Git と BuildKit が使える Docker が必要です。ネイティブ Linux amd64 または arm64 が対象で、Docker Desktop は Linux コンテナを選びます。

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
  --mount "type=bind,src=$PWD/catalog,dst=/catalog,readonly" \
  --mount "type=bind,src=$PWD/output,dst=/output" \
  --env DRMR_CATALOG_ROOT=/catalog --env DRMR_OUTPUT_ROOT=/output \
  drmr:local serve
```

Docker Desktop では追加の host networking 機能が必要です。コンテナ内の loopback だけで待ち受けるサーバーには、通常のポート公開だけでは接続できません。接続のために loopback・Host・Origin の保護を解除しないでください。ネイティブの Desktop GUI は、このイメージには含まれません。

Windows では Docker Desktop の WSL 2 連携を使い、Linux シェルで実行します。秘密情報を含む設定は Linux 側のファイルシステムに置いてください。Windows ドライブをマウントした場合は権限の扱いが異なるため、ネイティブ Windows 向けの ACL の設定手順は適用できません。

イメージ内の `/usr/share/drmr/build-manifest.json` は元コミット、ツリー、実際の依存版を記録します。ライセンスは `/usr/share/drmr/licenses`、対応するメディアツール等のソースは `/usr/share/drmr/sources` にあります。Debian パッケージ全体のソースミラーではありません。

参照（英語）: [Docker の使い方と確認結果](https://github.com/kazuhide/drmr/blob/main/docs/docker.md)。

# 14. Windows のネイティブ CLI

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

Windows では chmod の代わりに access control list（アクセス制御リスト、ACL）を使います。所有者を現在のユーザーに設定し、権限の継承を止めて、そのユーザーだけに完全アクセスを許可する ACL が必要です。次の例では所有者専用の新しいディレクトリと空ファイルを作成し、その後でエディターから設定を入力します。

```powershell
$sid = [System.Security.Principal.WindowsIdentity]::GetCurrent().User
$private = Join-Path $env:LOCALAPPDATA 'drmr-keys'
if (Test-Path -LiteralPath $private) { throw 'Choose a new directory' }
New-Item -ItemType Directory -Path $private | Out-Null

function Set-OwnerOnlyAcl($path, $security) {
    $security.SetOwner($sid)
    $security.SetAccessRuleProtection($true, $false)
    $rule = [System.Security.AccessControl.FileSystemAccessRule]::new(
        $sid, 'FullControl', 'Allow')
    $security.AddAccessRule($rule)
    Set-Acl -LiteralPath $path -AclObject $security
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

エディターによるファイルの置き換えで ACL が変わった場合は、ファイルを所有者専用ディレクトリ内に置いたまま ACL を再適用してください。既存ファイルの不適切な権限を drmr が自動で修正することはありません。

## PowerShell からの変換

```powershell
.\drmr.exe inspect 'C:\media\source.mp4' --json
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

# 15. ローカル Web コンソール

## 起動と対象

Web コンソールは認証なしのローカル操作画面です。信頼できるマシン上の loopback で使い、リバースプロキシやポート転送で公開しないでください。Host と Origin の確認は、他のローカルプロセスを認証する機能ではありません。

```sh
DRMR_CATALOG_ROOT=/absolute/path/catalog \
DRMR_OUTPUT_ROOT=/absolute/path/output \
drmr serve
```

`http://127.0.0.1:8080` を開きます。二つのルートディレクトリが必要です。`DRMR_BIND_PORT` でポート、`DRMR_MAX_WORKERS` で HLS パッケージのワーカー上限を設定します。カタログには対応レイアウトで準備したエントリーを置きます。一般のファイル選択画面や、汎用 CLI の全機能を備えた画面ではありません。

サーバーとジョブ用の CLI は同じバージョンを使います。`--cli` で差し替える場合は実行ファイルを直接指定してください。別の CLI を起動するラッパーを指定すると、子プロセスを管理するための起動条件を満たさず、拒否される場合があります。

## HLS 入力の取得ポリシー

追加設定がなければ、default プロファイルは入力 URL 自身の HTTPS origin だけを許可します。別 origin の参照や認証が必要なら、所有者専用の profiles.json に名前付きポリシーを保存し、`DRMR_HLS_PROFILES` にそのファイルのパスを設定します。環境変数へ JSON 文字列自体を渡す指定ではありません。

```json
{
  "default": {
    "policy": {
      "allowed_origins": ["https://media.example.com"],
      "headers_by_origin": {
        "https://media.example.com": {"Authorization": "Bearer <token>"}
      }
    }
  }
}
```

```sh
chmod 600 profiles.json
DRMR_CATALOG_ROOT=/absolute/path/catalog \
DRMR_OUTPUT_ROOT=/absolute/path/output \
DRMR_HLS_PROFILES=/absolute/path/profiles.json \
drmr serve
```

この例は、画面の default プロファイルに対応します。入力 URL の origin も許可リストに含める必要があります。Web 画面からの取得ではローカルファイルを許可せず、ポリシー内のローカルルートも除外します。カタログ内のローカル DASH の処理や、CLI analyze-hls で明示的に許可したローカル入力とは扱いが異なります。

## ジョブ状態と Stop

画面からの JSON リクエストは、ヘッダーと本文の受信を含めて 15 秒でタイムアウトします。応答がない場合に画面を再操作できるようにするための制限であり、メディア処理の制限時間ではありません。開始要求への応答が届かなくても、同じ要求を自動で再送しません。記録済みのジョブと照合し、開始したか判断できない場合は別のジョブの開始も止めます。「状態を確認」（英語表示では Check job status）で状態だけを再照会してください。

実行中の Stop は、そのジョブと子プロセスの終了を要求します。応答だけで後片付け完了と判断せず、最終ジョブ状態とログを確認してください。通常の停止後は次の待機ジョブへ進みます。サーバー終了中は新しい待機ジョブを開始しません。

Web の準備済みワークフローでは、汎用 `convert` のように新規出力を完成後に配置する方式とは異なり、途中出力や以前の結果を変更する場合があります。「停止」（英語表示では Stop）を押しても、それまでの処理を元に戻すことはありません。終了コード 0、出力ファイルの存在、カタログの古い `converted` フラグだけで再生準備が完了したと判断せず、現在保存されている出力に対応する `output_readiness` を確認してください。

子プロセスの終了を確認できない場合は、ジョブの実行枠と一時設定を保持します。状態の確認、ログの参照、停止に関する診断機能は利用できますが、新規処理は拒否します。原因を確認し、サーバーの正常終了を再試行してください。強制終了すると、この確認を行えません。

## 保存物と秘密情報

clear HLS を追加すると、URL、原本プレイリスト、取得メディアは `OUTPUT_ROOT/<asset-id>/clear/source/` に残ります。履歴から削除しても、保存原本、結果、ログをディスクから消すわけではありません。

鍵入力欄は既定で視覚的にマスクし、明示的に表示できます。送信後は入力値を消し、アプリ自身の localStorage/sessionStorage へ鍵を保存しません。ただし、ブラウザーのメモリ、開発者ツール、ページや通信の観察から鍵を隔離する機能ではありません。

ローカル ClearKey 再生では、JWK と呼ぶ JSON 形式の鍵応答かプレーヤー設定を通じて、コンテンツキーをブラウザーに渡します。no-store を指定していても、開発者ツール、HAR、拡張機能などから読み取れます。運用 DRM のライセンスサービスや、ハードウェアで保護した再生を提供するものではありません。

HLS パッケージ後は、プレビュー再生に必要な鍵などの情報が、操作がなくなってから最大 30 分間メモリに残る場合があります。30 秒ごとの定期処理と関連アクセスの際に期限を確認します。フォームの入力を消しても、再生用に保持した情報が直ちに失効するわけではありません。その結果に対する最後の再生セッションが失効するか、サーバーが終了した時点で関連情報を破棄します。

再生開始処理の制限時間は 20 秒で、前のプレーヤーの破棄、鍵応答の取得、メディアの読み込み、実際の playing イベントまでを含みます。タイムアウトしても画面を再操作できますが、変換結果の検証状態は変えません。前のプレーヤーを破棄できたか確認できない場合は、別のプレーヤーを重ねて作らず、ページを再読み込みしてください。

参照（英語）: [Web の操作とプロセス管理](https://github.com/kazuhide/drmr/blob/main/docs/usage.md)、[ローカル再生での鍵の扱い](https://github.com/kazuhide/drmr/blob/main/docs/security.md)。

# 16. Desktop の使い方と検証範囲

## フォルダーの選択とコンソールの起動

実験的な Desktop アプリは、ローカル Web コンソールをネイティブウィンドウで開きます。カタログ、ジョブ、外部メディアツールは Web 版と共通です。

1. File → Choose catalog folder でカタログを選ぶ。
2. File → Choose output location で出力先を選ぶ。その下に所有者専用の drmr-output を作成するか、既存のものを開く。
3. File → Check external tools… で、実際の起動環境の外部ツールを確認する。
4. File → Open console で操作画面を開く。

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

macOS では、ネイティブのフォルダーメニュー、キーボード操作、レポート画面への移動、カタログ変換、処理中の Stop、正常終了と待ち受けの解除を動作確認しています。Chrome のローカル ClearKey 試験では、原本と変換結果の両方を最後まで再生できました。

WKWebView では、英語・日本語の両画面で再生失敗後の表示と再試行ができることを確認しています。ただし、最後まで再生できたという確認はありません。Windows/Linux のネイティブ GUI の動作も確認できていません。各 OS でバンドルのビルドと検査に成功しても、ダイアログ、WebView、DRM 再生まで動作確認したことにはなりません。Linux GUI の依存関係には、公開ドキュメントに記載した保守上の制限もあります。

参照（英語）: [Desktop の詳細と確認範囲](https://github.com/kazuhide/drmr/blob/main/docs/desktop.md)、[依存関係のセキュリティ方針](https://github.com/kazuhide/drmr/blob/main/docs/dependency-security.md)。

# 17. 結果・終了コード・問題の切り分け

## JSON と進捗

スクリプトなどから結果を読み取る場合は `convert --json`、`verify --json` を使います。正常な結果は、スキーマのバージョンを含む一つの JSON オブジェクトとして返します。`--measure` を付ける場合も `--json` が必要です。

例えば、計測なしの `verify --json` による成功結果は次の形式です。

```json
{
  "schema_version": 1,
  "passed": true,
  "media_verified": true,
  "checks": {
    "stream_metadata": "pass",
    "packet_payloads_equal": true,
    "packet_timing_equal": true,
    "decoded_content_equal": true
  },
  "license_playback_verified": false
}
```

有効な引数で処理が始まった後にエラーが起きると、`schema_version: 1`、`command`、`status`、`error` を返します。status は failed または cancelled です。error には固定の code、秘密情報を含めない hint、最後に確認した処理名 last_activity が入ります。比較を終えた結果が不一致だった場合は、checks と比較情報も残します。

映像のみの変換結果から有限再生範囲の情報を削除すると、符号化データと復号画像が一致していても、比較は次のように失敗します。この例の `packet_timing_equal: false` は再生範囲の不一致を示します。hint と last_activity は実際の英語出力です。

```json
{
  "schema_version": 1,
  "passed": false,
  "media_verified": false,
  "checks": {
    "stream_metadata": "pass",
    "packet_payloads_equal": true,
    "packet_timing_equal": false,
    "decoded_content_equal": true
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

JSON を使わない場合は、処理名、件数、経過時間を標準エラーに一行ずつ表示します。一つのトラックの処理が終わっても、変換全体の完了を意味するわけではありません。複数の処理段階が重なったり、終了した段階が失敗していたりする場合もあります。最後の処理名だけで原因を断定しないでください。

## 終了コード

| 値 | 意味 |
| --- | --- |
| 0 | 選択したコマンドの確認・処理に成功。何を検証したかはコマンドによる |
| 1 | 処理、比較、必要な診断、終了確認などの失敗 |
| 2 | 引数・設定の不正。verify-snapshot では不正なスナップショット |
| 130 | 協調停止に対応する CLI で、管理中の子プロセスの終了確認まで完了 |

`doctor` の 0 は選択した診断の成功、`inspect` の 0 はメタデータの読み取り成功を示します。コンテンツ全体の検証に成功したという意味ではありません。`verify-snapshot` の 1 は、記録や保存内容に変更があったことを示します。

## よくある失敗

| 症状・コード | 確認すること |
| --- | --- |
| 必須ツールが見つからない | 実際の起動環境の PATH、実行権限、Desktop の Check external tools を確認する |
| バージョン・機能検査が失敗 | 版だけでなくビルドのオプションを確認する。Bento4 の build は配布元でも確認する |
| 鍵設定が拒否される | 所有者、600 または ACL の権限、リンク数、JSON の重複キー、不明フィールド、スキーマを確認する |
| `output-spec-invalid` | 出力一覧の schema_version が 1 か、ID が重複せず安全な名前か、形式・暗号・DRM の組み合わせが対応範囲内か確認する |
| `execution-plan-output-keys-mismatch` / `execution-plan-drm-mismatch` | version 2 の鍵ファイルの暗号化出力 ID と DRM 選択を計画に合わせる |
| `manifest-validator-unavailable` / `manifest-validator-not-configured` | Apple/DASH-IF の依存ツールと実行ファイルの場所を確認する。必須検査の省略では成功にできない |
| `input-key-required` | 原本の各 KID または正確な HLS 鍵 URI に対応した入力鍵を設定する |
| `output-key-required` / `output-key-not-used` | 暗号化出力に必要な鍵があるか、clear 出力に不要な output が残っていないか確認する |
| `presentation-window-not-supported` | 編集情報、開始位置・終端、通常速度、正確なクロック換算などが対応条件を満たすか確認する。処理を通すために原本を切り詰めない |
| `progressive-cenc-profile-not-supported` / `progressive-cbcs-profile-not-supported` | 暗号配置、IV、補助情報、入力 PSSH などが限定プロファイル内か確認する |
| `unsupported-packet-timing` | FFprobe が全 PTS・DTS・duration を返すか、必要なツール版と実データを確認する。欠落値を推測して埋めない |
| `round-trip-verification-failed` | 原本と結果のメタデータ、符号化内容、全デコード、時刻のどこが一致しないか確認する |
| `resource-budget` | 取得できた CPU・メモリの情報、入力のサイズと寸法を確認する。ワーカー上限を増やしても解決するとは限らない |
| `media-output-limit` / `media-probe-output-limit` | 捕捉上限やレコード上限を超えていないか確認する。部分結果を成功扱いしない |
| `cleanup-unconfirmed` / `media-cleanup-unconfirmed` | 子プロセスやロックの状態を確認する。停止や処理が成功したとは扱わず、子プロセスの管理を続けて原因を解消する |

変換を繰り返す場合は、新しい出力名を選びます。URL からの取得が完了していれば保存した input.m3u8 を再利用できますが、既存の `--download-dir` を再取得先には指定しません。作業領域を手動で削除する前に、その領域を使う子プロセスが終了したことを確認してください。

問題を報告する際は、コマンド名、固定のエラーコード、バージョン、入力と出力の対応条件、秘密情報を除いた最小限の再現手順を中心に共有してください。レポート内の鍵などがマスクされていても、原本 URL、パス、メディア名、出力シグナリングに秘密情報が残っている場合があります。

# 18. 試験結果の読み方と再現方法

## 短い公開アセットによる回帰試験

主なメディア試験には、出典と権利を明記した Big Buck Bunny の十秒アセットを使います。H.264 の映像パケット 240 個と AAC 音声を含み、映像は二秒を目標に五つのセグメントへ分割します。音声はフレーム境界で分割するため、すべてのセグメントが厳密に二秒になるわけではありません。

| 試験 | 対象と確認内容 |
| --- | --- |
| 従来の暗号方式の 16 経路 | clear/cenc/cbcs DASH と AES-128 fMP4 HLS の原本から四つの出力を作る。原本と結果に別々の鍵を使い、変換後に改めて verify を行う |
| TS→DASH の 27 経路 | clear/AES-128/SAMPLE-AES 入力 × 映像のみ・映像音声・音声のみ × clear/cenc/cbcs DASH。原本の全データ・時刻と最終宣言を確認 |
| 音声のみ TS→fMP4 HLS の 9 経路 | clear/AES-128/SAMPLE-AES 入力から clear/CBCS/AES-128 の fMP4 HLS へ変換。原本の全パケット・時刻・デコード音声を確認 |
| TS 出力の 27 経路 | 同じ三つの入力暗号方式と三つのトラック構成から clear/AES-128/SAMPLE-AES TS を作り、内容全体を比較する |
| 有限音声範囲の 54 経路 | clear/cenc/cbcs MP4 × 音声のみ・映像音声 × MP4/DASH/fMP4 HLS の九出力。音声全体と各ファイルの正確な再生範囲を確認 |
| 有限 MP4 の CLI 27 経路 | 三つの入力暗号方式の映像音声 MP4 から九出力を作り、実際の CLI で独立検証する |
| URL 入力 | 対応する clear／暗号化 TS・fMP4 を保持し、入力鍵の URI 対応と CBCS 変換を確認。配信元停止後も保存原本で再実行する |
| DRM を明示した五つの出力 | CBCS DASH の Widevine/PlayReady 併用・各単独と、FairPlay の fMP4/TS HLS。コンテンツ全体を比較し、実際のマニフェストと初期化ファイルの宣言も再検証する |
| 名前を付けた複数の暗号化出力 | 一つの clear TS から DASH、fMP4 HLS、SAMPLE-AES TS、AES-128 TS を別の出力鍵で作る。単一 TS 出力の保存計画も別途実行・検証する |

CLI の 27 経路は、54 種類のうち映像と音声を持つ組み合わせを、実際のコマンドでもう一度確認する試験です。

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

有限音声の 54 種類は、音声のみ 27 種類と映像音声 27 種類です。実 CLI の 27 経路は後者の再確認で、追加の独立したプロファイルではありません。従来の 16 経路は事前にフラグメント化した原本を基準にし、有限音声の試験は元のプログレッシブ MP4 自身の再生範囲を基準にします。原本やデコード方法が異なる試験のサンプル数を混同しません。

Bento4 の build 番号など、配布物に指定されたバージョンと起動時の表示で確認できる情報は区別してください。Desktop のビルド成功は GUI や再生の動作確認にはなりません。Docker の HTTP 試験も、ブラウザー画面を確認した結果ではありません。既知の鍵でコンテンツ全体が一致していても、DRM ライセンス取得や実機再生を確認したことにはなりません。

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

# 19. CLI オプション一覧

この章に公開 CLI の引数とオプションをまとめます。すべてのコマンドで `-h` / `--help` を使えるほか、`drmr help COMMAND` でもヘルプを確認できます。CLI のバージョンは `-V` / `--version` または `drmr version` で表示します。子プロセスの管理に使う内部用の起動引数は、利用者向けのインターフェースに含めません。

## FILE・PATH に指定するもの

`FILE` や `PATH` は、その文字列をそのまま入力する指定ではありません。使うファイルのパスに置き換えます。相対パスはコマンドを実行するディレクトリを基準にします。パスに空白がある場合は引用符で囲んでください。JSON の中身をオプションの直後に直接書く方式ではありません。

同じ `FILE` 表記でも、設定 JSON、生成済みの計画、実行プログラムでは渡すものが異なります。代表的な指定例と、内容の説明は次のとおりです。

| 指定例 | 用意するもの・詳しい説明 |
| --- | --- |
| `--fetch-policy config/fetch.json` | URL の取得条件を書く JSON。第8章の[取得ポリシーの書式と使用例](https://kazuhide.github.io/drmr/ja/guide/#url-input)を参照。追加 origin・認証ヘッダー・取得量などを設定する |
| `--keys-file config/keys.json` | 鍵を読み込む JSON。第5章の[入力鍵・出力鍵の構造例と保護方法](https://kazuhide.github.io/drmr/ja/guide/#key-configuration)を参照。通常の単一出力は version 1。--outputs または version 2 の計画を使う場合は、出力が一つでも version 2。出力一覧や取得ポリシーとは別ファイル |
| `--outputs config/outputs.json` | 利用者が作る version 1 の出力一覧 JSON。第6章の[複数出力の設定例](https://kazuhide.github.io/drmr/ja/guide/#inspect-plan)を参照。出力 ID・形式・暗号方式などを指定し、鍵は含めない |
| `--plan conversion.plan.json` | `plan --save conversion.plan.json` で生成した実行計画 JSON。第6章の[計画の保存と実行](https://kazuhide.github.io/drmr/ja/guide/#inspect-plan)を参照。鍵 JSON や出力一覧を代わりに指定することはできない |
| `--packager-bin /opt/media-tools/packager` | インストール済みの Shaka Packager 実行ファイル。設定 JSON やインストーラーではない。第4章の[依存ツールの準備](https://kazuhide.github.io/drmr/ja/guide/#installation)を参照 |
| `--hls-validator-bin /opt/media-tools/mediastreamvalidator` | macOS に用意した Apple の検証プログラム。第9章の[外部マニフェスト検査の準備](https://kazuhide.github.io/drmr/ja/guide/#verification)を参照 |
| `--dash-validator-bin /opt/media-tools/php` | ローカルの DASH-IF アダプターを動かす PHP 実行ファイル。MPD や DASH-IF のディレクトリではない。第9章の[検査ツールと DRMR_DASH_IF_ROOT](https://kazuhide.github.io/drmr/ja/guide/#verification)を参照 |

`analyze` は、どちらの版の鍵 JSON からも入力鍵を読み込めます。鍵ファイルに出力設定がある場合も、解析には入力鍵だけを使います。

`--packager-bin` の既定値 `packager` は、起動環境の PATH から実行ファイルを探す指定です。別の場所に配置した場合は、その実行ファイルのパスを渡します。例えば macOS/Linux では次のように指定します。入力ファイル、出力先、鍵設定、実行ファイルの場所は自身の環境に合わせて置き換えてください。

```sh
drmr convert original.mp4 --to cbcs --format mp4 \
  --output protected.mp4 --keys-file config/keys.json \
  --packager-bin "/opt/media-tools/packager"
```

Windows では、例えば `--packager-bin 'C:\media-tools\packager.exe'` と指定します。汎用 `analyze`・`plan`・`convert`・`verify` と `package-hls` は実行ファイルを受け取ります。`run` と準備済み DASH の `batch` は、`packager`（Windows は `packager.exe`）という名前の実行ファイルを含むディレクトリも指定できます。HLS を含む `batch` では実行ファイルのパスを指定してください。オプションの値にシェルコマンド全体や追加引数は含めません。CLI アーカイブには Shaka Packager は同梱されていません。

`NEW_FILE`・`NEW_PATH`・`NEW_DIR` は、これから作る保存先です。読み込む `FILE` とは異なり、既存の出力先を上書きするための指定ではありません。秘密情報を含むファイルは、鍵設定の保護手順に従って作成してください。

## doctor

| オプション | 意味・既定値 |
| --- | --- |
| `--json` | 一つの機械可読 JSON レポート |
| `--probe` | バージョンとヘルプに記載されたオプションを、各呼び出し三秒の上限で検査。既定は実行ファイルの探索のみ |
| `--route PROFILE` | 確認するツールの組み合わせ。既定は all |

PROFILE に指定できるのは `all`、`prepared-dash`、`progressive-mp4`、`fragmented-mp4`、`dash-presentation`、`fmp4-hls-presentation`、`mpeg-ts-hls`、`mpeg-ts-sample-aes-hls`、`mpeg-ts-hls-ts` です。変換結果に表示される処理経路名を、そのまま doctor に指定できるとは限りません。TS→DASH を含めてツール一式を診断する場合は all を使えます。

## inspect

| 引数・オプション | 意味 |
| --- | --- |
| `INPUT` | ローカル MP4、MPD、M3U8。リモート参照は取得しない |
| `--json` | バージョン付きメタデータレポート |

## analyze

| 引数・オプション | 意味・既定値 |
| --- | --- |
| `INPUT` | 必要なファイルがすべて揃ったローカル MP4・DASH・HLS、または配信内容が確定して取得条件を満たす HLS の URL |
| `--download-dir NEW_DIR` | URL に必須。原本を所有者専用で保存する新規ディレクトリ |
| `--fetch-policy FILE` | 例 `config/fetch.json`。URL 取得用の JSON。[書式・各項目・権限の例](https://kazuhide.github.io/drmr/ja/guide/#url-input) |
| `--keys-file FILE` | 例 `config/keys.json`。原本の復号に必要な入力鍵の JSON。[構造例と保護方法](https://kazuhide.github.io/drmr/ja/guide/#key-configuration) |
| `--packager-bin FILE` | Shaka Packager の実行ファイル。既定 packager。[配置と最低要件](https://kazuhide.github.io/drmr/ja/guide/#installation) |
| `--max-workers N` | 上限 1-64。既定は CPU・メモリから自動選択 |
| `--json` | 一つの入力解析レポート。input_admitted は変換結果の検証を意味しない |
| `--measure` | 数値計測を追加。--json が必要 |

ローカル入力に取得用オプションを付けると拒否します。URL の解析には常に新規の取得先が必要です。

## plan

| 引数・オプション | 意味 |
| --- | --- |
| `INPUT` | 変換先の候補と条件を確認するローカル入力 |
| `--to TARGET` | INPUT を使い --outputs がない場合に必須。clear、cenc、cbcs、hls-aes128、hls-sample-aes |
| `--format FORMAT` | mp4、dash、hls、hls-ts。--save なしでは複数候補、単一出力の保存計画では一形式。省略時は MP4/DASH/HLS に従い、HLS は fMP4。--outputs とは併用不可 |
| `--outputs FILE` | version 1 の[出力一覧 JSON の構造例](https://kazuhide.github.io/drmr/ja/guide/#inspect-plan)。--save が必要。--to・--format を置き換え、version 2 の実行計画を作る |
| `--json` | 実行せず、バージョン付き計画を JSON で表示 |
| `--save NEW_FILE` | 原本を確認して計画 JSON を新規保存。例 `conversion.plan.json`。INPUT と --output に加え、--to または --outputs が必要。[保存例](https://kazuhide.github.io/drmr/ja/guide/#inspect-plan) |
| `--output NEW_PATH` | 計画に記録する新規出力先。--save なしでは拒否 |
| `--keys-file FILE` | 原本確認の入力鍵 JSON。単一出力は version 1、--outputs は version 2。--save 時のみ。[鍵の構造例](https://kazuhide.github.io/drmr/ja/guide/#key-configuration) |
| `--packager-bin FILE` | 原本確認の Shaka Packager 実行ファイル。--save 時のみ。既定 packager。[準備方法](https://kazuhide.github.io/drmr/ja/guide/#installation) |
| `--max-workers N` | 原本確認の同時作業上限 1-64。--save 時のみ |
| `--measure` | 標準出力に計測を追加。--json と --save が必要。保存ファイル自体は実行用の計画 |
| `--mpd MPD` | 準備済み DASH のレンディションを列挙する別モード |
| `--asset-id ID` | --mpd モードのアセット名を指定 |

## convert

| 引数・オプション | 意味・既定値 |
| --- | --- |
| `INPUT` | 対応するローカル原本、または配信内容が確定した HLS の URL。暗号化入力には入力鍵が必要。--plan 時は省略 |
| `--to TARGET` | --plan・--outputs がない場合に必須。出力保護方式 |
| `--format FORMAT` | 一つの出力形式。省略時は MP4/DASH/HLS に従い、HLS は TS 入力でも fMP4 |
| `--outputs FILE` | version 1 の[出力一覧 JSON](https://kazuhide.github.io/drmr/ja/guide/#inspect-plan)。すべての結果を一つの新規出力ルートに作る。--to・--format・--plan とは併用不可 |
| `--output NEW_PATH` | --plan がない場合に必須。新規ファイルまたは新規ディレクトリ |
| `--plan FILE` | version 1 または 2 の[生成済み計画 JSON](https://kazuhide.github.io/drmr/ja/guide/#inspect-plan)を実行。原本・出力条件の上書きは拒否 |
| `--keys-file FILE` | 所有者専用の鍵 JSON。通常は version 1。--outputs または version 2 の計画では、出力が一つでも version 2。[鍵と DRM 情報の構造例](https://kazuhide.github.io/drmr/ja/guide/#key-configuration) |
| `--download-dir NEW_DIR` | URL 入力に必須。取得した原本の保存用に新しく作るディレクトリ |
| `--fetch-policy FILE` | 例 `config/fetch.json`。アクセス権を制限した URL 取得用 JSON。[設定項目と例](https://kazuhide.github.io/drmr/ja/guide/#url-input) |
| `--packager-bin FILE` | Shaka Packager の実行ファイル。既定 packager。[準備方法](https://kazuhide.github.io/drmr/ja/guide/#installation) |
| `--hls-validator-bin FILE` | 要求した HLS 検査用の Apple 実行ファイル。既定 mediastreamvalidator。[検査ツールの準備](https://kazuhide.github.io/drmr/ja/guide/#verification) |
| `--dash-validator-bin FILE` | ローカル DASH-IF アダプターを実行する PHP 実行ファイル。既定 php。[配置先と設定](https://kazuhide.github.io/drmr/ja/guide/#verification) |
| `--max-workers N` | メディアの同時処理数の上限 1-64。既定は CPU・メモリから自動選択 |
| `--json` | スキーマのバージョンを含む一つの結果を表示。通常の進捗表示は省略 |
| `--measure` | 数値の計測情報を追加。--json が必要 |

汎用 CLI に ffmpeg-bin/ffprobe-bin の独自フラグはありません。FFmpeg/ffprobe は起動環境の PATH を用います。

`--plan` は INPUT、--to、--format、--outputs、--output、--download-dir、--fetch-policy と併用できません。鍵・検証ツールの場所・並列数・レポート指定は実行時の設定として渡せます。各出力に保存したマニフェスト検証方針を実行時に上書きすることはできません。

## verify

| 引数・オプション | 意味・既定値 |
| --- | --- |
| `OUTPUT` | 比較する結果の MP4、MPD、M3U8。--plan 時は省略 |
| `--source INPUT` | --plan・--manifest-only がない場合に必須。元の入力。結果から原本を推測しない |
| `--plan FILE` | [生成済み計画 JSON](https://kazuhide.github.io/drmr/ja/guide/#inspect-plan)に記録した原本と出力条件を照合し、コンテンツ全体の一致を確認。OUTPUT・--source とは併用不可 |
| `--manifest-only` | Rust の内蔵 Linter による HLS/DASH 検査。media_verified は false。--plan・--source・--keys-file とは併用不可 |
| `--external-validator` | Apple／DASH-IF の検査も追加する。--manifest-only が必要 |
| `--manifest-profile PROFILE` | 内蔵検査のプロファイル。--manifest-only が必要。既定 base、apple-vod は HLS のみ |
| `--keys-file FILE` | 原本と結果の鍵 JSON。version 2 の計画では、出力が一つでも version 2。それ以外は version 1。[鍵の構造例](https://kazuhide.github.io/drmr/ja/guide/#key-configuration) |
| `--packager-bin FILE` | Shaka Packager の実行ファイル。既定 packager。[準備方法](https://kazuhide.github.io/drmr/ja/guide/#installation) |
| `--hls-validator-bin FILE` | Apple の実行ファイル。既定 mediastreamvalidator。[検査ツールの準備](https://kazuhide.github.io/drmr/ja/guide/#verification) |
| `--dash-validator-bin FILE` | ローカル DASH-IF アダプターを実行する PHP 実行ファイル。既定 php。[配置先と設定](https://kazuhide.github.io/drmr/ja/guide/#verification) |
| `--max-workers N` | 上限 1-64。利用可能な資源に応じて実際の並列数は減る場合がある |
| `--json` | 一つの独立検証レポート |
| `--measure` | 計測を追加。--json が必要 |

verify は検証用のコマンドです。target や format を指定して変換結果を作り直すことはできません。

version 1 の計画検証では plan_id、source_unchanged、output_matches_plan を返し、コンテンツ全体の比較結果を verification の下に入れます。成功には source_unchanged、output_matches_plan、verification.passed がすべて true である必要があります。PlayReady 情報を明示した場合は、現在の出力とも照合します。version 2 は各出力のメディア・暗号方式・シグナリング・外部検査を一覧で返し、全体の passed も示します。内容が一致していても、それだけで計画した暗号方式や DRM 宣言を確認できるわけではありません。どちらのモードもライセンス取得を伴う再生の確認は行いません。

## run

| オプション | 意味・既定値 |
| --- | --- |
| `--mpd MPD` | 必須。対応する準備済み DASH マニフェスト |
| `--output ROOT` | 必須。ROOT/asset-id の下に出力 |
| `--asset-id ID` | アセット名。既定は原本ディレクトリ名 |
| `--hls-master M3U8` | 原本 HLS master。--no-manifests を使わなければ必要 |
| `--packager-bin PATH` | packager 実行ファイル、またはそれを含むディレクトリ。[準備済み DASH の実行例](https://kazuhide.github.io/drmr/ja/guide/#prepared-dash-batch) |
| `--max-workers N` | レンディション並列度の上限 |
| `--manifest-local-url-base URL` | ローカル HTTP 配信用ベース URL のマニフェストも生成 |
| `--log-format FORMAT` | text または jsonl。概要表示または一行に一つの JSON。既定は text |
| `--no-round-trip` | デバッグ用。原本と変換結果のデコード照合を省く |
| `--limit-renditions N` | デバッグ用。一部のレンディションだけ処理する |
| `--no-manifests` | デバッグ用。プレイリスト・マニフェストを生成しない |

## analyze-hls / package-hls

| analyze-hls のオプション | 意味 |
| --- | --- |
| `--source-config FILE` | 必須。入力 URL/パスとアクセス許可を記す、所有者専用 JSON。[source.json の構造例](https://kazuhide.github.io/drmr/ja/guide/#saved-hls) |
| `--asset-id ID` | 必須。保存アセット名 |
| `--output ROOT` | 必須。ROOT/ID/clear/source の下に保存 |

| package-hls のオプション | 意味 |
| --- | --- |
| `--analysis FILE` | 必須。analyze-hls が生成した analysis.json。[生成手順](https://kazuhide.github.io/drmr/ja/guide/#saved-hls) |
| `--drm-config FILE` | 必須。DRM シグナリング用の所有者専用 JSON。汎用の鍵 JSON とは別の書式。[drm.json の構造例](https://kazuhide.github.io/drmr/ja/guide/#saved-hls) |
| `--selection FILE` | 解析済み音声トラックの選択用 JSON。所有者専用。[選択ファイルの書式（英語）](https://github.com/kazuhide/drmr/blob/main/docs/usage.md#package-the-hls-snapshot) |
| `--packager-bin FILE` | Shaka Packager の実行ファイル。既定 packager。[準備方法](https://kazuhide.github.io/drmr/ja/guide/#installation) |
| `--max-workers N` | 全トラック間のセグメント処理上限 1-64 |

これらの暗号鍵は、DRM_KID/DRM_KEY/DRM_IV から受け取ります。汎用鍵 JSON を drm-config へ渡す方式ではありません。

## verify-snapshot / canonical / version

| コマンドと引数 | 意味 |
| --- | --- |
| `verify-snapshot PATH` | スナップショットディレクトリまたは analysis.json の記録を確認 |
| `verify-snapshot --files PATH` | 保存プレイリスト・メディアも再ハッシュ |
| `canonical PATH` | 任意の JSON を安定したキー順で表示。末尾の改行追加や秘密情報の除去は行わない |
| `version` | 版を表示。個別オプションなし |

## batch

| オプション | 意味・既定値 |
| --- | --- |
| `--work-list FILE` | 必須。schema ヘッダーとアセット行を持つ JSONL。一行ごとに JSON を書く。[作業リスト例](https://kazuhide.github.io/drmr/ja/guide/#prepared-dash-batch) |
| `--output ROOT` | 必須。出力ルート |
| `--resume-log FILE` | 必須。batch が再開状態を記録するファイル。鍵ファイルではない。[再開手順](https://kazuhide.github.io/drmr/ja/guide/#prepared-dash-batch) |
| `--storage STORAGE` | local または s3。既定 local |
| `--storage-root DIR` | ローカルオブジェクトキーの基点 |
| `--bucket NAME` | S3 bucket |
| `--region REGION` | 既定 ap-northeast-1。この版の S3 対応 region も同じ |
| `--output-prefix PREFIX` | ストレージの結果 prefix。既定 output |
| `--key-provider PROVIDER` | env または file。既定 env |
| `--key-file FILE` | file 方式の所有者専用アセット別 JSONL。汎用 --keys-file の JSON とは異なる。[一行ごとの鍵設定例](https://kazuhide.github.io/drmr/ja/guide/#prepared-dash-batch) |
| `--hls-config-dir DIR` | HLS ワークフロー用設定群 |
| `--packager-bin PATH` | packager 実行ファイル。準備済み DASH の batch では、それを含むディレクトリも指定可能。HLS を含む場合は実行ファイルを指定する。[準備方法](https://kazuhide.github.io/drmr/ja/guide/#prepared-dash-batch) |
| `--max-workers N` | 各アセット内の同時処理数の上限。アセット自体は一つずつ処理 |
| `--retry-failed` | 失敗項目の再試行を選択 |
| `--plan` | 実処理せず予定を表示 |
| `--max-attempts N` | アセット1件あたりの試行回数の上限。既定は 3 |
| `--per-asset-timeout-s SEC` | アセット1件あたりの処理時間の上限。既定は 3600 秒 |
| `--heartbeat-s SEC` | 状態通知の間隔。既定 60 秒 |

## serve

| オプション・環境 | 意味・既定値 |
| --- | --- |
| `--static-dir DIR` | 組み込み画面の代わりに配信する信頼済み UI ディレクトリ |
| `--cli FILE` | ジョブを実行する同じ版の drmr 実行ファイル。設定 JSON ではない。[Web の起動方法](https://kazuhide.github.io/drmr/ja/guide/#web-console) |
| `DRMR_CATALOG_ROOT` | 必須。準備済みメディアのカタログルート |
| `DRMR_OUTPUT_ROOT` | 必須。結果・保存 HLS・ログのルート |
| `DRMR_BIND_HOST` | 既定は loopback。リモート公開を想定しない |
| `DRMR_BIND_PORT` | 既定 8080 |
| `DRMR_MAX_WORKERS` | HLS パッケージの並列度上限 |
| `DRMR_HLS_PROFILES` | Web 用取得プロファイルの所有者専用 JSON ファイルへのパス。汎用 --fetch-policy とは別の書式。[プロファイルの構造例](https://kazuhide.github.io/drmr/ja/guide/#web-console) |

# 20. 資料とこのガイドの更新

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
