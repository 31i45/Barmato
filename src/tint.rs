//! 任务栏整体染色模块（文档 §4.1 首选渲染策略 D）
//!
//! 通过 `SetWindowCompositionAttribute`（undocumented 但经 TranslucentTB
//! 八年、数百万用户实战验证的 user32 导出）把任务栏底色染为当前阶段色相。
//! windows-rs 未声明此 API 且未收录于官方 user32 导入库，故经 GetProcAddress
//! 运行期动态解析——全部系统复杂度内埋本模块，
//! 调用方只看到 `apply(hwnd, Option<Rgb>) -> bool`。
//!
//! 降级契约（文档 §4.1 渲染降级阶梯）：
//! - `apply` 返回 false = 平台不支持/调用失败 → 上层永久降级为嵌入细条（C 方案）
//! - `apply(hwnd, None)` = 恢复系统默认外观（ACCENT_ENABLE_DEFAULT）
//!
//! 架构意义（第一性原理）：指示器寄生在宿主（任务栏）上，宿主的生命周期
//! 行为（全屏隐藏、自动隐藏、多显示器）被自动继承，无需任何探测逻辑。

use std::sync::OnceLock;

use crate::colors::Rgb;
use windows::core::{s, w};
use windows::Win32::Foundation::{BOOL, HWND};
use windows::Win32::System::LibraryLoader::{GetModuleHandleW, GetProcAddress};

/// `WindowCompositionAttribute` 枚举值：设置 ACCENT_POLICY（undocumented 常量）
const WCA_ACCENT_POLICY: u32 = 19;

/// 恢复系统默认任务栏外观
const ACCENT_ENABLE_DEFAULT: u32 = 0;
/// 纯色不透明染色（GradientColor 的 alpha 分量为 FF）
const ACCENT_ENABLE_ACCENTGRADIENT: u32 = 4;

/// TranslucentTB 实践验证的 flags：绘制左/上边框，与系统任务栏描边一致
const ACCENT_FLAGS_GRADIENT: u32 = 2;

/// SetWindowCompositionAttribute 的函数指针签名
type WcaFn = unsafe extern "system" fn(HWND, *mut WindowCompositionAttribData) -> BOOL;

/// 进程级缓存：函数指针在生命期内恒定，OnceLock 保证只解析一次，
/// 后续调用零 FFI 开销（GetModuleHandleW + GetProcAddress 不再重复）。
static WCA: OnceLock<Option<WcaFn>> = OnceLock::new();

#[repr(C)]
struct AccentPolicy {
    accent_state: u32,
    accent_flags: u32,
    /// AABBGGRR：alpha=FF（不透明），Barmato 仅使用不透明染色保证对比度确定性
    gradient_color: u32,
    animation_id: u32,
}

#[repr(C)]
struct WindowCompositionAttribData {
    attrib: u32,
    data: *mut core::ffi::c_void,
    size_of_data: usize,
}

/// 运行期解析 SetWindowCompositionAttribute：该 undocumented API 未收录于
/// 官方 user32 导入库（静态链接报 LNK2019），经 GetProcAddress 动态获取。
/// 函数指针在进程生命期内恒定，OnceLock 缓存后后续调用零 FFI 开销。
/// 解析失败（精简系统/Wine）→ None → 上层永久降级为细条（§4.1 降级阶梯）。
fn resolve_wca() -> Option<WcaFn> {
    *WCA.get_or_init(|| {
        // SAFETY: user32 模块在本进程必然已加载（消息循环依赖它）；
        // 返回的函数指针仅在拥有任务栏 HWND 的消息线程调用，无并发共享。
        unsafe {
            let user32 = GetModuleHandleW(w!("user32")).ok()?;
            let f: WcaFn =
                core::mem::transmute(GetProcAddress(user32, s!("SetWindowCompositionAttribute"))?);
            Some(f)
        }
    })
}

/// RGB → AABBGGRR（alpha = FF，不透明）。纯函数，供测试锁定字节序。
fn to_abgr(c: Rgb) -> u32 {
    0xFF00_0000 | (c.b as u32) << 16 | (c.g as u32) << 8 | c.r as u32
}

/// 把任务栏染为 `tint`；`None` 恢复系统默认。
/// 返回 false 表示平台不支持或调用失败——上层据此执行降级（文档 §4.1）。
pub fn apply(hwnd: HWND, tint: Option<Rgb>) -> bool {
    let Some(wca) = resolve_wca() else {
        return false;
    };
    let policy = match tint {
        Some(c) => AccentPolicy {
            accent_state: ACCENT_ENABLE_ACCENTGRADIENT,
            accent_flags: ACCENT_FLAGS_GRADIENT,
            gradient_color: to_abgr(c),
            animation_id: 0,
        },
        None => AccentPolicy {
            accent_state: ACCENT_ENABLE_DEFAULT,
            accent_flags: 0,
            gradient_color: 0,
            animation_id: 0,
        },
    };
    let mut data = WindowCompositionAttribData {
        attrib: WCA_ACCENT_POLICY,
        data: &policy as *const AccentPolicy as *mut core::ffi::c_void,
        size_of_data: core::mem::size_of::<AccentPolicy>(),
    };
    // SAFETY: wca 由本进程已加载的 user32 导出；仅在拥有该 HWND 的
    // 消息线程（main 消息循环）调用，data 指向的栈上策略生命期覆盖调用。
    unsafe { wca(hwnd, &mut data) }.as_bool()
}

/// 系统任务栏是否为浅色主题（浅色 → 系统文字为黑字，染色需向白提亮）。
/// 注册表缺失/读取失败时返回 false（深色，Windows 出厂默认）。
pub fn system_taskbar_is_light() -> bool {
    use windows::Win32::System::Registry::{RegGetValueW, HKEY_CURRENT_USER, RRF_RT_REG_DWORD};

    let mut value: u32 = 0;
    let mut size = core::mem::size_of::<u32>() as u32;
    let ok = unsafe {
        RegGetValueW(
            HKEY_CURRENT_USER,
            w!(r"Software\Microsoft\Windows\CurrentVersion\Themes\Personalize"),
            w!("AppsUseLightTheme"),
            RRF_RT_REG_DWORD,
            None,
            Some(&mut value as *mut u32 as *mut core::ffi::c_void),
            Some(&mut size),
        )
    }
    .is_ok();
    ok && value != 0
}

#[cfg(test)]
mod tests {
    use super::*;

    /// AABBGGRR 字节序锁定：alpha 高位 FF，RGB 反序（文档 §4.1）
    #[test]
    fn abgr_packing() {
        assert_eq!(to_abgr(Rgb::new(0x3A, 0x6B, 0x4C)), 0xFF4C_6B3A);
        assert_eq!(to_abgr(Rgb::new(0, 0, 0)), 0xFF00_0000);
        assert_eq!(to_abgr(Rgb::new(0xFF, 0xFF, 0xFF)), 0xFFFF_FFFF);
    }
}
