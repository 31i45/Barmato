//! 色彩模块（文档 §4.2 / §4.3）
//!
//! 低饱和配色、暂停亮度修饰（仅变暗不换色相）、阶段平滑过渡插值、
//! 染色 WCAG 对比度自适应钳制（§4.1 染色策略）。纯计算，无系统依赖。

use crate::cycle::Phase;

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Rgb {
    pub r: u8,
    pub g: u8,
    pub b: u8,
}

impl Rgb {
    pub const fn new(r: u8, g: u8, b: u8) -> Self {
        Self { r, g, b }
    }

    /// 解析 "#RRGGBB" 或 "RRGGBB"；非法输入返回 None（由配置层静默回退）
    pub fn from_hex(s: &str) -> Option<Self> {
        let s = s.trim().trim_start_matches('#');
        if s.len() != 6 {
            return None;
        }
        let v = u32::from_str_radix(s, 16).ok()?;
        Some(Self::new(
            (v >> 16) as u8,
            ((v >> 8) & 0xFF) as u8,
            (v & 0xFF) as u8,
        ))
    }

    /// 亮度降低（文档 §4.2：暂停时整体亮度降低 40%，仅变暗不换色）
    pub fn darken(self, amount: f64) -> Self {
        let k = 1.0 - amount.clamp(0.0, 1.0);
        Self::new(
            (self.r as f64 * k) as u8,
            (self.g as f64 * k) as u8,
            (self.b as f64 * k) as u8,
        )
    }

    /// 向白色线性提亮（保持色相不变），用于进度填充色派生
    pub fn lighten(self, amount: f64) -> Self {
        self.lerp(Self::new(255, 255, 255), amount)
    }

    /// 线性插值，用于阶段切换平滑过渡（文档 §4.3：无闪烁、无突变）
    pub fn lerp(self, other: Self, t: f64) -> Self {
        let t = t.clamp(0.0, 1.0);
        Self::new(
            (self.r as f64 + (other.r as f64 - self.r as f64) * t) as u8,
            (self.g as f64 + (other.g as f64 - self.g as f64) * t) as u8,
            (self.b as f64 + (other.b as f64 - self.b as f64) * t) as u8,
        )
    }

    /// softbuffer 像素格式：0RGB
    pub fn to_pixel(self) -> u32 {
        (self.r as u32) << 16 | (self.g as u32) << 8 | self.b as u32
    }
}

/// 透明键（color-key）：覆盖窗口以此色呈现完全透明（文档 §4.1）。
/// 阶段配色不得取该值，否则进度填充将不可见——配置层已强制拦截。
pub const COLOR_KEY: Rgb = Rgb::new(255, 0, 255);

/// 待机态托盘圆点：中性暗灰（文档 §4.2）。
/// 低存在感但恒可见——托盘图标是唯一交互入口（§3.1 交互不变量），
/// 待机时若完全透明，用户将无从发现入口。
pub const IDLE_DOT: Rgb = Rgb::new(90, 90, 90);

/// 三阶段配色（文档 §4.2 默认值：低饱和、低对比度）
#[derive(Clone, Copy, PartialEq, Debug)]
pub struct Palette {
    pub focus: Rgb,
    pub short_break: Rgb,
    pub long_break: Rgb,
}

impl Palette {
    /// 出厂默认：低饱和深绿 / 薄荷青 / 浅蓝
    ///
    /// 配色经 HSL 校准（文档 §4.2：低饱和、低对比度，余光可感知）：
    /// - Focus      H145 S31% L33%  — 深绿，亮度从 L23% 提至 L33%，深色壁纸上可见
    /// - ShortBreak  H185 S30% L50%  — 薄荷青，与 Focus 色相差 40°，余光可区分
    /// - LongBreak   H215 S28% L55%  — 浅蓝，与 ShortBreak 差 30°，休息→深休息渐冷
    pub const fn defaults() -> Self {
        Self {
            focus: Rgb::new(58, 107, 76),
            short_break: Rgb::new(89, 168, 166),
            long_break: Rgb::new(101, 133, 180),
        }
    }

    pub fn for_phase(&self, phase: Phase) -> Rgb {
        match phase {
            Phase::Focus => self.focus,
            Phase::ShortBreak => self.short_break,
            Phase::LongBreak => self.long_break,
        }
    }
}

/// WCAG 相对亮度（0.0 黑 – 1.0 白）
fn relative_luminance(c: Rgb) -> f64 {
    fn lin(v: f64) -> f64 {
        if v <= 0.03928 {
            v / 12.92
        } else {
            ((v + 0.055) / 1.055).powf(2.4)
        }
    }
    let (r, g, b) = (
        lin(c.r as f64 / 255.0),
        lin(c.g as f64 / 255.0),
        lin(c.b as f64 / 255.0),
    );
    0.2126 * r + 0.7152 * g + 0.0722 * b
}

/// WCAG 对比度（1.0 – 21.0）
fn contrast_ratio(a: Rgb, b: Rgb) -> f64 {
    let (l1, l2) = (relative_luminance(a), relative_luminance(b));
    let (hi, lo) = if l1 >= l2 { (l1, l2) } else { (l2, l1) };
    (hi + 0.05) / (lo + 0.05)
}

const MIN_TEXT_CONTRAST: f64 = 4.5;
const WHITE: Rgb = Rgb::new(255, 255, 255);
const BLACK: Rgb = Rgb::new(0, 0, 0);

