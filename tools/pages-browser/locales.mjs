// Fixed public locales; no runtime language detection, cookies or redirects.
export const locales = [
  {
    language: 'en', route: '', asset: 'index.html', skip: 'Skip to content',
    navigation: 'Main navigation', footer: 'Documentation and source', languageNavigation: 'Language',
    switchName: '日本語', switchLanguage: 'ja', switchHref: './ja/',
    headings: ['Input and output support', 'Analyze, plan, convert, verify', 'Four ways back to the original', 'Start with the CLI'],
    groups: ['Analyze command', 'Plan command', 'Convert command', 'Verify command', 'Build and check the CLI'],
    anchors: ['Support', 'Workflow', 'Get started'],
    matrixRegion: 'Conversion support matrix', matrixCaption: 'Supported conversion profiles',
    matrixCorner: 'Input ↓ / Output →', outside: 'Outside profile',
    profiles: ['Progressive or fragmented', 'Fragmented MP4 tracks', 'Fragmented MP4 (fMP4)', 'Video with optional audio', 'Audio only'],
    matrix: 'Read the full support matrix and restrictions', license: 'Font license',
    releaseStatus: 'release compatibility',
  },
  {
    language: 'ja', route: 'ja/', asset: 'ja/index.html', skip: '本文へ移動',
    navigation: 'メインナビゲーション', footer: 'ドキュメントとソース', languageNavigation: '言語',
    switchName: 'English', switchLanguage: 'en', switchHref: '../',
    headings: ['入力と出力の対応表', '解析・計画・変換・検証', '原本との比較項目', 'CLI のインストール'],
    groups: ['入力を解析するコマンド', '変換を計画するコマンド', '変換するコマンド', '原本と比較するコマンド', 'CLIのビルドとツール確認'],
    anchors: ['対応範囲', '使い方', '始める'],
    matrixRegion: '変換対応表', matrixCaption: '対応する変換プロファイル',
    matrixCorner: '入力 ↓ / 出力 →', outside: '対象外',
    profiles: ['プログレッシブ / フラグメント化', 'フラグメント化MP4トラック', 'フラグメント化MP4（fMP4）', '映像（音声は省略可）', '音声のみ'],
    matrix: '対応範囲と制約を読む（英語）', license: 'フォントライセンス（英語）',
    releaseStatus: '配布版との違い（英語）',
  },
];
export const english = locales[0];
