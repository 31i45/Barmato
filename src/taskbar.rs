//! 任务栏几何检测模块（文档 §6.2）
//!
//! 定位任务栏（主屏 `Shell_TrayWnd`，副屏 `Shell_SecondaryTrayWnd`），
//! 获取其几何区域。所有系统复杂度内埋于此。
//!
//! 交互模型已迁移至托盘图标（文档 §3.1），本模块不再负责空白区域判定，
//! 仅提供任务栏句柄与矩形——渲染覆盖层所需的唯一信息。
//!
//! 使用 `FindWindowW` O(1) 精确查找（类名为已知常量），替代 `EnumWindows`
//! 全量枚举 + 逐窗口 `GetClassNameW` + `String::from_utf16_lossy` 堆分配。

use windows::core::w;
use windows::Win32::Foundation::{HWND, RECT};
use windows::Win32::UI::WindowsAndMessaging::{FindWindowW, GetWindowRect};

pub struct Taskbar {
    pub hwnd: HWND,
    pub rect: RECT,
}

impl Taskbar {
    /// 探测所有需要渲染的任务栏。
    /// `all_monitors=false` 时仅返回主屏任务栏（文档 §4.1 默认）。
    pub fn refresh_all(all_monitors: bool) -> Vec<Taskbar> {
        let mut result = Vec::new();
        // 主屏任务栏：FindWindowW O(1) 精确查找，零堆分配
        if let Some(tb) = Self::from_class(w!("Shell_TrayWnd")) {
            result.push(tb);
        }
        if all_monitors {
            if let Some(tb) = Self::from_class(w!("Shell_SecondaryTrayWnd")) {
                result.push(tb);
            }
        }
        result
    }

    /// 按窗口类名查找任务栏并获取其屏幕矩形。
    /// 类名不存在/窗口无有效矩形 → None（静默跳过，文档 §7）。
    fn from_class(class: windows::core::PCWSTR) -> Option<Taskbar> {
        let hwnd = unsafe { FindWindowW(class, None) }.ok()?;
        let mut rect = RECT::default();
        if unsafe { GetWindowRect(hwnd, &mut rect) }.is_err() {
            return None;
        }
        if rect.right <= rect.left || rect.bottom <= rect.top {
            return None;
        }
        Some(Taskbar { hwnd, rect })
    }
}