/// 任务栏染色自适应（文档 §4.1 染色策略）：
/// 任务栏上的系统文字（托盘时钟等）在深色任务栏为白字、浅色任务栏为黑字，
/// 染色必须保证系统文字对比度 ≥ 4.5:1（WCAG AA）。
///
/// - `light_taskbar == false`（深色任务栏，白字）：向黑压暗至对白字对比度达标
/// - `light_taskbar == true`（浅色任务栏，黑字）：向白提亮至对黑字对比度达标
///
/// 纯函数、单调收敛，用户自定义配色（§2.2 逃逸口）同样被钳制。
pub fn ensure_contrast(color: Rgb, light_taskbar: bool) -> Rgb {
    let target = if light_taskbar { BLACK } else { WHITE };
    if contrast_ratio(color, target) >= MIN_TEXT_CONTRAST {
        return color;
    }
    for step in 1..=100 {
        let t = step as f64 / 100.0;
        let adjusted = if light_taskbar {
            color.lighten(t)
        } else {
            color.darken(t)
        };
        if contrast_ratio(adjusted, target) >= MIN_TEXT_CONTRAST {
            return adjusted;
        }
    }
    if light_taskbar {
        WHITE
    } else {
        BLACK
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const PHASES: [(Phase, Rgb); 3] = [
        (Phase::Focus, Palette::defaults().focus),
        (Phase::ShortBreak, Palette::defaults().short_break),
        (Phase::LongBreak, Palette::defaults().long_break),
    ];

    #[test]
    fn hex_parse() {
        assert_eq!(Rgb::from_hex("#2D4A3A"), Some(Rgb::new(0x2D, 0x4A, 0x3A)));
        assert_eq!(Rgb::from_hex("112233"), Some(Rgb::new(0x11, 0x22, 0x33)));
        assert_eq!(Rgb::from_hex("xyz"), None);
        assert_eq!(Rgb::from_hex("#12345"), None);
    }

    #[test]
    fn darken_reduces_brightness_only() {
        assert_eq!(Rgb::new(100, 100, 100).darken(0.3), Rgb::new(70, 70, 70));
        assert_eq!(Rgb::new(10, 20, 30).darken(0.0), Rgb::new(10, 20, 30));
    }

    #[test]
    fn lerp_endpoints() {
        let a = Rgb::new(0, 0, 0);
        let b = Rgb::new(100, 200, 50);
        assert_eq!(a.lerp(b, 0.0), a);
        assert_eq!(a.lerp(b, 1.0), b);
    }

    #[test]
    fn lighten_moves_toward_white() {
        assert_eq!(Rgb::new(0, 0, 0).lighten(1.0), Rgb::new(255, 255, 255));
        assert_eq!(
            Rgb::new(100, 100, 100).lighten(0.0),
            Rgb::new(100, 100, 100)
        );
        // 提亮后每通道单调不减
        let c = Rgb::new(58, 107, 76).lighten(0.28);
        assert!(c.r >= 58 && c.g >= 107 && c.b >= 76);
    }

    #[test]
    fn pixel_format_is_0rgb() {
        assert_eq!(Rgb::new(0xAA, 0xBB, 0xCC).to_pixel(), 0x00AABBCC);
    }

    /// 深色任务栏：染色后系统白字对比度 ≥ 4.5:1（文档 §4.1 染色策略）
    #[test]
    fn tint_on_dark_taskbar_keeps_white_text_readable() {
        for (phase, color) in PHASES {
            let tint = ensure_contrast(color, false);
            assert!(
                contrast_ratio(tint, WHITE) >= MIN_TEXT_CONTRAST,
                "{phase:?}: contrast {}",
                contrast_ratio(tint, WHITE)
            );
            // 钳制必须向黑压暗而非提亮
            assert!(tint.r <= color.r && tint.g <= color.g && tint.b <= color.b);
        }
    }

    /// 浅色任务栏：染色后系统黑字对比度 ≥ 4.5:1
    #[test]
    fn tint_on_light_taskbar_keeps_black_text_readable() {
        for (phase, color) in PHASES {
            let tint = ensure_contrast(color, true);
            assert!(
                contrast_ratio(tint, BLACK) >= MIN_TEXT_CONTRAST,
                "{phase:?}: contrast {}",
                contrast_ratio(tint, BLACK)
            );
            assert!(tint.r >= color.r && tint.g >= color.g && tint.b >= color.b);
        }
    }

    /// 用户自定义极端配色（近白/近黑/透明键同值）同样被钳制（§2.2 逃逸口）
    #[test]
    fn ensure_contrast_clamps_extreme_custom_colors() {
        for c in [
            Rgb::new(250, 250, 250),
            Rgb::new(5, 5, 5),
            COLOR_KEY,
            Rgb::new(128, 128, 128),
        ] {
            let dark = ensure_contrast(c, false);
            let light = ensure_contrast(c, true);
            assert!(contrast_ratio(dark, WHITE) >= MIN_TEXT_CONTRAST);
            assert!(contrast_ratio(light, BLACK) >= MIN_TEXT_CONTRAST);
            assert_ne!(dark, COLOR_KEY);
        }
    }

    /// 已达标的颜色不被修改（最小修改原则）
    #[test]
    fn ensure_contrast_is_noop_when_already_sufficient() {
        let c = Rgb::new(43, 80, 57); // 深绿，对白字 ≈ 8:1
        assert_eq!(ensure_contrast(c, false), c);
    }
}
