---
title: ユーザーガイド
updated: 2026-10-20
version: 0.1.0
---

# 1. はじめに

drmr は、既存の映像・音声の符号化を保ったまま、メディアの格納形式、配信形式、暗号化方式を変更するツールです。対象は、復号に必要な鍵を持ち、管理下のオンデマンドコンテンツを解析・変換・検証する技術者です。新しい画質を作るエンコーダーや、DRM ライセンスを取得するクライアントではありません。

このガイドは drmr 0.1.0 の操作方法と対応条件を説明します。更新日は 2026-10-20 です。公開ソースとヘルプを基準にし、利用する版が異なる場合は同梱ドキュメントも確認してください。関連するリポジトリのドキュメントは英語です。

## まず選ぶ操作

| 目的 | コマンド | わかること・実行すること |
| --- | --- | --- |
| 外部ツールを探す | `doctor` | 起動環境の PATH に必要な実行ファイルがあるか |
| ツールの版と機能を確認する | `doctor --probe` | バージョンとヘルプに必要なオプションがあるか。メディア処理はしない |
| 入力の種類と暗号宣言を見る | `inspect` | ローカル MP4・DASH・HLS のメタデータ。復号や実サンプルの検証はしない |
| 変換の意図と候補を確認する | `plan` | 指定した形式・暗号方式の組み合わせと、実行時に必要な条件 |
| メディアを変換する | `convert` | 実データの条件を確認し、変換・照合後に新しい出力を公開する |
| 原本と結果を改めて比べる | `verify` | 原本を独立に読み直し、内容・復号結果・時刻などを照合する |

通常は `inspect`、必要に応じて `plan`、続いて `convert`、最後に `verify` を使います。HLS でも、汎用変換の前に `analyze-hls` を実行する必要はありません。

## 最短の実行例

最初は、対応プロファイルの暗号化されていないローカル MP4 で動作を確認できます。出力の親ディレクトリは先に用意し、出力ファイル自体は存在しない名前にしてください。原本は変更されません。

```sh
drmr --version
drmr doctor --probe --route progressive-mp4
drmr inspect original.mp4 --json
drmr plan original.mp4 --to clear --format mp4 --json
drmr convert original.mp4 --to clear --format mp4 \
  --output clear.mp4
drmr verify clear.mp4 --source original.mp4 --json
```

暗号化された入力や出力には、後述の保護された鍵設定ファイルを渡します。メディアが対応範囲外なら、最初の例でも処理は拒否されます。`doctor` や `plan` の成功だけでは、その入力の変換成功を保証しません。

CLI が主な操作手段です。Docker は依存ツールを含めて実行する選択肢です。ローカル Web コンソールと Desktop は実験的な操作画面で、CLI の全機能を備えた汎用ファイル変換画面ではありません。

# 2. 形式・暗号・DRM の読み方

## 容器形式と配信形式

| 用語 | 意味 |
| --- | --- |
| MP4 | 符号化済みの映像・音声と、その解釈に必要な情報を格納する容器形式 |
| プログレッシブ MP4 | サンプルの位置と時刻をまとめた表を持つ MP4。通常の単一ファイル配布で使われる |
| fMP4 | fragmented MP4。初期化情報と、その後の小さな断片に分けた MP4。単一ファイルにも、DASH/HLS の分割配信にも使われる |
| MPEG-TS | MPEG transport stream。188 バイトのパケットを用いる容器形式。このガイドでは TS と略す |
| DASH | Dynamic Adaptive Streaming over HTTP。MPD というマニフェストで初期化情報とメディア断片を参照する配信方式 |
| HLS | HTTP Live Streaming。プレイリストでメディアを参照する配信方式。名前に Live を含むが、drmr の対象は完了したオンデマンド配信 |
| セグメント | 配信のために分割されたメディアの一部分。映像と音声で境界や個数が一致するとは限らない |
| トラック・レンディション | トラックは映像や音声の系列。レンディションは選択できる画質や音声などの版 |

HLS は TS と fMP4 の両方を使えます。`--format hls` は fMP4 HLS、`--format hls-ts` は TS HLS を選びます。「HLS 入力」という情報だけでは、実際の容器形式は決まりません。

## 暗号方式は別に指定する

AES は共通鍵暗号の名前です。CTR は counter mode、CBC は cipher block chaining の略で、どちらも AES の適用方法です。サンプルとは、符号化された映像の一枚や音声フレームなど、処理の単位になるデータです。

| `--to` | 暗号と適用単位 | 対応する出力 |
| --- | --- | --- |
| `clear` | 暗号化しない | 対応プロファイルの MP4、DASH、fMP4 HLS、TS HLS |
| `cenc` | MP4 サンプルに AES-CTR を適用する | MP4、DASH |
| `cbcs` | MP4 サンプルに AES-CBC を適用する。映像では暗号化するブロックと残すブロックのパターンを使う | MP4、DASH、fMP4 HLS |
| `hls-aes128` | HLS リソース全体に、パディングを伴う AES-CBC を適用する | fMP4 HLS、TS HLS |
| `hls-sample-aes` | TS 内の H.264・AAC フレームの定められた部分に AES-CBC を適用する | TS HLS |

部分サンプル暗号化は、コーデックのヘッダーなどを平文のまま残してサンプルの一部を暗号化する方式です。これは CTR/CBC の違いや、暗号化・スキップのパターンとは別の軸です。`cenc` と書かれていてもファイルの全バイトが暗号化されるわけではありません。

`cbcs` と HLS AES-128 は、どちらも CBC を使いますが、適用単位とメタデータが異なります。fMP4 HLS の CBCS と、FairPlay 向け TS の SAMPLE-AES も同じファイル形式ではありません。方式名を置き換えるだけでは変換できません。

これらの違いは W3C の MP4 暗号化説明、RFC 8216 の暗号化宣言、Apple の SAMPLE-AES 説明にも記載されています。一次資料のリンクは第20章にまとめています。

## DRM システムと暗号方式を分ける

Widevine、PlayReady、FairPlay は、鍵の取得や利用条件、プレーヤーとの連携を含む DRM システムです。drmr は既知の鍵を使ってメディアを変換し、対応する経路では必要なシグナリングを格納します。シグナリングとは、プレーヤーが DRM システムや鍵の場所を判断するための付加情報です。

暗号方式の変換、シグナリングの整合性確認、ライセンス取得、実機再生は別の確認事項です。メディア検証が通っても、対象機器がその容器・コーデック・暗号方式・DRM の組み合わせを再生できるとは限りません。実サービスに出す場合は、そのライセンスサーバーと機器で別途確認してください。

`cens` と `cbc1` は解析で識別できる場合がありますが、現在の変換先ではありません。対応表の「対象外」は drmr の現在の実装・検証範囲を表し、その形式で技術的に実現不可能という意味ではありません。

# 3. 入力と出力の対応表

## 容器形式から選ぶ

この表はローカルに保存された、完了済みオンデマンドメディアの汎用 `convert` が対象です。各セルの暗号方式に加えて、後述のコーデック・トラック・時刻・暗号配置の条件を満たす必要があります。URL 取得には別の制限があります。

| 入力 | MP4 出力 | DASH 出力 | fMP4 HLS 出力 | TS HLS 出力 |
| --- | --- | --- | --- | --- |
| MP4 | clear / cenc / cbcs。複数トラックは条件付き | clear / cenc / cbcs | clear / cbcs / AES-128 | 対象外 |
| DASH・fMP4 | clear / cenc / cbcs。単一トラックのみ | clear / cenc / cbcs | clear / cbcs / AES-128 | 対象外 |
| HLS・fMP4 | clear / cenc / cbcs。単一トラックのみ | clear / cenc / cbcs | clear / cbcs / AES-128 | 対象外 |
| HLS・TS、映像あり | 対象外 | clear / cenc / cbcs | clear / cbcs / AES-128 | clear / AES-128 / SAMPLE-AES |
| HLS・TS、音声のみ | 対象外 | clear / cenc / cbcs | clear / cbcs / AES-128 | clear / AES-128 / SAMPLE-AES |

MP4 に映像と音声をまとめた出力は、同じプログレッシブ MP4 の対応トラックを保持する、後述の有限音声範囲プロファイルに限ります。DASH/HLS の別々のトラックや異なるファイルを一つの MP4 に組み合わせる機能ではありません。

## HLS の入力暗号方式

| 入力の容器 | 読み込める保護 | 必要な入力鍵 |
| --- | --- | --- |
| fMP4 | clear、cbcs、AES-128 | cbcs は KID ごと、AES-128 はプレイリストの正確な鍵 URI ごとの設定 |
| TS | clear、AES-128、SAMPLE-AES | 暗号化入力は鍵 URI ごとの設定。SAMPLE-AES は関連付けられた IV も必要 |

fMP4 メディアプレイリストは一つの音声または映像トラックを持つ必要があります。マスタープレイリストで別々の映像・音声プレイリストを関連付ける構成は扱えます。保持できない言語・選択情報や、混在した音声グループの関連付けは拒否します。

## MP4 の有限音声範囲

「有限音声範囲」は別の入力形式の名前ではありません。MP4 の編集情報が、復号した音声のうち実際に再生する開始位置と長さを指定している状態です。エンコーダーの初期遅延や最後の詰め物を除外するために、一般的な MP4 でも使われます。

対応する入力は、同一の完全なプログレッシブ MP4 に入った AAC-LC 音声一つと、任意で H.264 映像一つです。音声の格納識別子は `mp4a`、映像は `avc1` です。音声の編集は通常速度、一つの連続した範囲、負でない開始位置で、長さを音声クロックの整数に正確に変換できる必要があります。

