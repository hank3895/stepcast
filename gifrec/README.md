# gifrec

輕量、可攜的螢幕錄影轉 GIF 工具，附簡易編輯器。用 Rust + egui 寫成，不需要 WebView2。

## 下載

不用安裝。每次推送 `gifrec/` 的變更後，GitHub Actions 會自動編譯：

1. 打開本 repo 的 **Actions** 頁 → 左側 **gifrec** → 最新一次執行
2. 下方 **Artifacts** 下載 `gifrec-windows`，解壓縮
3. 雙擊 `gifrec-gui.exe`（`gifrec.exe` 必須放在同一個資料夾）

## 錄影

- 錄影範圍：全螢幕，或拖曳框選區域（單擊 = 整個螢幕，Esc / 右鍵取消）
- 開始方式：倒數幾秒，或按熱鍵才開始
- 全域熱鍵（預設 **Ctrl+Shift+F9**）：待命時按一下開始，錄影中按一下停止
- 錄影時設定視窗會自動縮小，不會被錄進去
- 畫面完全沒變的影格會跳過，GIF 只記錄有變動的區域，檔案較小

## 編輯

錄完按「編輯這個 GIF」，或把任何 GIF 拖進視窗、拖到 `gifrec-gui.exe` 圖示上。

- **馬賽克**：拖曳移動、拉右下角縮放，可調格子大小
- **文字**：多行、字級、顏色、半透明黑底
- **圖片**：PNG / JPG（支援透明），維持比例縮放、可調不透明度；也可以直接拖進視窗
- 每個效果可設定出現的影格範圍、上下圖層，Delete 鍵刪除
- 播放預覽、逐格前後、頭尾裁剪
- 匯出成新檔，不會覆蓋原始檔

## 命令列

```
gifrec                              全螢幕（主螢幕）
gifrec --select --wait              框選後待命，按熱鍵開始
gifrec --region 100,200,800,600     指定區域 x,y,寬,高
gifrec --list                       列出螢幕
gifrec --help                       所有參數
```

## 自己編譯

```
cd gifrec
cargo build --release
```

## 授權

MIT。所有相依套件皆為寬鬆授權（以 `cargo deny check licenses` 檢查，設定見 `deny.toml`）。
