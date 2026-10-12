//! 配置加载模块（文档 §6.5 / §2.2）
//!
//! 只读加载 %APPDATA%\Winmato\config.json。
//! 缺失、损坏、字段非法时一律静默回退出厂默认参数——不报错、不弹窗、不写回。
//!
//! 配置单位为**秒**（出厂默认 1500s/300s/1200s，即 25min/5min/20min）。
//! 秒级配置让开发者/发烧友可用短时长（如 25s）快速验证完整番茄大周期；
//! 合法范围 1..=36000 秒（1 秒 ~ 10 小时），越界回退默认。

use std::path::PathBuf;
use std::time::Duration;

use serde::Deserialize;

use crate::colors::{Palette, Rgb};

/// 出厂默认时长（秒）——全局唯一事实来源。
/// 程序默认与配置解析回退共用此常量，杜绝两处字面量漂移（避免重复实现）。
/// 真实番茄钟节奏：专注 25min / 短休 5min / 长休 20min。
pub const DEFAULT_FOCUS_SECS: u64 = 1500;
pub const DEFAULT_SHORT_BREAK_SECS: u64 = 300;
pub const DEFAULT_LONG_BREAK_SECS: u64 = 1200;

/// 时长合法区间（秒）：1 ~ 10 小时
const MIN_SECS: u64 = 1;
const MAX_SECS: u64 = 36000;

pub struct AppConfig {
    pub focus: Duration,
    pub short_break: Duration,
    pub long_break: Duration,
    pub palette: Palette,
}

impl Default for AppConfig {
    fn default() -> Self {
        Self {
            focus: Duration::from_secs(DEFAULT_FOCUS_SECS),
            short_break: Duration::from_secs(DEFAULT_SHORT_BREAK_SECS),
            long_break: Duration::from_secs(DEFAULT_LONG_BREAK_SECS),
            palette: Palette::defaults(),
        }
    }
}

#[derive(Deserialize)]
struct RawFile {
    /// 各阶段时长（秒）
    durations: Option<RawDurations>,
    colors: Option<RawColors>,
}

#[derive(Deserialize)]
struct RawDurations {
    focus: Option<u64>,
    short_break: Option<u64>,
    long_break: Option<u64>,
}

#[derive(Deserialize)]
struct RawColors {
    focus: Option<String>,
    short_break: Option<String>,
    long_break: Option<String>,
}

pub fn load() -> AppConfig {
    let text = std::fs::read_to_string(config_path()).unwrap_or_default();
    parse(&text)
}

fn config_path() -> PathBuf {
    let mut p = std::env::var("APPDATA")
        .map(PathBuf::from)
        .unwrap_or_else(|_| PathBuf::from("."));
    p.push("Winmato");
    p.push("config.json");
    p
}

/// 解析配置文本。任何异常都静默回退默认值（优雅降级，文档 §7）
pub fn parse(text: &str) -> AppConfig {
    let mut cfg = AppConfig::default();
    if text.trim().is_empty() {
        return cfg;
    }
    let Ok(raw) = serde_json::from_str::<RawFile>(text) else {
        return cfg;
    };
    if let Some(d) = raw.durations {
        cfg.focus = seconds(d.focus, DEFAULT_FOCUS_SECS);
        cfg.short_break = seconds(d.short_break, DEFAULT_SHORT_BREAK_SECS);
        cfg.long_break = seconds(d.long_break, DEFAULT_LONG_BREAK_SECS);
    }
    if let Some(c) = raw.colors {
        if let Some(v) = parse_color(&c.focus) {
            cfg.palette.focus = v;
        }
        if let Some(v) = parse_color(&c.short_break) {
            cfg.palette.short_break = v;
        }
        if let Some(v) = parse_color(&c.long_break) {
            cfg.palette.long_break = v;
        }
    }
    cfg
}

/// 时长合法性校验：1..=36000 秒，越界/缺失回退默认
fn seconds(v: Option<u64>, default: u64) -> Duration {
    match v {
        Some(s) if (MIN_SECS..=MAX_SECS).contains(&s) => Duration::from_secs(s),
        _ => Duration::from_secs(default),
    }
}

/// 解析阶段配色；非法输入返回 None（由调用方静默回退默认）
fn parse_color(v: &Option<String>) -> Option<Rgb> {
    v.as_deref().and_then(Rgb::from_hex)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn empty_gives_defaults() {
        let c = parse("");
        assert_eq!(c.focus, Duration::from_secs(DEFAULT_FOCUS_SECS));
        assert_eq!(c.short_break, Duration::from_secs(DEFAULT_SHORT_BREAK_SECS));
        assert_eq!(c.long_break, Duration::from_secs(DEFAULT_LONG_BREAK_SECS));
    }

    #[test]
    fn invalid_json_gives_defaults() {
        let c = parse("{oops");
        assert_eq!(c.long_break, Duration::from_secs(DEFAULT_LONG_BREAK_SECS));
        assert_eq!(c.palette, Palette::defaults());
    }

    #[test]
    fn partial_override() {
        let c = parse(r##"{"durations":{"focus":60},"colors":{"focus":"#112233"}}"##);
        assert_eq!(c.focus, Duration::from_secs(60));
        assert_eq!(c.short_break, Duration::from_secs(DEFAULT_SHORT_BREAK_SECS));
        assert_eq!(c.palette.focus, Rgb::new(0x11, 0x22, 0x33));
        assert_eq!(c.palette.short_break, Palette::defaults().short_break);
    }

    #[test]
    fn seconds_unit_accepted() {
        // 秒级配置：25s/5s/20s，用于快速验证大周期流转
        let c = parse(r##"{"durations":{"focus":25,"short_break":5,"long_break":20}}"##);
        assert_eq!(c.focus, Duration::from_secs(25));
        assert_eq!(c.short_break, Duration::from_secs(5));
        assert_eq!(c.long_break, Duration::from_secs(20));
    }

    #[test]
    fn out_of_range_duration_falls_back() {
        let c = parse(r#"{"durations":{"focus":0,"long_break":99999}}"#);
        assert_eq!(c.focus, Duration::from_secs(DEFAULT_FOCUS_SECS));
        assert_eq!(c.long_break, Duration::from_secs(DEFAULT_LONG_BREAK_SECS));
    }

    #[test]
    fn invalid_color_falls_back() {
        let c = parse(r#"{"colors":{"focus":"not-a-color"}}"#);
        assert_eq!(c.palette.focus, Palette::defaults().focus);
    }
}
