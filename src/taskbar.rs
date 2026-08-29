//! 任务栏几何检测模块（文档 §6.2）
//!
//! 定位任务栏（主屏 `Shell_TrayWnd`，副屏 `Shell_SecondaryTrayWnd`），
//! 获取其几何区域。所有系统复杂度内埋于此。
//!
//! 交互模型已迁移至托盘图标（文档 §3.1），本模块不再负责空白区域判定，
//! 仅提供任务栏句柄与矩形——渲染覆盖层所需的唯一信息。

use windows::Win32::Foundation::{BOOL, HWND, LPARAM, RECT};
use windows::Win32::UI::WindowsAndMessaging::{EnumWindows, GetClassNameW, GetWindowRect};

/// 主/副屏任务栏窗口类名
const PRIMARY_CLASS: &str = "Shell_TrayWnd";
const SECONDARY_CLASS: &str = "Shell_SecondaryTrayWnd";

pub struct Taskbar {
    pub hwnd: HWND,
    pub rect: RECT,
}

impl Taskbar {
    /// 探测所有需要渲染的任务栏。
    /// `all_monitors=false` 时仅返回主屏任务栏（文档 §4.1 默认）。
    pub fn refresh_all(all_monitors: bool) -> Vec<Taskbar> {
        let mut found: Vec<(HWND, bool)> = Vec::new(); // (hwnd, is_primary)
        unsafe {
            let _ = EnumWindows(
                Some(enum_taskbars),
                LPARAM(&mut found as *mut Vec<(HWND, bool)> as isize),
            );
        }
        // 主屏任务栏排前
        found.sort_by_key(|(_, is_primary)| !*is_primary);
        if !all_monitors {
            found.truncate(1);
        }
        found
            .into_iter()
            .filter_map(|(hwnd, _)| Taskbar::from_hwnd(hwnd))
            .collect()
    }

    fn from_hwnd(hwnd: HWND) -> Option<Taskbar> {
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

/// 枚举顶层窗口，收集主/副屏任务栏
unsafe extern "system" fn enum_taskbars(hwnd: HWND, lp: LPARAM) -> BOOL {
    let out = &mut *(lp.0 as *mut Vec<(HWND, bool)>);
    let mut name = [0u16; 64];
    let n = GetClassNameW(hwnd, &mut name).max(0);
    let class = String::from_utf16_lossy(&name[..n as usize]);
    if class == PRIMARY_CLASS {
        out.push((hwnd, true));
    } else if class == SECONDARY_CLASS {
        out.push((hwnd, false));
    }
    BOOL(1)
}
