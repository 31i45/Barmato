//! Winmato 库入口（文档 §6：深模块、浅接口）。
//!
//! 全部领域复杂度集中于此：`cycle` 周期状态机、`icon` 托盘渲染、
//! `colors` 色彩、`config` 配置。二进制主线（main.rs）只做系统粘合。
//! 独立库目标让示例与集成测试复用同一实现——
//! 如 `examples/gen_icon` 用与托盘相同的渲染核心生成应用图标。

pub mod colors;
pub mod config;
pub mod cycle;
pub mod icon;
