// Fixed public locales; no runtime language detection, cookies or redirects.
export const locales = [
  {
    language: 'en', route: '', asset: 'index.html', skip: 'Skip to content',
    navigation: 'Main navigation', footer: 'Documentation and source', languageNavigation: 'Language',
    switchName: '日本語', switchLanguage: 'ja', switchHref: './ja/',
    headings: ['From declarations to checked media', 'The container matters', 'Four ways back to the original', 'Start with the CLI'],
    groups: ['Inspect command', 'Plan command', 'Convert command', 'Verify command', 'Build and check the CLI'],
    anchors: ['Workflow', 'Support', 'Get started'],
    matrix: 'Read the full support matrix and restrictions', license: 'Font license',
  },
  {
    language: 'ja', route: 'ja/', asset: 'ja/index.html', skip: '本文へ移動',
    navigation: 'メインナビゲーション', footer: 'ドキュメントとソース', languageNavigation: '言語',
    switchName: 'English', switchLanguage: 'en', switchHref: '../',
    headings: ['記載内容を調べて、実データを確かめる', 'コンテナ形式で対応範囲が変わる', '原本と比べる4つの確認', 'CLIから始める'],
    groups: ['入力を調べるコマンド', '変換を計画するコマンド', '変換するコマンド', '原本と比較するコマンド', 'CLIのビルドとツール確認'],
    anchors: ['使い方', '対応範囲', '始める'],
    matrix: '対応範囲と制約を読む（英語）', license: 'フォントライセンス（英語）',
  },
];
export const english = locales[0];
