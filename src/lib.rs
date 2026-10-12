//! Winmato 库入口（文档 §6：深模块、浅接口）。
//!
//! 全部领域复杂度集中于此：`cycle` 周期状态机、`icon` 托盘渲染、
//! `colors` 色彩、`config` 配置。二进制主线（main.rs）只做系统粘合。
//! 独立库目标让集成测试直接对库断言。

pub mod colors;
pub mod config;
pub mod cycle;
pub mod icon;
