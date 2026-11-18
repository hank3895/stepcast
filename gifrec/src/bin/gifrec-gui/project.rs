//! 專案檔（.gifrec，JSON）：記住來源 GIF 與所有編輯內容，之後可以回來繼續改。
//! 加入的圖片以 PNG 內嵌在檔案裡，搬到別台電腦也不會遺失。

use crate::render::{Doc, Item, Kind};
use base64::{engine::general_purpose::STANDARD as B64, Engine};
use image::RgbaImage;
use serde::{Deserialize, Serialize};
use std::{
    io::Cursor,
    path::{Path, PathBuf},
    sync::Arc,
};

pub const EXT: &str = "gifrec";

#[derive(Serialize, Deserialize)]
struct ProjectFile {
    app: String,
    version: u32,
    /// 來源 GIF：存相對於專案檔的路徑（同資料夾時就是檔名），另存絕對路徑備用
    source: String,
    source_abs: String,
    frames: usize,
    trim: [usize; 2],
    crop: [u32; 4],
    out_w: u32,
    items: Vec<ItemFile>,
}

#[derive(Serialize, Deserialize)]
struct ItemFile {
    #[serde(flatten)]
    kind: KindFile,
    x: f32,
    y: f32,
    w: f32,
    h: f32,
    start: usize,
    end: usize,
}

#[derive(Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "lowercase")]
enum KindFile {
    Mosaic { block: u32 },
    Blur { radius: u32 },
    Text { text: String, size: f32, color: [u8; 4], bg: bool },
    Image { name: String, png: String, opacity: f32 },
    Zoom,
}

pub struct Loaded {
    pub source: PathBuf,
    pub doc: Doc,
    pub frames: usize,
}

fn rel_path(target: &Path, base_dir: &Path) -> String {
    match target.strip_prefix(base_dir) {
        Ok(r) => r.to_string_lossy().replace('\\', "/"),
        Err(_) => target.to_string_lossy().into_owned(),
    }
}

pub fn save(path: &Path, source: &Path, doc: &Doc, frames: usize) -> Result<(), String> {
    let dir = path.parent().unwrap_or(Path::new("."));
    let source_abs = std::fs::canonicalize(source).unwrap_or_else(|_| source.to_path_buf());
    let dir_abs = std::fs::canonicalize(dir).unwrap_or_else(|_| dir.to_path_buf());
    let mut items = Vec::new();
    for it in &doc.items {
        let kind = match &it.kind {
            Kind::Mosaic { block } => KindFile::Mosaic { block: *block },
            Kind::Blur { radius } => KindFile::Blur { radius: *radius },
            Kind::Text { text, size, color, bg } => {
                KindFile::Text { text: text.clone(), size: *size, color: *color, bg: *bg }
            }
            Kind::Image { name, img, opacity } => {
                let mut png = Vec::new();
                img.write_to(&mut Cursor::new(&mut png), image::ImageFormat::Png)
                    .map_err(|e| format!("圖片編碼失敗：{e}"))?;
                KindFile::Image { name: name.clone(), png: B64.encode(png), opacity: *opacity }
            }
            Kind::Zoom => KindFile::Zoom,
        };
        items.push(ItemFile { kind, x: it.x, y: it.y, w: it.w, h: it.h, start: it.start, end: it.end });
    }
    let pf = ProjectFile {
        app: "gifrec".into(),
        version: 1,
        source: rel_path(&source_abs, &dir_abs),
        source_abs: source_abs.to_string_lossy().into_owned(),
        frames,
        trim: [doc.trim.0, doc.trim.1],
        crop: doc.crop,
        out_w: doc.out_w,
        items,
    };
    let json = serde_json::to_string_pretty(&pf).map_err(|e| e.to_string())?;
    // 先寫暫存檔再改名，存到一半當掉也不會弄壞原本的專案
    let tmp = path.with_extension("gifrec.tmp");
    std::fs::write(&tmp, json).map_err(|e| format!("無法寫入專案檔：{e}"))?;
    std::fs::rename(&tmp, path).map_err(|e| format!("無法寫入專案檔：{e}"))?;
    Ok(())
}

