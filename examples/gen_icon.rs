//! 生成应用图标 `Winmato.ico`（仓库资产，一次性工具，非构建期依赖）。
//!
//! 复用托盘渲染核心（`icon::idle_rgba`）逐尺寸绘制「待填充四格」待机态——
//! 每个尺寸独立推导几何而非缩放位图，任何尺寸下线条都精确 1px，
//! 且 exe 图标与托盘待机图标语义、几何、配色逐像素一致。
//!
//! 用法：`cargo run --example gen_icon`
//! 输出：`$CARGO_MANIFEST_DIR/Winmato.ico`（覆盖）
//!
//! 实现说明：ICO 容器内嵌 32 位 BMP 项（BITMAPINFOHEADER + 自底向上
//! BGRA 像素 + 全零 AND 掩码），零第三方依赖——标准库即可完成。

use std::env;
use std::fs::File;
use std::io::Write;
use std::path::PathBuf;

use winmato::colors::Palette;
use winmato::icon;

/// 图标包含的尺寸：Explorer / 任务栏按 DPI 与视图取用，256 供大图标视图
const SIZES: [u16; 7] = [16, 24, 32, 48, 64, 128, 256];

fn main() -> std::io::Result<()> {
    let palette = Palette::defaults();
    let entries: Vec<(u16, Vec<u8>)> = SIZES
        .iter()
        .map(|&s| (s, bmp_entry(s as i32, &palette)))
        .collect();

    // ICONDIR
    let mut out = Vec::new();
    out.extend(0u16.to_le_bytes()); // reserved
    out.extend(1u16.to_le_bytes()); // type: icon
    out.extend((entries.len() as u16).to_le_bytes());
    // ICONDIRENTRY × N
    let mut offset = (6 + 16 * entries.len()) as u32;
    for (size, data) in &entries {
        let dim = if *size == 256 { 0u8 } else { *size as u8 };
        out.extend([dim, dim, 0, 0]); // width, height, 调色板数, 保留
        out.extend(1u16.to_le_bytes()); // planes
        out.extend(32u16.to_le_bytes()); // bit count
        out.extend((data.len() as u32).to_le_bytes());
        out.extend(offset.to_le_bytes());
        offset += data.len() as u32;
    }
    for (_, data) in &entries {
        out.extend(data);
    }

    let path = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("Winmato.ico");
    File::create(&path)?.write_all(&out)?;
    println!("已生成 {}（{} 个尺寸）", path.display(), entries.len());
    Ok(())
}

/// 单尺寸 BMP 图像项：BITMAPINFOHEADER + XOR（自底向上 BGRA）+ AND 掩码
fn bmp_entry(size: i32, palette: &Palette) -> Vec<u8> {
    let rgba = icon::idle_rgba(size, palette);
    let w = size as usize;
    let xor_len = w * w * 4;
    let and_row = w.div_ceil(32) * 4; // AND 掩码行按 32 位对齐
    let and_len = and_row * w;

    let mut bmp = Vec::with_capacity(40 + xor_len + and_len);
    // BITMAPINFOHEADER：biHeight = XOR + AND 两段的总行数（×2）
    bmp.extend(40u32.to_le_bytes());
    bmp.extend((size as u32).to_le_bytes());
    bmp.extend(((size * 2) as u32).to_le_bytes());
    bmp.extend(1u16.to_le_bytes()); // planes
    bmp.extend(32u16.to_le_bytes()); // bit count
    bmp.extend(0u32.to_le_bytes()); // BI_RGB
    bmp.extend(((xor_len + and_len) as u32).to_le_bytes());
    bmp.extend([0u8; 16]); // 分辨率 ×2 + 调色板数 + 重要色数
                           // XOR 像素：RGBA → BGRA，自底向上
    for y in (0..w).rev() {
        for x in 0..w {
            let i = (y * w + x) * 4;
            bmp.extend([rgba[i + 2], rgba[i + 1], rgba[i], rgba[i + 3]]);
        }
    }
    // AND 掩码：全 0——透明度完全由 alpha 通道表达
    bmp.resize(bmp.len() + and_len, 0);
    bmp
}