| 入力の保護 | MP4 出力 | DASH 出力 | fMP4 HLS 出力 |
| --- | --- | --- | --- |
| clear | clear / cenc / cbcs | clear / cenc / cbcs | clear / cbcs / AES-128 |
| cenc | clear / cenc / cbcs | clear / cenc / cbcs | clear / cbcs / AES-128 |
| cbcs | clear / cenc / cbcs | clear / cenc / cbcs | clear / cbcs / AES-128 |

clear MP4 出力はプログレッシブのサンプル表を保持します。暗号化 MP4 出力はフラグメント化され、この条件を満たす映像・音声を一つのファイルに保持できます。DASH/HLS 出力は役割ごとのトラックに分けます。入力暗号化には各トラックの鍵が必要です。

複数の編集、先頭に空の編集を入れた遅延、速度変更、追加トラック、別ファイルの結合、鍵の切り替え、入力の DRM 初期化ヘッダーは、この変換プロファイルでは拒否します。cenc は検証済みの 8/16 バイトのサンプル IV、cbcs は検証済みの 16 バイト定数 IV とパターンなど、実装が確認できる配置に限ります。任意の暗号化 MP4 を受け入れるという意味ではありません。

## TS の入力条件

対応する TS は、固定された一つの番組構成、プログレッシブ H.264 映像一つ、必要に応じて 48 kHz AAC-LC 音声一つを持つ完了済み HLS です。同じ一番組・48 kHz AAC-LC の条件で、DASH、fMP4 HLS、TS 出力では音声のみも対応します。

TS パケットは 188 バイトです。動画の SPS/PPS という復号設定は固定され、AAC は 7 バイトの ADTS ヘッダー、チェックサムなし、一単位あたり一つの音声フレームが必要です。複数番組、設定変更、壊れたパケット、時計の周回、不連続、インターレース映像、44.1 kHz AAC、未対応記述子などは拒否します。

SAMPLE-AES 入力では暗号方式を途中で変更できません。鍵形式は `identity` または `com.apple.streamingkeydelivery` の一つで、指定する形式版は 1 です。異なる鍵形式の同時宣言は拒否します。非ゼロの音声初期遅延も対象外です。

TS 出力で映像と音声がある場合は、それぞれ別の TS プレイリストと、それを関連付けるマスタープレイリストを作ります。一つの TS セグメント内に両方を多重化する出力は、現在の実装では作りません。

## 共通の対象外と上限

ローカル DASH は静的な一つの Period、連続した有限の SegmentTimeline、対応する相対参照を持つ必要があります。ライブ、動的 DASH、複数 Period、バイト範囲、不連続、保持できないイベントやレンディション情報などは対象外です。汎用変換で字幕を一律に保持することもできません。対応する字幕には別の HLS スナップショット経路があります。

汎用変換の上限は、作業用に保存する一トラックあたり 16 GiB、64 トラック、DASH の参照セグメント 100,000 個です。別途、初期化メタデータや断片ごとの上限、CPU・メモリの受け入れ制限があります。上限を超えた部分だけを無視して成功にはしません。