pub fn load(path: &Path) -> Result<Loaded, String> {
    let text = std::fs::read_to_string(path).map_err(|e| format!("無法開啟專案檔：{e}"))?;
    let pf: ProjectFile = serde_json::from_str(&text).map_err(|e| format!("專案檔格式錯誤：{e}"))?;
    if pf.app != "gifrec" {
        return Err("這不是 gifrec 的專案檔".into());
    }
    let dir = path.parent().unwrap_or(Path::new("."));
    let candidates = [dir.join(&pf.source), PathBuf::from(&pf.source_abs)];
    let source = candidates
        .iter()
        .find(|p| p.is_file())
        .cloned()
        .ok_or_else(|| format!("找不到來源 GIF：{}（請放在專案檔旁邊）", pf.source))?;

    let mut items = Vec::new();
    for it in pf.items {
        let kind = match it.kind {
            KindFile::Mosaic { block } => Kind::Mosaic { block },
            KindFile::Blur { radius } => Kind::Blur { radius },
            KindFile::Text { text, size, color, bg } => Kind::Text { text, size, color, bg },
            KindFile::Image { name, png, opacity } => {
                let bytes = B64.decode(png).map_err(|e| format!("圖片資料損壞：{e}"))?;
                let img: RgbaImage = image::load_from_memory(&bytes)
                    .map_err(|e| format!("圖片資料損壞：{e}"))?
                    .to_rgba8();
                Kind::Image { name, img: Arc::new(img), opacity }
            }
            KindFile::Zoom => Kind::Zoom,
        };
        items.push(Item { kind, x: it.x, y: it.y, w: it.w, h: it.h, start: it.start, end: it.end });
    }
    Ok(Loaded {
        source,
        doc: Doc { items, trim: (pf.trim[0], pf.trim[1]), crop: pf.crop, out_w: pf.out_w },
        frames: pf.frames,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn roundtrip() {
        let dir = std::env::temp_dir().join("gifrec-proj-test");
        std::fs::create_dir_all(&dir).unwrap();
        let gif = dir.join("a.gif");
        std::fs::write(&gif, b"GIF89a").unwrap();
        let mut doc = Doc::new(100, 50, 20);
        doc.crop = [10, 5, 80, 40];
        doc.out_w = 40;
        doc.trim = (2, 15);
        let img = Arc::new(RgbaImage::from_pixel(4, 3, image::Rgba([10, 20, 30, 128])));
        doc.items.push(Item { kind: Kind::Mosaic { block: 9 }, x: 1.0, y: 2.0, w: 30.0, h: 10.0, start: 0, end: 5 });
        doc.items.push(Item { kind: Kind::Text { text: "你好\n第二行".into(), size: 22.0, color: [1, 2, 3, 4], bg: true }, x: 3.0, y: 4.0, w: 0.0, h: 0.0, start: 3, end: 9 });
        doc.items.push(Item { kind: Kind::Image { name: "logo.png".into(), img: img.clone(), opacity: 0.5 }, x: 5.0, y: 6.0, w: 8.0, h: 6.0, start: 1, end: 2 });
        doc.items.push(Item { kind: Kind::Zoom, x: 0.0, y: 0.0, w: 40.0, h: 20.0, start: 4, end: 12 });
        let p = dir.join("t.gifrec");
        save(&p, &gif, &doc, 20).unwrap();
        let back = load(&p).unwrap();
        assert_eq!(back.frames, 20);
        assert_eq!(back.source.file_name().unwrap(), "a.gif");
        assert_eq!(back.doc.crop, doc.crop);
        assert_eq!(back.doc.trim, doc.trim);
        assert_eq!(back.doc.items.len(), 4);
        assert_eq!(back.doc.items[1].kind, doc.items[1].kind);
        match &back.doc.items[2].kind {
            Kind::Image { img: i2, .. } => assert_eq!(**i2, *img),
            _ => panic!(),
        }
        // 只存相對路徑，搬資料夾也找得到
        assert!(std::fs::read_to_string(&p).unwrap().contains("\"source\": \"a.gif\""));
    }
}
