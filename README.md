# StepCast

Portable, offline Windows screen recorder that turns your actions into small GIFs, with click/key highlights, zoom and annotations. Built with Rust + Tauri.

可攜、離線的 Windows 螢幕錄製工具：把操作錄成小巧的 GIF，自動標示點擊與按鍵，可拉近畫面、加標注。

> 目前階段：**第 1 階段・Rust 驗證程式**（`poc/`）。驗證 Rust 截圖＋輸出 GIF 的效能與檔案大小，正式的 Tauri 介面之後加入。

## 下載與試用

不需要安裝 Rust。每次推送後 GitHub Actions 會自動編譯：

1. 打開本 repo 的 **Actions** 頁 → 最新一次 `build` → 下方 **Artifacts** 下載 `stepcast-poc-windows`
2. 解壓縮後，在該資料夾開啟命令提示字元或 PowerShell：

```
stepcast-poc.exe record --seconds 10 --fps 12 --width 960 --out test.gif
```

執行後 3 秒開始錄製主螢幕，結束時會顯示實際 FPS、寫入格數、編碼時間與檔案大小。

| 參數 | 預設 | 說明 |
| --- | --- | --- |
| `--seconds` | 5 | 錄製秒數（1–600） |
| `--fps` | 12 | 影格率（1–60） |
| `--width` | 960 | 輸出寬度上限，高度等比例 |
| `--out` | stepcast-test.gif | 輸出檔名 |
| `--no-dither` | 關 | 關閉抖色；檔案通常更小，漸層會有色帶 |

`stepcast-poc.exe selftest` 會用合成畫面測試編碼器，不需要螢幕。

## 怎麼做到檔案小

- 畫面沒變的影格合併成一格，延長停留時間
- 每格只編碼有變化的矩形區域
- 每格各自用 [quantette](https://crates.io/crates/quantette) 產生 256 色調色盤

## 授權

MIT，見 [LICENSE](LICENSE)。所有相依套件皆為 MIT / Apache-2.0 / Zlib 等寬鬆授權，CI 以 cargo-deny 阻擋 GPL 類套件。
