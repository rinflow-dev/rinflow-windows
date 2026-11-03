# Rinflow for Windows

[English](README.md) | [简体中文](README.zh-CN.md) | 日本語

Rinflow は、ローカルネットワーク上の複数のコンピューターで 1 組のキーボードとマウスを共有するソフトウェアです。ポインターを画面の端の外へ動かすと、隣のデバイスを操作できます。

これは Windows クライアントです。[macOS クライアント](https://github.com/rinflow-dev/rinflow-macos)と連携でき、[rinflow-core](https://github.com/rinflow-dev/rinflow-core) をベースにしています。

**状態：** 開発初期の段階です。バイナリのリリースはまだありません。既知の問題は[変更履歴](CHANGELOG.md)をご覧ください。

## 機能

- 画面の端を越えて他のデバイスを操作でき、他のデバイスから操作されることも可能
- デバイスをドラッグしてディスプレイの周囲に配置
- 暗号化接続と 6 桁のコードによるペアリング
- クリップボードとファイルの同期（任意、既定ではオフ）
- 管理者ウィンドウ、UAC ダイアログ、ロック画面への入力（任意）
- 英語、簡体字中国語、日本語のインターフェース

## ビルド

Windows 10 1809 以降、MSVC ツールチェーンの Rust、C++ と WinUI のワークロードを含む Visual Studio または Build Tools 2026 が必要です。詳しくは[ビルドと実行](docs/building.md)をご覧ください。

```powershell
git clone https://github.com/rinflow-dev/rinflow-core.git
git clone https://github.com/rinflow-dev/rinflow-windows.git
cd rinflow-windows
.\build.cmd -Configuration Release
.\installer\build-installer.ps1    # 任意：セットアップを作成（Inno Setup 6 が必要）
```

## 使い方

1. 同じネットワーク上の両方のデバイスで Rinflow を起動します。
2. **デバイスを追加** をクリックし、両方のデバイスに同じ 6 桁のコードが表示されていることを確認します。
3. デバイスを、実際に置かれている画面の側へドラッグします。
4. ポインターをその端の外へ押し出します。**Ctrl+Alt+Esc** で操作を戻せます。

## ドキュメント（英語）

- [ビルドと実行](docs/building.md)
- [設計](docs/design.md)：各機能の仕組み、設定、エージェントの IPC プロトコル
- [クリップボードの実装](agent/docs/clipboard.md)

## コントリビュート

[CONTRIBUTING.md](CONTRIBUTING.md) をご覧ください。脆弱性の報告については [SECURITY.md](SECURITY.md) をご覧ください。

## ライセンス

[Mozilla Public License 2.0](LICENSE)。サードパーティ製ソフトウェアは [THIRD_PARTY_NOTICES.txt](THIRD_PARTY_NOTICES.txt) に記載しています。
