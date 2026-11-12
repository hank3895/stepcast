# StepCast

Portable, offline Windows screen recorder that turns your actions into small GIFs, with click/key highlights, zoom and annotations. Built with Rust + Tauri.

可攜、離線的 Windows 螢幕錄製工具：把操作錄成小巧的 GIF，自動標示點擊與按鍵，可拉近畫面、加標注。

> 目前階段：**第一版介面**。可錄製主螢幕並輸出 GIF；縮放、標注、框選範圍等功能開發中。

## 下載與使用

不需要安裝。每次推送後 GitHub Actions 會自動編譯：

1. 打開本 repo 的 **Actions** 頁 → 最新一次 `build` → 下方 **Artifacts** 下載 `StepCast-windows`
2. 解壓縮，雙擊 `StepCast.exe`
3. 選好影格率、輸出寬度，按「開始錄製主螢幕」（或按 **F9**）
4. 倒數 3 秒後開始錄製，視窗自動縮到工作列；再按 **F9** 停止
5. 完成後會顯示預覽、檔案大小，並可「在資料夾中顯示」

GIF 預設存在 `StepCast.exe` 旁的 `recordings` 資料夾；無法寫入時改存到「圖片\StepCast」。設定可在畫面上修改。

需求：Windows 10 1903 以上，以及 WebView2（Windows 11 內建）。

### 命令列版（驗證用）

```
stepcast-poc.exe record --seconds 10 --fps 12 --width 960 --out test.gif
stepcast-poc.exe selftest
```

## 專案結構

| 資料夾 | 內容 |
| --- | --- |
| `core/` | 截圖（Windows Graphics Capture）與 GIF 編碼，介面與命令列共用 |
| `app/` | Tauri 2 + Svelte 介面 |
| `poc/` | 命令列驗證程式 |

## 怎麼做到檔案小

- 畫面沒變的影格合併成一格，延長停留時間
- 每格只編碼有變化的矩形區域
- 每格各自用 [quantette](https://crates.io/crates/quantette) 產生 256 色調色盤

## 授權

MIT，見 [LICENSE](LICENSE)。所有相依套件皆為 MIT / Apache-2.0 / Zlib 等寬鬆授權，CI 以 cargo-deny 阻擋 GPL 類套件。
