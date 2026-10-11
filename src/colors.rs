//! 色彩模块（文档 §4.2 / §4.3）
//!
//! 低饱和配色、暂停亮度修饰（仅变暗不换色相）、阶段平滑过渡插值。
//! 纯计算，无系统依赖。

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

#[cfg(test)]
mod tests {
    use super::*;

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
}