参照（英語）: [変換対応範囲](https://github.com/kazuhide/drmr/blob/main/docs/conversion-support.md)、[処理・検証条件](https://github.com/kazuhide/drmr/blob/main/docs/media-processing.md)。

# 4. 導入と依存ツール

## ソースから CLI を導入する

ソースビルドには Rust 1.95 以上が必要です。リポジトリは特定の Rust 版に自動で切り替えず、利用者が選んだツールチェーンを使います。1.99 などの記載は検証に使った版で、最低要件をその版に固定する意味ではありません。ビルド済み CLI アーカイブを使う場合、Rust のインストールは不要です。

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

`--locked` は Cargo.lock に記録した依存関係を使う指定です。Cargo.toml は対応する依存ライブラリの基準版と互換範囲、Cargo.lock は検証した具体的な組み合わせを記録しています。依存ライブラリの更新後も最低 Rust 版と安定版の CI が通る必要があります。

## 外部メディアツール

| 実行ファイル | 対応する下限 | 用途 |
| --- | --- | --- |
| `ffmpeg` / `ffprobe` | 9.0.1 | メディアの読み取り、復号・復号後データ、パケット・時刻の確認 |
| `packager` | Shaka Packager 3.8.0 | MP4 サンプル暗号化、格納形式の変更、セグメント化 |
| `mp4dump` | Bento4 1.6.0、build 641 | 準備済み DASH などの MP4 構造確認 |

必要な実行ファイルを信頼できる配布元から入手し、シェルがプログラムを検索する PATH に追加してください。CLI アーカイブと Desktop バンドルには、これらのメディアツールは含まれません。Docker イメージには含まれます。

下限より新しくても、必要なオプションや完全なパケット時刻を出力できないビルドは使えません。Bento4 はバナーに build 番号を出さないため、`doctor --probe` の `build-unverified` だけで build 641 以上と判断せず、配布元でも確認してください。

## doctor の使い分け

```sh
drmr doctor
drmr doctor --probe --route all --json
drmr doctor --probe --route dash-presentation
```

既定の `doctor` は実行ファイルの探索だけを行い、外部ツールを起動しません。`--probe` はバージョンとヘルプを、各呼び出し 3 秒、標準出力・標準エラー各 1 MiB の上限で確認します。生の出力はレポートに含めません。メディア、鍵、ライセンス、再生機器は検証しません。

`--route` は必要なツールセットを選びます。`progressive-mp4` は FFmpeg と ffprobe、`prepared-dash` と `all` は Bento4 も必要です。他の処理経路は必要に応じて Shaka Packager を確認します。選んだ経路と無関係なツールは任意扱いですが、プロセスの終了確認ができない場合は任意の検査でも準備完了にはなりません。

レポートの `ready` は選んだ診断項目が通ったという意味です。`media_compatibility_checked` は false のままです。`path` は実際に解決した実行ファイルの場所です。GUI の起動環境とターミナルの PATH は異なることがあるため、Desktop 内の外部ツール確認も使ってください。

参照（英語）: [必要条件](https://github.com/kazuhide/drmr/blob/main/docs/requirements.md)、[Windows](https://github.com/kazuhide/drmr/blob/main/docs/windows.md)。

# 5. 鍵設定と秘密情報の扱い

## 入力鍵と出力鍵を分ける

`convert` と `verify` は `--keys-file` で保護された JSON を読みます。環境変数への暗黙のフォールバックはありません。入力鍵は原本の復号、出力鍵は結果の暗号化・検証に使います。同じ暗号方式への変換でも、入力鍵と出力鍵は独立です。

鍵は 16 バイトを 32 桁の十六進数で表します。KID は key identifier、つまり鍵を選ぶための識別子で、暗号鍵そのものではありません。IV は initialization vector、暗号化の開始状態を指定する値です。KID やメディア内の IV は公開メタデータになる場合があります。

macOS/Linux では、まず所有者だけが入れるディレクトリを作り、その中でエディターや秘密管理ツールから設定を作成してください。次の例の `keys.json` は、構造例を保存した後で権限も確認します。

```sh
umask 077
mkdir config
chmod 700 config
```

```sh
chmod 600 config/keys.json
```

実行時には、設定が所有者のみアクセス可能な通常ファイルで、単一のハードリンクであることなどを確認します。シンボリックリンク、他ユーザーが読める設定、重複キー、不明なフィールド、未対応の schema_version、過大な設定は拒否します。出力ツリーとバージョン管理の外に置いてください。Windows は後述の所有者専用 ACL を使います。

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

入力と出力が同じ URI を広告する場合でも、両側の鍵・IV は別の役割です。`output.iv` は SAMPLE-AES TS 専用で、clear、MP4 サンプル暗号化、AES-128 出力では拒否します。

## ログ、メモリ、一時ファイル

| 情報・場所 | 実装上の扱いと限界 |
| --- | --- |
| 正常な結果・ジョブ履歴 | 内容鍵を含めない。KID は公開の識別情報として扱う |
| 認識済み秘密値の診断 | 鍵・設定 IV・シグナリングを認識し、対応する十六進、Base64、エスケープ表現などもマスクする |
| パス・名前・メディアタグ | 一律に秘密とはみなさない。成功レポートや一部診断に表示される |
| 外部プロセスの引数 | FFmpeg や Shaka に平文の鍵を渡す経路がある。OS のプロセス情報から見える可能性がある |
| メモリ | 通常のプロセスメモリに平文が載る。一部の Rust バッファは破棄時に消去するが、全コピーの消去は保証しない |
| 作業用ディスク | 復号済みメディアや平文鍵ファイルを所有者専用の一時領域に書く経路がある。メモリ内だけの処理ではない |
| 通常終了後 | 所有する一時ファイルを削除する。強制終了や未確認の子プロセスにより残る場合がある |

消去は Secure Enclave などの隔離実行を意味しません。外部プロセス、ブラウザー、アロケーターのコピー、スワップ、クラッシュダンプ、バックアップは一括消去の保証外です。ファイル削除も、ストレージやスナップショットからの安全な消去を保証しません。

ライセンス URL、HLS 鍵 URI、PSSH に含めた URL は、意図して出力メディアへ書かれる場合があります。Base64 や十六進表現は暗号化ではありません。公開する予定のない認証トークンをこれらの値へ含めないでください。入力 URL、原本プレイリスト、保存したダウンロードにも署名や認証情報が含まれることがあります。

信頼できる OS アカウント、外部メディアツール、ブラウザーを使い、作業ディレクトリも鍵と同様に保護してください。共有するログは、マスクの対象外となる名前や第三者ツールの出力も含めて確認してください。Rust API が返す生の外部出力バッファは、安全なログ形式ではありません。

参照（英語）: [鍵とローカルセキュリティ](https://github.com/kazuhide/drmr/blob/main/docs/security.md)。

# 6. inspect と plan

## inspect は宣言と初期化情報を読む

```sh
drmr inspect media.mp4
drmr inspect source/manifest.mpd --json
drmr inspect source/master.m3u8 --json
```

入力形式は内容から判定します。JSON は `schema_version: 1` を持ち、形式、トラック・サンプル記述ごとの保護方式、既知の KID、DRM システム名、`findings` の説明トークンなどを返します。内容鍵、IV、ライセンス URL、鍵 URL は含めません。

MP4 は初期化メタデータを読み、メディアの全ペイロードを走査しません。DASH/HLS は宣言を読みます。いずれも `media_verified: false` で、全サンプルの復号・デコード・再生は証明しません。未対応や不完全な暗号情報は `unknown` のままです。HLS の SAMPLE-AES 宣言だけから MP4 の暗号方式を断定しません。

HLS の子プレイリストは、入力ディレクトリ配下のローカル参照だけをたどります。リモート、欠落、循環、ツリー外参照は解決しません。プレイリスト・メディア・鍵をネットワークから取得しません。

解析の主な上限は、一マニフェスト 4 MiB、合計 16 MiB、32 プレイリスト、128 参照、トラックごとに 64 KID、MP4 の movie メタデータ 16 MiB です。変換本体や URL 取得の上限とは別です。

## plan は条件付きの実行候補を示す

```sh
drmr plan media.mp4 --to cbcs --format dash --json
drmr plan source/master.m3u8 --to clear --format dash,hls
```

`plan INPUT` には `--to` が必要です。`--format` を省略すると入力形式を引き継ぎます。複数形式はカンマ区切りで計画できますが、`convert` は一回につき一形式です。

`compatible` は宣言と指定の組み合わせが整い、すべての要求形式に候補経路があることを表します。`execution_available` には、出力形式が一つであることも必要です。複数形式の計画は成功しても `execution_available: false` になり得ます。

どちらも実メディアの適合、鍵の正しさ、ツールの存在、実際の処理ハンドラーの確定を保証しません。`source-profile-validation-required` などの条件は実行時に確認します。SAMPLE-AES の宣言が整っていても、実際の TS 構成や IV の検証は残ります。計画結果をそのまま信用済み実行データとして投入することもできません。

`details` がある場合は、宣言された暗号、暗号化単位、パターン、未確定の範囲を示します。変換成功レポートの `processing` は実際に選んだ経路です。計画の候補と区別してください。

互換の `plan --mpd MPD` は、準備済み DASH のレンディションとセグメント数を読む別モードです。汎用の `plan INPUT --to ...` とは入力配置が異なります。具体例は「準備済み DASH とバッチ処理」に示します。

参照（英語）: [処理経路と解析の証拠](https://github.com/kazuhide/drmr/blob/main/docs/processing-routes.md)。

# 7. ローカル変換の実例

## 共通の実行規則

`convert INPUT --to ... --format ... --output ...` は、ローカルの MP4、DASH、HLS を内容から判定します。一回の実行では出力形式を一つ選びます。`--format` を省略すると入力形式を引き継ぐため、TS と fMP4 HLS の違いが重要な場合は明示してください。

ここで入力形式の HLS は容器を区別しません。TS HLS 入力でも、省略時の出力は hls、つまり fMP4 HLS です。TS の出力を選ぶ場合は `--format hls-ts` を明示します。

出力先は、新しい MP4 ファイル、または新しい DASH/HLS ディレクトリです。親は存在している必要があります。既存出力は上書きしません。入力を専用の作業領域に保存し、内容・時刻・原本の整合性と停止要求を確認してから完成結果を公開します。ここでの公開とは指定したローカル出力先に配置することで、インターネットへのアップロードではありません。

以下の例は対応メディアと必要な鍵設定を準備済みとします。鍵設定名は例で、拡張子以外の決まりはありません。

## MP4 を MP4 または DASH へ

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

音声の有限再生範囲を持つ対応 MP4 は、同じ `convert` で処理します。専用の「音声編集 MP4」入力コマンドはありません。開始・終了位置を処理に合わせて短くしたり、最後の音声パケットを捨てたりして成功にしません。

## 暗号化 DASH を clear または別の暗号へ

clear 出力には入力側の鍵だけを持つ設定を使います。

```sh
drmr convert protected/manifest.mpd --to clear --format dash \
  --output clear-dash --keys-file config/source.json
drmr verify clear-dash/manifest.mpd \
  --source protected/manifest.mpd --keys-file config/source.json --json
```

別の出力鍵を使って CBCS に変える場合は、入力・出力両方を持つ設定を使います。

```sh
drmr convert cenc-source/manifest.mpd --to cbcs --format dash \
  --output cbcs-result --keys-file config/both.json
drmr verify cbcs-result/manifest.mpd \
  --source cenc-source/manifest.mpd --keys-file config/both.json --json
```

## clear HLS を fMP4 HLS へ

```sh
drmr convert source/master.m3u8 --to cbcs --format hls \
  --output cbcs-hls --keys-file config/hls-output.json
drmr verify cbcs-hls/master.m3u8 --source source/master.m3u8 \
  --keys-file config/hls-output.json --json
```

CBCS HLS には `output.hls_key_uri` が必要です。AES-128 にする場合は `--to hls-aes128` と対応する出力設定を使います。鍵 URI のサーバー運用や FairPlay のライセンス提供は drmr の外で行います。

## TS HLS から、DRM 有無を選んだ DASH へ

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

上は clear TS 入力の例です。AES-128 / SAMPLE-AES 入力は、それぞれの入力鍵と必要な IV を `input.hls_keys` に追加します。`--to cenc` も対応する DASH 出力です。暗号化メディアを作ったことと、特定 DRM 用のライセンス情報を設定したことは別です。

## 音声のみの TS を fMP4 HLS へ

完了済みの音声のみの TS プレイリストから、clear、CBCS、AES-128 の fMP4 HLS を作れます。CBCS には `output.hls_key_uri` を持つ出力鍵設定を使います。暗号化入力には、それとは別に入力鍵 URI の対応付けも必要です。

```sh
drmr convert audio-ts/master.m3u8 --to cbcs --format hls \
  --output audio-fmp4-hls --keys-file config/hls-output.json
drmr verify audio-fmp4-hls/master.m3u8 \
  --source audio-ts/master.m3u8 --keys-file config/hls-output.json --json
```

Shaka Packager で音声トラック全体を分離し、新しいセグメントを作ります。FFmpeg/FFprobe は解析、復号、ハッシュの照合に使います。原本の順序付き PES、全 AAC フレーム、絶対 PTS/DTS/継続時間を照合の基準に保ちます。二秒のセグメント目標に合わせて AAC フレームを切り落とすことはしません。470 フレームの TS 回帰入力は、一チャンネルあたり 481,280 サンプルを保持します。有限範囲で 480,000 サンプルを提示するプログレッシブ原本とは、再生範囲が異なります。

## FairPlay 向け SAMPLE-AES TS を作る

```sh
drmr convert clear-ts/media.m3u8 --to hls-sample-aes --format hls-ts \
  --output fairplay-ts --keys-file config/sample-aes-output.json
drmr verify fairplay-ts/master.m3u8 --source clear-ts/media.m3u8 \
  --keys-file config/sample-aes-output.json --json
```

反対に、対応する SAMPLE-AES TS を clear にする例です。

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

この機能は、同じ完全なファイルからの H.264 映像と任意の AAC 音声を対象とする限定プロファイルです。一般の有限 AAC 変換表の全セルにシグナリングを追加できるわけではありません。既存 PSSH・保護情報や保持未検証の不透明なメタデータを拒否します。詳細な入力条件は公開の対応表を確認してください。

ライセンス URL は絶対 HTTP(S) で、認証情報・フラグメント・空白・制御文字なし、UTF-8 で 4096 バイト以内です。指定した文字列をそのまま PSSH/PRO と MPD へ含め、URL へのリクエストは行いません。すべてのトラックの実初期化情報と MPD のシグナリングを照合します。検証結果の `license_playback_verified` は false のままです。

# 8. URL の取得を変換に含める

## 保存先を指定して convert を実行する

```sh
drmr convert "https://example.com/vod/master.m3u8" \
  --download-dir ./saved-source --to cbcs --format hls \
  --output ./converted --keys-file config/hls-output.json --json
```

URL 入力では `--download-dir` が必須です。取得先と出力先はどちらも新規で、互いを包含せず、それぞれの親ディレクトリが存在する必要があります。既存ダウンロードを上書きしたり、自動的に再利用したりしません。ローカル入力に `--download-dir` や `--fetch-policy` は不要です。

取得対象は clear、完了済み HLS の対応する TS/fMP4 プロファイルです。リモート DASH、単独 MP4、暗号化 HLS、ライブ、バイト範囲は取得対象外です。ローカルで読める暗号化 HLS があっても、その URL 取得まで対応したことにはなりません。鍵 URI も取得しません。

元のプレイリストとリソースを保存し、ローカル参照にした `saved-source/input.m3u8` を作ります。原本プレイリストのバイト列は別に保持し、ローカル版では参照だけを書き換えます。時刻、シーケンス番号、音声の関連付けなどを解析結果から作り直す方式ではありません。

## 完成した取得結果をオフラインで使う

```sh
drmr convert saved-source/input.m3u8 --to cbcs --format hls \
  --output retry-hls --keys-file config/hls-output.json
drmr verify converted/master.m3u8 --source saved-source/input.m3u8 \
  --keys-file config/hls-output.json --json
```

取得が完了した後は、配信元へ再接続せず保存した同じ原本で処理・検証できます。変換が失敗しても完成した取得結果は残ります。未完了のダウンロードを原本として採用しません。停止時は取得やメディア処理を止めてから後片付けします。

出力配置の直前にも保存した原本とローカル参照を確認します。取得後に原本が改変・削除されると出力を公開しません。取得済みのディレクトリを既存 URL 処理の resume オプションとして渡すこともできません。再実行はローカルのエントリーポイントを使ってください。

## 取得ポリシー

既定は HTTPS と入力 URL と同じ origin だけです。origin はスキーム、ホスト、ポートの組み合わせです。別 origin の参照や認証には所有者専用の `--fetch-policy` JSON を使います。

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

```sh
drmr convert "https://example.com/vod/master.m3u8" \
  --download-dir saved-source --fetch-policy config/fetch.json \
  --to clear --format hls --output clear-result
```

ヘッダーは設定した origin にだけ送ります。リダイレクト先と子参照にも許可が必要です。HTTP とプライベートネットワークへの接続は既定で無効で、必要な場合だけ `allow_http`、`allow_private_network` を明示します。JSON に認証情報がある場合は、鍵設定と同じく秘密として扱います。

主な追加設定は `max_playlist_bytes`、`max_playlists`、`max_segments_per_track`、`max_resource_bytes`、`max_total_resources`、`min_free_disk_bytes`、`timeout`、`attempts` です。数値の型と範囲、不明・重複フィールドを厳格に確認します。`connections` は取得の上限で、CPU・メモリの受け入れや `--max-workers` により実際は少なくなる場合があります。

取得側の実装上限は、一プレイリスト 4 MiB、プレイリストとメディアの合計リソース 100,000、予定するテキスト・URL メタデータ 16 MiB です。リソース本体は私有ファイルへ逐次保存します。ポリシーを緩めても、メディアプロファイルの上限は変わりません。

成功 JSON の `download` には保存先の表示パス、エントリーポイントと相対パス、`retained: true` が入ります。入力 URL とリクエストヘッダーは含めません。一方、保存した原本には署名 URL などが残り得るため、保存先を保護してください。

# 9. 検証の意味と処理の単位

## 四つの照合

`verify` は結果と原本をそれぞれ独立に読み、次を確認します。DASH/HLS では、出力ディレクトリだけでなく実際の `manifest.mpd` または `master.m3u8` を指定してください。

| レポートの項目 | 確認する内容 |
| --- | --- |
| `stream_metadata` | コーデック設定、映像寸法・画素形式、音声レート・チャンネル、色、回転、言語など。成功値は `pass` |
| `packet_payloads_equal` | 順序と個数を含む符号化済みデータの一致。容器をまたぐ場合は検証済みの符号化単位を比較する |
| `packet_timing_equal` | 全パケットの表示時刻・復号時刻・継続時間、対応する場合は正確な再生範囲 |
| `decoded_content_equal` | 完全なデコード映像・音声の一致。対応有限音声では全デコード内容と各自の再生範囲の両方 |

PTS は presentation timestamp、表示する時刻です。DTS は decode timestamp、復号処理に必要な時刻です。B ピクチャーなど、表示順と復号順が異なる映像では両者がずれます。相対的な音声・映像の同期も含め、絶対時刻で比較します。

`packet_timing_equal` には再生範囲の比較も含まれます。たとえば全映像パケットの時刻と復号画像が一致していても、原本の有限な編集範囲が結果に保持されていなければ不一致です。変換前の入力準備でも、元の開始位置・長さを正確に保持する必要があります。

`passed`、`media_verified` と、個々の結果を確認してください。`license_playback_verified` は false です。ファイルが存在すること、暗号宣言があること、ローカルで一度再生できたことは、四つの照合を代替しません。

## AAC と二秒セグメント

一般的な AAC-LC フレームは 1,024 サンプルです。48,000 サンプル/秒では約 21.333 ミリ秒になり、二秒は 93.75 フレームです。フレームの途中で符号化データを切れないため、音声の境界や個数は二秒の映像セグメントと一致しないことがあります。

この差は、音声の欠落・重複・すき間・重なり・累積ずれを許す理由にはなりません。「一フレーム以内なら同じ」という許容差は使わず、すべてのパケット時刻と全サンプルを確認します。

十秒の試験 MP4 は、470 AAC フレーム、全デコード 481,280 サンプル/チャンネルを持ちます。先頭 1,024 サンプルの初期遅延と、末尾 256 サンプルを除いた再生範囲は 480,000 サンプル/チャンネルです。TS にした原本が詰め物を含めて提示する場合は、その TS 自身の範囲を基準にし、以前の MP4 に合わせて短くはしません。

## 有限再生範囲を別に照合する理由

圧縮パケットが同じでも、MP4 の編集情報が消えると、再生すべきでない先頭や末尾が聞こえる場合があります。FFmpeg 9.0.2 の一部のフラグメント化 MP4 読み取りでは、有限の末尾切り詰めが通常デコードに反映されません。このため、単に同じツールで普通に復号して比較するだけでは足りません。

対応する有限 AAC 経路では、原本と結果を自動編集・破棄なしで全デコードして、全内容と個数を比較します。続いて、各ファイル自身から検証した開始位置・長さを一度だけ適用し、提示される内容と個数も比較します。さらにプログレッシブ原本の通常デコードが、その原本自身の範囲と一致することも確認します。候補に合わせて原本を切る処理ではありません。

比較はデコーダーの 32 ビット浮動小数点出力を使い、16 ビット整数化で生じる追加の丸めを避けます。ハッシュと範囲選択は固定サイズのバッファへ流して処理し、完全な非圧縮音声を一括でメモリに保持しません。ネイティブの再生設定、音量、速度、有効フラグ、映像表示寸法も確認します。プレーヤー全般が同じ編集を適用することの保証ではありません。

## マニフェストの時刻も照合する

TS の時計はゼロ以外から始まることが多く、音声が最初の映像より先に始まる場合もあります。TS→DASH では全トラックの原本表示時刻の最小値を共通の始点とし、DASH の presentation-time offset で配信時刻へ対応させます。トラックごとに独立にゼロへずらしません。

パケットとセグメントは整数や有理数で正確に扱います。TS→DASH の最終 Period、有限範囲の DASH/HLS の提示時間を小数文字列にする場合は、正確な終端を小数九桁に切り上げます。差は 1 ナノ秒未満で、これは記述形式の規則です。パケット時刻の誤差許容ではなく、書かれた値が計算結果と異なれば拒否します。

通常の有限編集を持たない HLS では、EXTINF の小数表現に対して一メディアクロックまたは約一ミリ秒の大きい方という別の丸め規則があります。これも全パケット時刻の厳密比較を緩めません。有限音声の一サンプル違いを、この丸めで受け入れることはありません。

## セグメントを一つずつ再暗号化するのか

| 経路 | 主な処理単位 |
| --- | --- |
| 汎用 `convert` | 原本トラックの初期化情報と断片を順に専用ディスク領域へ組み立て、必要な復号・格納形式変更・再セグメント化・暗号化を行う |
| TS を読む汎用変換 | 原本 TS の構成・連続性・時刻を先に検査。SAMPLE-AES は Rust で符号化フレームを復号し、完全トラックとして新たにパッケージする |
| 準備済み DASH の `run` | 通常は全レンディションの明文参照を作って再セグメント化。条件付き AAC では元の境界を保持するセグメント別経路も使う |
| HLS スナップショットの `package-hls` | 初期化情報とともに各セグメントを処理・暗号化する。完全な提示を検証するために結合する処理もある |

汎用 `convert` は、各元セグメントをその場で順に再暗号化するだけの処理ではありません。入力セグメント境界を必ずそのまま出す機能でもありません。映像・音声の符号化と正確な提示を保ち、二秒を目標として再分割します。HLS AES-128 は、その後の出力リソースごとに適用します。

作業用ディスクには原本、明文参照、候補などが存在するため、元データだけの容量では足りない場合があります。必要量は経路により異なります。ディスクとアクセス権を確保し、原本と鍵は独立検証が終わるまで保持してください。

参照（英語）: [処理と完全メディア検証](https://github.com/kazuhide/drmr/blob/main/docs/media-processing.md)。

# 10. HLS スナップショット経路

## 汎用 convert との違い

`analyze-hls` は読み取り専用のメタデータ表示ではありません。clear HLS を取得またはコピーし、メディアを調べ、保存したファイルと `analysis.json` を含むスナップショットを作ります。`package-hls` はそれを確認して DRM シグナリング付きでパッケージします。名前に反して DASH と HLS の両方の出力を扱う別ワークフローです。

対応する WebVTT や MP4 内 TTML の字幕プロファイルなど、汎用 `convert` と違う対象を持ちます。一方、解析元は clear、完了済み HLS に限定され、暗号化入力、ライブ、不連続、未対応トラック・字幕は拒否します。二つの経路の対応表を混ぜないでください。

## ローカル HLS を保存する

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

## シグナリングを付けて package-hls

`DRM_KID`、`DRM_KEY`、`DRM_IV` は秘密管理の手順から環境へ渡しておきます。どれも 16 バイト、32 桁の十六進数です。鍵値を含むシェルコマンドを履歴に残さないでください。この環境変数方式は汎用 `convert` の鍵設定と別です。

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

`verify-snapshot` の既定は記録の内容ダイジェストを確認し、`--files` は保存されたプレイリストとセグメントも再ハッシュします。原本と変換結果の全映像・音声・時刻を比較する `verify OUTPUT --source INPUT` の代わりではありません。終了値は 0 が一致、1 が変更、2 が不正なスナップショットです。

参照（英語）: [保存 HLS の使い方](https://github.com/kazuhide/drmr/blob/main/docs/usage.md)。

# 11. 準備済み DASH とバッチ処理

## run 用の入力レイアウト

`run` は、元の URL と対応するダウンロード済みの配置を使います。任意の MPD を単独で置けば使えるというインターフェースではありません。

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
  --hls-master sample/download/manifest.example.test/sample/hls/manifest.m3u8 \
  --output output --asset-id sample --log-format jsonl
```

結果は `output/sample/` の下です。`--packager-bin` は実行ファイルまたは packager のあるディレクトリを指定します。`--no-round-trip`、`--no-manifests`、`--limit-renditions` はデバッグ用途で、検査や出力を省きます。完全な処理結果と同じ意味では使わないでください。

## batch の作業リスト

`batch` は複数アセットを一つずつ実行し、再開ログを使います。汎用 `convert` のコマンド列を任意に投入する機能ではなく、準備済み DASH と clear-HLS の別ワークフローを対象にします。

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

`canonical` の出力自体には終端改行がないため、レコードを追加するごとに上の例のように改行も書きます。秘密情報の除去コマンドではなく、入力した JSON の値を出力します。鍵ファイルや認証情報をログへ表示する目的で使わないでください。

## 鍵と再開

既定の鍵提供元は環境変数です。アセットごとの保護された JSONL を使う場合は次の形式を指定します。鍵ファイルにはスキーマヘッダーはなく、各行が `asset_id`、`kid`、`key`、`iv` だけを持ちます。汎用 `--keys-file` の形式とは違います。

```sh
drmr batch --work-list work-list.jsonl --output output \
  --resume-log resume.jsonl --storage local --storage-root ./catalog \
  --key-provider file --key-file ./private/keys.jsonl
```

鍵ファイルを所有者専用にし、出力ツリーの外に置きます。HLS 向けの設定群には `--hls-config-dir` を使います。アセットごとの設定と入力は、対応するワークフローの条件を満たす必要があります。

`--retry-failed` は失敗した項目の再試行、`--max-attempts` は一アセットの試行上限で既定 3 です。`--per-asset-timeout-s` は既定 3600 秒、`--heartbeat-s` は既定 60 秒です。再開は副作用のロールバックではありません。ログと既存出力を確認してから再処理してください。

`--storage s3` も実装されていますが、この版の対応 region は `ap-northeast-1` に限られ、bucket の場所を確認します。認証は AWS の環境・標準プロファイルを使います。`--bucket`、`--region`、`--output-prefix` はストレージ設定です。このガイドのネイティブメディア検証表は、実 S3 アカウントでの実行認定を含みません。

参照（英語）: [準備済み入力と batch](https://github.com/kazuhide/drmr/blob/main/docs/usage.md)、[batch の CLI 定義](https://github.com/kazuhide/drmr/blob/main/crates/drmr-core/src/batch_cli.rs)。

# 12. 並列処理・メモリ・停止

## --max-workers は上限

`convert` と `verify` は、利用可能 CPU、メモリ、メディアの寸法・リソースサイズ・トラック数から並列度を選びます。指定しなくても自動調整します。手動指定は 1 から 64 の上限で、必要ならそれより少ないワーカーを使います。既定の推奨数は最大 32 です。

```sh
drmr convert source.mpd --to cbcs --format dash \
  --output result --keys-file config/keys.json --max-workers 2
drmr verify result/manifest.mpd --source source.mpd \
  --keys-file config/keys.json --max-workers 1 --json --measure
```

ワーカー数を変えても、内容・時刻の検証は省略しません。トラック、セグメント、原本プローブのプールは同じプロセス内で外部処理の予約容量を共有します。`batch` 自体は一アセットずつです。別々の CLI プロセス間では予約を共有しません。

## 資源見積もりの範囲

読める Linux の CPU quota とメモリ cgroup 上限、親 cgroup の上限も見積もりに使います。cgroup はコンテナやサービスの OS 資源制御です。資源を読めない場合は、512 MiB の枠と一つの子処理を仮定します。一つの処理も見積もり内に入らなければ `resource-budget` で失敗します。

メモリがわかる場合は、その四分の一か最低 256 MiB を予約し、トラックごとの保持分も見積もります。子処理には基礎分、画素数、入力作業量、出力捕捉上限を加味します。これは受け入れの推定で、OS によるメモリ制限や最大使用量の保証ではありません。

FFmpeg のデコーダー・エンコーダー・フィルターと FFprobe のデコードスレッドを一つに設定します。補助スレッドまで一つになる意味ではありません。音声ハッシュとパケット時刻の証明は逐次処理しますが、トラック・セグメントのメタデータは保持されるため、総メモリが常に一定になるわけではありません。

捕捉する標準出力は既定 16 MiB、標準エラーは 1 MiB です。超過は `output-limit` などで失敗し、切り詰めたデータを検証成功には使いません。待機中も時間予算と停止要求を確認します。

## Ctrl+C と終了確認

macOS/Linux の汎用 `convert`・`verify` は SIGINT、SIGTERM、Windows は Ctrl+C と Ctrl+Break で協調停止します。予約待ちと新規処理を止め、所有する子プロセスを終了し、後片付けを確認します。公開前に停止要求を観測した場合は結果を配置しません。

終了値 130 は、停止とプロセス終了確認が完了したことを示します。`cleanup-unconfirmed` や `media-cleanup-unconfirmed` は、停止要求があっても失敗です。強制 kill や OS の強制終了は協調停止と別で、一時領域を残し得ます。

Unix は所有するプロセスグループ、Windows は job object で子孫を管理します。終了要求を送ったことだけでは、子孫がファイルを閉じたことになりません。Unix の直接の子終了後のグループ確認、Windows の job 終了通知待ちには、それぞれ別の 30 秒の予算があります。ファイル I/O、名前解決、OS のスケジューリングを含めた厳密な全体終了時刻の保証ではありません。

## 計測値を読む

`--measure` は `--json` と併用します。メディア検証の条件は変えず、数値を追加します。フェーズは重なったり入れ子になったりするため、フェーズ時間を単純合計して全体時間とみなさないでください。

短い十秒アセットの時間は、起動処理やキャッシュの影響を受けます。メモリのサンプリング値は真の最大値ではありません。比較する場合は、原本・ツール版・ビルド設定・ワーカー上限・ホスト負荷を揃え、まず同じ完全検証が通ることを確認してください。

参照（英語）: [資源制限と測定方法](https://github.com/kazuhide/drmr/blob/main/docs/resources.md)。

# 13. Docker で依存ツールごと実行する

## イメージを作る

Docker イメージは CLI、FFmpeg/ffprobe、Bento4 の mp4dump、Shaka Packager を含みます。ビルドには Git と BuildKit が使える Docker が必要です。ネイティブ Linux amd64 または arm64 が対象で、Docker Desktop は Linux コンテナを選びます。

コミット済みのリポジトリルートから実行します。

```sh
revision=$(git rev-parse HEAD)
tree=$(git rev-parse "${revision}^{tree}")
git archive --format=tar "$revision" | docker build \
  --build-arg DRMR_REVISION="$revision" \
  --build-arg DRMR_TREE="$tree" --tag drmr:local -
docker run --rm --init drmr:local doctor --probe --route all
```

捕捉したコミットのファイルとロックファイルを使います。未コミットの変更は含みません。鍵、入力メディア、ダウンロード、結果はリポジトリ外に置き、実行時にマウントします。最初のビルドは同梱ツールもコンパイルするため時間とディスクを使い、Docker はキャッシュを残します。

イメージのコンパイラーやツールの固定版は、そのイメージを再現するための指定です。ソースビルドの最低要件 Rust 1.95 以上とは別です。

## 読み取り専用の入力と設定、書き込み用出力

ホストに `media`、`config`、`output` を用意します。設定と出力を所有者専用ディレクトリにし、鍵 JSON を 600 にします。以下は clear/DASH など、対応する原本と有効な鍵設定を用意済みの例です。

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

`--init` は停止シグナルの転送と終了した子の回収を助けます。drmr 自身の停止・所有権確認は引き続き行います。接続した CLI は Ctrl+C、名前付きコンテナは `docker stop --timeout 60 NAME` で協調終了の時間を与えます。強制終了へ移った場合と、drmr が終了確認を完了した場合を区別してください。

`--rm` は終了したコンテナの一時ファイルシステムを消します。マウントした原本・設定・結果は残ります。利用が終わった自分のタグは、使用中のコンテナがなくなった後に削除できます。

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

Docker Desktop は追加の host networking 機能が必要です。コンテナ内 loopback だけに待ち受けるサーバーは、通常のポート公開だけでは届きません。既存の loopback・Host・Origin 保護を解除して解決しないでください。ネイティブ Desktop GUI 自体はこのイメージに含まれません。

Windows は Docker Desktop の WSL 2 連携を使い、Linux シェルで実行します。秘密設定は Linux 側のファイルシステムに置いてください。Windows ドライブのマウントでは権限の意味が異なるため、ネイティブ Windows の ACL ワークフローと混同しません。

イメージ内の `/usr/share/drmr/build-manifest.json` は元コミット、ツリー、実際の依存版を記録します。ライセンスは `/usr/share/drmr/licenses`、対応するメディアツール等のソースは `/usr/share/drmr/sources` にあります。Debian パッケージ全体のソースミラーではありません。

参照（英語）: [Docker の使い方と確認結果](https://github.com/kazuhide/drmr/blob/main/docs/docker.md)。

# 14. Windows のネイティブ CLI

## 実行環境

Windows x64 の通常ユーザーとして、ローカル NTFS 上で使います。管理者昇格により新規ファイルの所有者が Administrators になる実行、ネットワーク共有、私有パス内の directory junction、alternate data stream、不適切な既存作業権限は拒否します。

CLI アーカイブを展開してユーザーの PATH に追加するか、完全パスで実行します。外部ツールは別途必要です。CI ビルドの CLI は VCRUNTIME140.dll を使うため、必要に応じて Microsoft Visual C++ x64 Redistributable をインストールしてください。

ソースビルドの場合:

```powershell
cargo build --release --locked -p drmr
.\target\release\drmr.exe --help
.\target\release\drmr.exe doctor --probe --route all
```

## 所有者専用の鍵ファイル

Windows は chmod の代わりに access control list、ACL を使います。所有者を現在のユーザーとし、継承を止め、現在のユーザーだけに完全アクセスを与える保護 ACL が必要です。次は新しい私有ディレクトリと空ファイルを作り、その後でエディターへ入力する例です。

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

エディターがファイルを置き換えて ACL を変更した場合は、私有ディレクトリ内のまま所有者専用 ACL を再適用してください。不正な既存ファイルの権限を drmr が自動で直すことはありません。

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

Ctrl+C と Ctrl+Break は協調停止に対応します。子孫の終了・ファイルロックの解放を確認してから終了値 130 を返します。終了確認失敗は成功した停止ではありません。長いパスを含め、外部ツールやファイルシステムの任意の構成での成功を保証するものではありません。

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

サーバーとジョブ CLI は同じ版を使います。`--cli` で差し替える場合は、その実行ファイルを直接指定します。別の CLI を起動するラッパーは、監督用の起動契約に適合せず拒否される場合があります。

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

この例は画面の default プロファイルへ対応します。入力 URL の origin も許可集合に入れる必要があります。Web からの取得はローカルファイルを許可せず、ポリシー中のローカルルートも除外します。カタログのローカル DASH 処理や、CLI analyze-hls の明示的なローカル入力許可とは別です。

## ジョブ状態と Stop

画面の JSON リクエストには、ヘッダー・本文を含む 15 秒の応答予算があります。これは画面の回復用で、メディア処理の時間制限ではありません。開始応答が失われても、自動で開始を再送しません。記録済みジョブを照合し、判定できなければ不明状態として別の開始を止めます。Check job status で状態だけを再照会します。

実行中の Stop は、そのジョブと子プロセスの終了を要求します。応答だけで後片付け完了と判断せず、最終ジョブ状態とログを確認してください。通常の停止後は次の待機ジョブへ進みます。サーバー終了中は新しい待機ジョブを開始しません。

Web の準備済みワークフローは、汎用 `convert` の新規出力公開方式と異なり、途中出力や以前の結果を変更し得ます。Stop は取り消しやロールバックではありません。終了コード 0、出力ファイルの存在、カタログの古い `converted` フラグだけで再生準備完了とみなさず、該当世代の `output_readiness` を確認します。

子プロセス終了が未確認なら、ジョブの実行枠と一時設定の所有を保持します。状態・ログ・停止の診断用機能は利用でき、新規処理は拒否されます。原因を確認し、サーバーの正常終了を再試行してください。強制終了はこの確認を省略します。

## 保存物と秘密情報

clear HLS を追加すると、URL、原本プレイリスト、取得メディアは `OUTPUT_ROOT/<asset-id>/clear/source/` に残ります。履歴から削除しても、保存原本、結果、ログをディスクから消すわけではありません。

鍵入力欄は既定で視覚的にマスクし、明示的に表示できます。送信後は入力値を消し、アプリ自身の localStorage/sessionStorage へ鍵を保存しません。ただし、ブラウザーのメモリ、開発者ツール、ページや通信の観察から鍵を隔離する機能ではありません。

ローカル ClearKey 再生では、内容鍵を JWK という JSON 鍵応答またはプレーヤー設定としてブラウザーへ意図して渡します。no-store でも開発者ツール、HAR、拡張機能などから読めます。運用 DRM のライセンスサービスやハードウェア保護再生ではありません。

HLS パッケージ後のプレビュー用認証コンテキストは、無操作 30 分までメモリに残る場合があります。30 秒ごとの掃除と関連アクセスで期限を確認します。フォーム消去は即時失効を意味しません。結果の最後の再生セッションの失効、またはサーバー終了で所有する関連情報を破棄します。

再生設定は、前のプレーヤー破棄、鍵応答、メディア読み込み、実際の playing イベントまで 20 秒の予算を持ちます。タイムアウトは画面を回復させるもので、変換結果の検証を書き換えません。前プレーヤーの破棄が確認できない場合は、重複して作らずページを再読込してください。

参照（英語）: [Web の操作と所有権](https://github.com/kazuhide/drmr/blob/main/docs/usage.md)、[ローカル再生の鍵境界](https://github.com/kazuhide/drmr/blob/main/docs/security.md)。

# 16. Desktop の使い方と検証範囲

## フォルダーを選んで開く

実験的 Desktop ホストは、同じローカル Web コンソールをネイティブウィンドウで開きます。カタログ、ジョブ、外部メディアツールは Web と共通です。

1. File → Choose catalog folder でカタログを選ぶ。
2. File → Choose output location で出力の置き場所を選ぶ。その下に私有の drmr-output を作成または開く。
3. File → Check external tools… で、実際の起動環境の外部ツールを確認する。
4. File → Open console で操作画面を開く。

開いた後にフォルダーを変える場合は、アプリを終了して再起動します。ウィンドウは所有する loopback のポートに移動先を限定し、別 origin や新規ウィンドウへ移動しません。レポートは同じウィンドウで開き、Open console で戻れます。Web 画面へネイティブのシェル・ファイルシステム権限を渡しません。

Quit drmr は受付を止め、サーバーと子プロセスの終了確認を待ちます。未確認ならウィンドウと所有権を保持してエラーを表示します。原因を直し、Quit drmr を再度選んでください。OS の強制終了とは異なります。

## 起動環境とバンドル

Finder などから起動した PATH は、ターミナルと違う場合があります。アプリはシェルの起動ファイルを実行したり、推測したインストール先を追加したりしません。必要なら、外部ツールが見えるターミナルからバンドルを起動します。

```sh
"/path/to/drmr.app/Contents/MacOS/drmr" doctor
"/path/to/drmr.app/Contents/MacOS/drmr-desktop"
```

Check external tools は、実際のアプリ環境で doctor --probe と同じバージョン・機能検査を行います。メディア適合性は別です。診断プロセスの終了が未確認なら、アプリを正常終了したりコンソールを開いたりせず所有を保持します。

CLI とホストは同じバンドル内に保持します。ホストは CLI の SHA-256 を確認します。外部ツール、鍵、ダウンロード、運用設定はバンドルに含みません。build-manifest.json はソース・ツリー・バイナリハッシュなどの記録で、署名付きの証明ではありません。

## ソースから作る場合

Tauri の OS 別前提条件と外部ツールを先に用意します。Windows は WebView2、macOS は WKWebView、Linux は WebKitGTK を使います。

```sh
RUSTUP_TOOLCHAIN=1.95.0 python3 tools/build_desktop.py --bundle
```

上は macOS/Linux シェルの例です。Windows の PowerShell は環境変数の指定方法も異なります。

```powershell
$env:RUSTUP_TOOLCHAIN = '1.95.0'
python tools/build_desktop.py --bundle
```

`--release` は最適化ビルド、`CARGO_TARGET_DIR` は専用の出力先を選び、既定は target/desktop です。スクリプトは同じ版の CLI と通知文を準備し、未署名の macOS .app または Windows/Linux の可搬ディレクトリとアーカイブを作ります。既存バンドルは上書きしません。署名、公証、インストーラー作成はこの手順の対象外です。

## 動作確認と未確認部分

macOS ではネイティブのフォルダーメニュー、キーボード、レポート移動、カタログ変換、処理中 Stop、正常終了と待受解除の実行確認があります。Chrome のローカル ClearKey 試験は原本・結果とも最後まで到達しました。

WKWebView は英語・日本語の再生失敗と再試行の回復を確認していますが、再生完了の認定はありません。Windows/Linux のネイティブ GUI 実行確認も残っています。各 OS でバンドルがビルド・検査できたことと、ダイアログ・WebView・DRM 再生が動いたことは別です。Linux GUI の依存関係には公開されている保守上の制限もあります。

参照（英語）: [Desktop の詳細と確認範囲](https://github.com/kazuhide/drmr/blob/main/docs/desktop.md)、[依存関係のセキュリティ方針](https://github.com/kazuhide/drmr/blob/main/docs/dependency-security.md)。

# 17. 結果・終了コード・問題の切り分け

## JSON と進捗

機械処理には `convert --json`、`verify --json` を使います。正常な結果は一つのバージョン付き JSON オブジェクトです。`--measure` を付ける場合も `--json` が必要です。

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

有効な引数で処理が始まった後の運用エラーは、`schema_version: 1`、`command`、`status`、`error` を返します。status は failed または cancelled で、error は固定の code、安全な hint、最後に観測した活動名 last_activity を持ちます。比較を終えて不一致となった検証は、既存の checks と比較情報も残します。

動画のみの候補から有限の再生範囲を削除した比較では、符号化内容と復号画像が一致しても、次のように失敗します。この例の `packet_timing_equal: false` は再生範囲の不一致を示しています。hint と last_activity は実際の英語出力です。

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

引数の構文エラーは処理開始前なので、この JSON 契約の対象外です。標準エラーにヘルプ等を出し終了値 2 になります。標準出力自体が書けない場合も完全な JSON は保証できず、標準エラーの `result-output-unavailable` と終了値 1 を確認します。

JSON を使わない場合は、観測した活動・件数・経過時間を標準エラーへ行単位で出します。トラック作業単位の完了は変換全体の完了ではありません。フェーズは重なり、閉じた作業範囲が失敗している場合もあります。最後の活動名だけから原因を断定しないでください。

## 終了値

| 値 | 意味 |
| --- | --- |
| 0 | 選択したコマンドの確認・処理に成功。何を検証したかはコマンドによる |
| 1 | 処理、比較、必要な診断、終了確認などの失敗 |
| 2 | 引数・設定の不正。verify-snapshot では不正なスナップショット |
| 130 | 対応する CLI の協調停止が、所有プロセスの終了確認まで完了 |

`doctor` の 0 は選択した診断の成功、`inspect` の 0 はメタデータ読み取りの成功であり、完全メディア検証の成功ではありません。`verify-snapshot` の 1 は記録・保存内容の変更を示します。

## よくある失敗

| 症状・コード | 確認すること |
| --- | --- |
| 必須ツールが見つからない | 実際の起動環境の PATH、実行権限、Desktop の Check external tools を確認する |
| バージョン・機能検査が失敗 | 版だけでなくビルドのオプションを確認する。Bento4 の build は配布元でも確認する |
| 鍵設定を拒否 | 所有者・600 または ACL・リンク数・JSON の重複、不明フィールド、スキーマを確認する |
| `input-key-required` | 原本の各 KID または正確な HLS 鍵 URI に対応した入力鍵を設定する |
| `output-key-required` / `output-key-not-used` | 暗号化出力に必要な鍵があるか、clear 出力に不要な output が残っていないか確認する |
| `presentation-window-not-supported` | 編集情報、開始・終端、通常速度、正確なクロック換算などが対応条件か確認する。任意に原本を切らない |
| `progressive-cenc-profile-not-supported` / `progressive-cbcs-profile-not-supported` | 暗号配置、IV、補助情報、入力 PSSH などが限定プロファイル内か確認する |
| `unsupported-packet-timing` | FFprobe が全 PTS・DTS・duration を返すか、必要なツール版と実データを確認する。欠落値を推測して埋めない |
| `round-trip-verification-failed` | 原本と結果のメタデータ、符号化内容、全デコード、時刻のどこが一致しないか確認する |
| `resource-budget` | 観測できた CPU・メモリ、入力サイズと寸法を確認する。ワーカー上限を増やしても必ず解決しない |
| `media-output-limit` / `media-probe-output-limit` | 捕捉上限やレコード上限を超えていないか確認する。部分結果を成功扱いしない |
| `cleanup-unconfirmed` / `media-cleanup-unconfirmed` | 子プロセスやロックの状態を確認する。停止済み・成功済みと扱わず、所有を維持して回復する |

変換を繰り返すときは、新しい出力名を選びます。URL 取得が完成済みなら保存した input.m3u8 を再利用できますが、既存 `--download-dir` を再取得先にしません。私有領域を手動削除する前に、利用する子プロセスが終了したことを確認してください。

共有する情報は、コマンド名、固定コード、版、対象の対応プロファイル、秘密を除いた最小の再現条件を中心にします。レポートにマスクがあっても、原本 URL、パス、メディア名、出力シグナリングがすべて秘密除去されたわけではありません。

# 18. 試験結果の読み方と再現方法

## 短い公開アセットによる回帰試験

主な実メディア試験は、出典と権利表示のある Big Buck Bunny の十秒アセットです。H.264 の 240 映像パケットと AAC 音声を持ち、分割時は映像を二秒目標の五セグメントにします。音声は完全フレーム境界に従うため、すべてを厳密な二秒に揃える意味ではありません。

| 試験 | 対象と確認内容 |
| --- | --- |
| 従来の 16 保護経路 | clear/cenc/cbcs DASH と AES-128 fMP4 HLS の原本から四つの出力を作る。原本・結果の鍵を分け、変換と別の verify を行う |
| TS→DASH の 27 経路 | clear/AES-128/SAMPLE-AES 入力 × 映像のみ・映像音声・音声のみ × clear/cenc/cbcs DASH。原本の全データ・時刻と最終宣言を確認 |
| TS 出力の 27 経路 | 同じ三入力保護・三トラック構成から clear/AES-128/SAMPLE-AES TS を作り、完全比較する |
| 有限音声範囲の 54 経路 | clear/cenc/cbcs MP4 × 音声のみ・映像音声 × MP4/DASH/fMP4 HLS の九出力。全音声と各自の正確な再生範囲を確認 |
| 有限 MP4 の CLI 27 経路 | 三入力保護の映像音声 MP4 から九出力を作り、実 CLI で独立検証する |
| URL 入力 | clear TS/fMP4 HLS を取得し CBCS HLS へ変換。配信元停止後も保存原本で再実行する |

CLI の 27 経路は、54 種類のうち映像と音声を持つ組み合わせを、実際のコマンドでもう一度確認する試験です。

暗号化 MP4 の公開試験 fixture は映像・音声で異なる KID と鍵を使います。GPAC は一部の fixture を事前生成するためだけに使用し、CLI の実行依存や CI の試験依存にはしません。

時刻・終端を一サンプル変える、鍵を欠落・交換する、ペイロード・再生フラグ・表示寸法・暗号補助情報・マニフェストを改変する、といった拒否試験もあります。正常系だけでなく、変更した結果を拒否できることが受け入れ条件です。

## OS と検証範囲

最低 Rust 版 1.95 と最新安定版を Linux、macOS、Windows の CI で検査します。ネイティブ Windows は通常ユーザーで権限、Ctrl+C/Ctrl+Break、子孫・ロック・作業領域を検査します。Docker は Linux AMD64 で実メディアと Web の HTTP・停止・終了を確認します。Desktop の各 OS ビルドはネイティブ GUI 再生試験とは別です。

## 実行結果の確認方法

CI のログと成果物には、実行したソースの識別値、コンパイラーと外部ツールの版、検証結果が記録されます。利用する版の実行結果を確認してください。このガイドの記述だけから、後の版や任意の入力・機器の動作を認定するものではありません。

有限音声の 54 種類は、音声のみ 27 種類と映像音声 27 種類です。実 CLI の 27 経路は後者の再確認で、追加の独立したプロファイルではありません。従来の 16 経路は事前にフラグメント化した原本を基準にし、有限音声の試験は元のプログレッシブ MP4 自身の再生範囲を基準にします。原本やデコード方法が異なる試験のサンプル数を混同しません。

Bento4 の build 番号など、設定した配布物の版と実行バナーから観測できる情報は区別してください。Desktop のビルド成功は GUI・再生の認定ではなく、Docker の HTTP 試験もブラウザー画面の認定ではありません。既知の鍵による完全メディア比較は、DRM ライセンス取得や実機再生の証明ではありません。

## 代表的な再現コマンド

開発用ソースとメディアツールを揃えた環境で、リポジトリのルートから実行します。

```sh
cargo test --locked -p drmr --test bbb_media -- --nocapture
cargo test --locked -p drmr-core --test conversion_ts_output -- --nocapture
cargo test --locked -p drmr-core --test conversion_ts_output audio_hls -- --nocapture
cargo test --locked -p drmr --test conversion_ts_dash -- --nocapture
cargo test --locked -p drmr-core --test conversion_finite_outputs -- --nocapture
cargo test --locked -p drmr --test conversion_finite_outputs -- --nocapture
```

16 経路を確認する bbb_media の回帰試験では、`DRMR_CONVERSION_REPORT` をリポジトリ外の新しいファイルへ設定すると JSON を保存できます。実際の fixture ハッシュ、経路、全比較、時間、サンプリングしたメモリ、ツール版、測定の限界を確認してください。他の TS・有限範囲試験がこの変数でレポートファイルを作るわけではありません。それぞれのテストログと CI 成果物を使います。

長時間のパケット証明には、32×18 の小さな派生映像を 417 回繰り返し、4,170 秒・100,080 パケットを変換・独立検証する別試験もあります。

```sh
cargo test --locked -p drmr --test long_conversion
```

これは長い時刻列を省略せず読めるかの回帰試験で、高解像度・長尺の処理速度や常時メモリ量を認定するベンチマークではありません。十秒の試験も同様に、万能な対応や DRM 実機再生の認定を意味しません。

参照（英語）: [アセットの出典・ライセンス](https://github.com/kazuhide/drmr/blob/main/crates/drmr/tests/fixtures/bbb/README.md)、[暗号化 fixture](https://github.com/kazuhide/drmr/blob/main/crates/drmr/tests/fixtures/bbb/CENC.md)、[処理と試験の詳細](https://github.com/kazuhide/drmr/blob/main/docs/media-processing.md)。

# 19. CLI オプション一覧

この章は公開 CLI の引数・オプションの索引です。全コマンドに `-h` / `--help` があり、`drmr help COMMAND` でも確認できます。全体の `-V` / `--version`、または `drmr version` で版を表示します。内部の監督プロセス用起動引数は、利用者向けインターフェースに含めません。

## doctor

| オプション | 意味・既定値 |
| --- | --- |
| `--json` | 一つの機械可読 JSON レポート |
| `--probe` | 三秒予算のバージョン・宣伝オプション検査。既定は探索のみ |
| `--route PROFILE` | 必要なツールセット。既定 all |

PROFILE は `all`、`prepared-dash`、`progressive-mp4`、`fragmented-mp4`、`dash-presentation`、`fmp4-hls-presentation`、`mpeg-ts-hls`、`mpeg-ts-sample-aes-hls`、`mpeg-ts-hls-ts` です。変換結果の処理経路名が、そのまま doctor の受理値とは限りません。TS→DASH を含め一式を診断する場合は all を使えます。

## inspect

| 引数・オプション | 意味 |
| --- | --- |
| `INPUT` | ローカル MP4、MPD、M3U8。リモート参照は取得しない |
| `--json` | バージョン付きメタデータレポート |

## plan

| 引数・オプション | 意味 |
| --- | --- |
| `INPUT` | 汎用の変換意図を調べるローカル入力 |
| `--to TARGET` | INPUT モードで必須。clear、cenc、cbcs、hls-aes128、hls-sample-aes |
| `--format FORMAT` | mp4、dash、hls、hls-ts。計画ではカンマ区切り複数も可。省略時は MP4/DASH/HLS に従い、HLS は fMP4 |
| `--json` | 実行せず、バージョン付き計画を JSON で表示 |
| `--mpd MPD` | 準備済み DASH のレンディションを列挙する別モード |
| `--asset-id ID` | --mpd モードのアセット名を指定 |

## convert

| 引数・オプション | 意味・既定値 |
| --- | --- |
| `INPUT` | 対応するローカル原本、または完了 clear HLS URL |
| `--to TARGET` | 必須。出力保護方式 |
| `--format FORMAT` | 一つの出力形式。省略時は MP4/DASH/HLS に従い、HLS は TS 入力でも fMP4 |
| `--output NEW_PATH` | 必須。新規ファイルまたは新規ディレクトリ |
| `--keys-file FILE` | 入力と出力の役割を分けた所有者専用 JSON |
| `--download-dir NEW_DIR` | URL 入力に必須。取得原本を残す新規ディレクトリ |
| `--fetch-policy FILE` | URL の追加 origin、認証、資源制限の私有 JSON |
| `--packager-bin FILE` | Shaka 実行ファイル。既定 packager |
| `--max-workers N` | 同時メディア作業の上限 1-64。既定は CPU・メモリから自動選択 |
| `--json` | 一つのバージョン付き結果。通常進捗行を抑制 |
| `--measure` | 数値の計測情報を追加。--json が必要 |

汎用 CLI に ffmpeg-bin/ffprobe-bin の独自フラグはありません。FFmpeg/ffprobe は起動環境の PATH を用います。

## verify

| 引数・オプション | 意味・既定値 |
| --- | --- |
| `OUTPUT` | 比較する結果の MP4、MPD、M3U8 |
| `--source INPUT` | 必須。元の入力。結果から原本を推測しない |
| `--keys-file FILE` | input が原本、output が結果の鍵 |
| `--packager-bin FILE` | Shaka 実行ファイル。既定 packager |
| `--max-workers N` | 上限 1-64。受け入れ時に減る場合がある |
| `--json` | 一つの独立検証レポート |
| `--measure` | 計測を追加。--json が必要 |

verify は target や format を指定して作り直すコマンドではありません。

## run

| オプション | 意味・既定値 |
| --- | --- |
| `--mpd MPD` | 必須。対応する準備済み DASH マニフェスト |
| `--output ROOT` | 必須。ROOT/asset-id の下に出力 |
| `--asset-id ID` | アセット名。既定は原本ディレクトリ名 |
| `--hls-master M3U8` | 原本 HLS master。--no-manifests を使わなければ必要 |
| `--packager-bin PATH` | packager 実行ファイル、またはそれを含むディレクトリ |
| `--max-workers N` | レンディション並列度の上限 |
| `--manifest-local-url-base URL` | ローカル HTTP 配信用ベース URL のマニフェストも生成 |
| `--log-format FORMAT` | text または jsonl。読みやすい概要または一行一 JSON。既定 text |
| `--no-round-trip` | デバッグ用。往復のデコード照合を省く |
| `--limit-renditions N` | デバッグ用。一部のレンディションだけ処理する |
| `--no-manifests` | デバッグ用。プレイリスト・マニフェストを生成しない |

## analyze-hls / package-hls

| analyze-hls のオプション | 意味 |
| --- | --- |
| `--source-config FILE` | 必須。入力 URL/パスとアクセス許可を持つ私有 JSON |
| `--asset-id ID` | 必須。保存アセット名 |
| `--output ROOT` | 必須。ROOT/ID/clear/source の下に保存 |

| package-hls のオプション | 意味 |
| --- | --- |
| `--analysis FILE` | 必須。完成した analysis.json |
| `--drm-config FILE` | 必須。Widevine・PlayReady・FairPlay シグナリングの私有 JSON |
| `--selection FILE` | 解析された音声トラックを選ぶ私有 JSON |
| `--packager-bin FILE` | Shaka 実行ファイル。既定 packager |
| `--max-workers N` | 全トラック間のセグメント処理上限 1-64 |

これらの暗号鍵は、DRM_KID/DRM_KEY/DRM_IV から受け取ります。汎用鍵 JSON を drm-config へ渡す方式ではありません。

## verify-snapshot / canonical / version

| コマンドと引数 | 意味 |
| --- | --- |
| `verify-snapshot PATH` | スナップショットディレクトリまたは analysis.json の記録を確認 |
| `verify-snapshot --files PATH` | 保存プレイリスト・メディアも再ハッシュ |
| `canonical PATH` | 任意 JSON を安定したキー順で表示。終端改行と秘密除去はない |
| `version` | 版を表示。個別オプションなし |

## batch

| オプション | 意味・既定値 |
| --- | --- |
| `--work-list FILE` | 必須。schema ヘッダーとアセット行を持つ JSONL |
| `--output ROOT` | 必須。出力ルート |
| `--resume-log FILE` | 必須。再開状態の記録先 |
| `--storage STORAGE` | local または s3。既定 local |
| `--storage-root DIR` | ローカルオブジェクトキーの基点 |
| `--bucket NAME` | S3 bucket |
| `--region REGION` | 既定 ap-northeast-1。この版の S3 対応 region も同じ |
| `--output-prefix PREFIX` | ストレージの結果 prefix。既定 output |
| `--key-provider PROVIDER` | env または file。既定 env |
| `--key-file FILE` | file 方式の所有者専用アセット別 JSONL |
| `--hls-config-dir DIR` | HLS ワークフロー用設定群 |
| `--packager-bin PATH` | packager の指定 |
| `--max-workers N` | 各アセット内の処理上限。アセット自体は逐次 |
| `--retry-failed` | 失敗項目の再試行を選択 |
| `--plan` | 実処理せず予定を表示 |
| `--max-attempts N` | 一アセットの試行上限。既定 3 |
| `--per-asset-timeout-s SEC` | 一アセットの時間予算。既定 3600 秒 |
| `--heartbeat-s SEC` | 状態通知の間隔。既定 60 秒 |

## serve

| オプション・環境 | 意味・既定値 |
| --- | --- |
| `--static-dir DIR` | 組み込み画面の代わりに配信する信頼済み UI ディレクトリ |
| `--cli FILE` | ジョブを実行する同じ版の CLI |
| `DRMR_CATALOG_ROOT` | 必須。準備済みメディアのカタログルート |
| `DRMR_OUTPUT_ROOT` | 必須。結果・保存 HLS・ログのルート |
| `DRMR_BIND_HOST` | 既定は loopback。リモート公開を想定しない |
| `DRMR_BIND_PORT` | 既定 8080 |
| `DRMR_MAX_WORKERS` | HLS パッケージの並列度上限 |
| `DRMR_HLS_PROFILES` | HLS 入力取得の接続許可・認証ヘッダー等を持つ、所有者専用ポリシー JSON ファイルのパス |

# 20. 資料とこのガイドの更新

## 版に対応した公開資料

以下はこのガイド の対象コミットへ固定した参照です。利用中の版が異なる場合は、対応するコミットまたは同梱ドキュメントを確認してください。

- [README・導入](https://github.com/kazuhide/drmr/blob/main/README.md)
- [使い方と設定](https://github.com/kazuhide/drmr/blob/main/docs/usage.md)
- [入出力対応表](https://github.com/kazuhide/drmr/blob/main/docs/conversion-support.md)
- [処理・時刻・完全メディア検証](https://github.com/kazuhide/drmr/blob/main/docs/media-processing.md)
- [処理経路と解析・計画の境界](https://github.com/kazuhide/drmr/blob/main/docs/processing-routes.md)
- [必要版と依存関係](https://github.com/kazuhide/drmr/blob/main/docs/requirements.md)
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

ホームページにも入出力表があります。サイトとガイドは公開 main を基準にします。保存した PDF は、その表紙の対象版も確認してください。

このガイドは、現在の実装・ヘルプ・公開ドキュメント・実行検証の境界を利用者向けにまとめたものです。本文中のパスや example ドメイン、山括弧の鍵値は説明用です。自分のメディアと権限・設定へ置き換えてから実行してください。
