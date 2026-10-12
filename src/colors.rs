//! 色彩模块（文档 §4.2 / §4.3）
//!
//! 三阶段配色、阶段平滑过渡插值（lerp）。
//! 纯计算，无系统依赖。
//!
//! T 方案：托盘图标为唯一可视化表面，色彩完全自控，
//! 不受 alpha 叠加限制——配色饱和度可适当提高以增强辨识度。

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

    /// 线性插值，用于阶段切换平滑过渡（文档 §4.3：无闪烁、无突变）
    pub fn lerp(self, other: Self, t: f64) -> Self {
        let t = t.clamp(0.0, 1.0);
        Self::new(
            (self.r as f64 + (other.r as f64 - self.r as f64) * t) as u8,
            (self.g as f64 + (other.g as f64 - self.g as f64) * t) as u8,
            (self.b as f64 + (other.b as f64 - self.b as f64) * t) as u8,
        )
    }
}

/// 待机态方格轮廓色：中性暗灰。
/// 低存在感但恒可见——托盘图标是唯一交互入口，
/// 待机时若完全透明，用户将无从发现入口。
pub const IDLE_OUTLINE: Rgb = Rgb::new(110, 110, 110);

/// 三阶段配色
#[derive(Clone, Copy, PartialEq, Debug)]
pub struct Palette {
    pub focus: Rgb,
    pub short_break: Rgb,
    pub long_break: Rgb,
}

impl Palette {
    /// 出厂默认：中饱和绿 / 青 / 蓝。
    /// T 方案不再 alpha 叠加于蓝色 logo，色彩自由度提升，
    /// 适当提高饱和度以在 32×32 小尺寸中保持辨识度。
    /// - Focus      深绿 H145 S47% L33% — 深度专注
    /// - ShortBreak  薄荷青 H175 S45% L50% — 短暂休息
    /// - LongBreak   天蓝 H210 S40% L55% — 长休息
    pub const fn defaults() -> Self {
        Self {
            focus: Rgb::new(45, 140, 83),
            short_break: Rgb::new(71, 179, 179),
            long_break: Rgb::new(85, 133, 200),
        }
    }

    pub fn for_phase(&self, phase: Phase) -> Rgb {
        match phase {
            Phase::Focus => self.focus,
            Phase::ShortBreak => self.short_break,
            Phase::LongBreak => self.long_break,
        }
    }

    /// 待机/未完成格的轮廓色
    pub const fn idle_outline(&self) -> Rgb {
        IDLE_OUTLINE
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
    fn lerp_endpoints() {
        let a = Rgb::new(0, 0, 0);
        let b = Rgb::new(100, 200, 50);
        assert_eq!(a.lerp(b, 0.0), a);
        assert_eq!(a.lerp(b, 1.0), b);
    }
}
