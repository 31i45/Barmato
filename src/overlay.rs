//! 渲染绘制模块（文档 §6.4）
//!
//! 技术方案（文档 §4.1）：创建独立顶层分层窗口，细条**嵌入任务栏屏幕内侧缘**
//! 呈现一条 5px 环境感知条（C 方案几何；首选的 D 整体染色由 tint.rs 承担）。
//! 用 `HWND_TOPMOST` 保持置顶，用 `WS_EX_TRANSPARENT` 实现鼠标穿透，
//! 用 `WS_EX_NOACTIVATE` 避免抢占焦点。
//!
//! **为何不用 `WS_CHILD` / `SetParent`**：
//! 任务栏属于 explorer.exe 的线程。跨线程创建 `WS_CHILD` 子窗口在 Windows 上
//! 会返回 NULL（`ERROR_ALREADY_EXISTS`）；`SetParent` 对 `WS_EX_LAYERED` 窗口
//! 也无法正确建立父子关系。因此采用独立顶层窗口 + 精确 Z 序管理。
//!
//! **为何嵌入任务栏内缘而非悬浮其上方**：
//! 悬浮线与任务栏无共同区域，格式塔上沦为"孤儿线"（归属歧义、背景被壁纸
//! 劫持）。嵌入内缘后与任务栏形成共同区域（归属明确、背景可控），Win11
//! 图标垂直居中留出内缘 padding，细条零功能遮挡；鼠标穿透保证点击原样下达。
//!
//! - 底色常亮：运行时细条铺阶段底色，余光即可感知（可感知性）；
//!   进度用同色相提亮色从左至右匀速叠加，表达**当前阶段**进度（文档 §4.2）。
//!   待机时窗口整体隐藏，桌面恢复原生样式。
//! - 4 枚几何刻点：空间模式表达「第 N 个专注番茄」，绝不显示数字（§4.2）。
//! - 边界脉冲：阶段切换瞬间整条 2.5s 慢脉冲一次，仅标记事件边界（§4.3）。
//! - 暂停呼吸：降亮 40% 叠加 12s 周期 ±10% 明度慢呼吸（§4.2）。
//! - `WS_EX_LAYERED` + color-key 透明：分层窗口、`COLOR_KEY` 像素完全透明。
//! - `WS_EX_TRANSPARENT`：鼠标输入穿透。
//! - `softbuffer` 软件光栅化：纯色填充 + 进度条，无 GPU/浏览器依赖。
//!
//! 渲染范围仅限细条本身；交互入口为托盘图标（文档 §3.1/§4.2），
//! 二者职责分离，互不影响。
//!
//! 待机状态彻底隐藏并停止绘制（文档 §4.3：销毁渲染、极致轻量）。
//! 阶段过渡插值、边界脉冲、暂停呼吸全部内埋于本模块（复杂度下沉，主线零感知）。

use std::num::NonZeroU32;
use std::sync::Once;
use std::time::Instant;

use raw_window_handle::{
    DisplayHandle, HasDisplayHandle, HasWindowHandle, RawDisplayHandle, RawWindowHandle,
    Win32WindowHandle, WindowHandle, WindowsDisplayHandle,
};
use windows::core::{w, PCWSTR};
use windows::Win32::Foundation::{COLORREF, HWND, LPARAM, LRESULT, RECT, WPARAM};
use windows::Win32::UI::WindowsAndMessaging::*;

use crate::colors::{Rgb, COLOR_KEY};
use crate::cycle::Snapshot;

const CLASS_NAME: PCWSTR = w!("BarmatoOverlay");

/// 窗口类全进程只注册一次（重复注册返回错误，Once 幂等保证）
static REGISTER: Once = Once::new();

/// 阶段切换色彩过渡时长（文档 §4.3：平滑过渡、无突变）
const TRANSITION_SECS: f64 = 0.6;

/// 细条高度（屏幕像素）。5px 足够余光可感知，又不干扰任务栏内容。
const BAR_HEIGHT_PX: i32 = 5;

/// 进度边界发光宽度（像素）。在进度前沿处渲染一条更亮的色带，
/// 类似发光边缘效果，增强当前进度位置的环境感知。
const GLOW_WIDTH_PX: usize = 8;

/// 边界脉冲时长（文档 §4.3：阶段切换瞬间整条慢脉冲一次，约 2.5s）
const PULSE_SECS: f64 = 2.5;
/// 边界脉冲峰值增益（明度提亮约 15%）
const PULSE_PEAK: f64 = 0.15;
/// 暂停呼吸周期（文档 §4.2：约 12s）
const BREATH_PERIOD_SECS: f64 = 12.0;
/// 暂停呼吸幅度（明度 ±10%）
const BREATH_AMPLITUDE: f64 = 0.10;
/// 刻点边长（2×2 px 微型几何标记，文档 §4.2）
const TICK_PX: usize = 2;
/// 刻点数量（4 次专注番茄，文档 §2.1 / §4.2）
const TICK_COUNT: usize = 4;

/// 边界脉冲增益：0 → 峰值 → 0 的单次正弦呼吸，t ∈ [0,1]（文档 §4.3）
fn pulse_gain(t: f64) -> f64 {
    PULSE_PEAK * (std::f64::consts::PI * t.clamp(0.0, 1.0)).sin()
}

/// 暂停呼吸增益：±10% 明度、12s 周期的低频正弦（文档 §4.2）
fn breath_gain(elapsed_secs: f64) -> f64 {
    BREATH_AMPLITUDE * (std::f64::consts::TAU * elapsed_secs / BREATH_PERIOD_SECS).sin()
}

/// 第 i 枚刻点的中心 x 坐标：均匀分布于细条 1/8、3/8、5/8、7/8 处（文档 §4.2）
fn tick_center_x(i: usize, width: usize) -> usize {
    ((2 * i + 1) * width) / (2 * TICK_COUNT)
}

/// raw-window-handle 适配：把裸 HWND 包装成 softbuffer 需要的句柄类型
struct WinHandle(HWND);

impl HasWindowHandle for WinHandle {
    fn window_handle(&self) -> Result<WindowHandle<'_>, raw_window_handle::HandleError> {
        let nz = std::num::NonZeroIsize::new(self.0 .0 as isize)
            .ok_or(raw_window_handle::HandleError::Unavailable)?;
        // SAFETY: HWND 由本进程创建且在当前线程有效
        Ok(unsafe { WindowHandle::borrow_raw(RawWindowHandle::Win32(Win32WindowHandle::new(nz))) })
    }
}

/// softbuffer 需要进程级显示句柄；Windows 下无真实句柄可借，
/// 提供空壳即满足 raw-window-handle 契约（进程内唯一，无并发共享）。
struct SoftbufferDisplay;

impl HasDisplayHandle for SoftbufferDisplay {
    fn display_handle(&self) -> Result<DisplayHandle<'_>, raw_window_handle::HandleError> {
        Ok(unsafe {
            DisplayHandle::borrow_raw(RawDisplayHandle::Windows(WindowsDisplayHandle::new()))
        })
    }
}

/// 紧贴任务栏屏幕内侧的 5px 细条矩形。
///
/// 判定逻辑（嵌入策略，文档 §4.1 C 方案）：细条嵌入任务栏**屏幕内侧缘**，
/// 与任务栏形成共同区域（归属明确），上/下邻至少一面为任务栏内表面（背景可控）：
/// - 水平任务栏（宽 > 高）：top == 0 → 顶部任务栏，细条嵌入其下缘；否则 → 底部任务栏，细条嵌入其上缘
/// - 垂直任务栏（高 > 宽）：left == 0 → 左侧任务栏，细条嵌入其右缘；否则 → 右侧任务栏，细条嵌入其左缘
///
/// Win11 任务栏图标垂直居中，内缘侧为 8px+ 纯 padding——细条零功能遮挡，
/// 且窗口鼠标穿透（WS_EX_TRANSPARENT），padding 区点击原样穿透给任务栏。
/// Win10 图标近满高，细条轻擦按钮顶边空白非客户区，视觉无感、功能零影响。
///
/// 主屏坐标系原点为 (0,0)，故 top==0 / left==0 可准确区分主屏顶部/左侧任务栏。
/// 副屏场景下该启发式可能偏差，但副屏默认关闭渲染（文档 §4.1），影响可忽略。
fn bar_rect(taskbar: RECT) -> Option<RECT> {
    let w = taskbar.right - taskbar.left;
    let h = taskbar.bottom - taskbar.top;
    if w <= 0 || h <= 0 {
        return None;
    }
    let strip = if h > w {
        // 垂直任务栏（左侧/右侧）：嵌入屏幕内侧缘
        if taskbar.left == 0 {
            // 左侧任务栏 → 细条嵌入其右缘
            RECT {
                left: taskbar.right - BAR_HEIGHT_PX,
                top: taskbar.top,
                right: taskbar.right,
                bottom: taskbar.bottom,
            }
        } else {
            // 右侧任务栏 → 细条嵌入其左缘
            RECT {
                left: taskbar.left,
                top: taskbar.top,
                right: taskbar.left + BAR_HEIGHT_PX,
                bottom: taskbar.bottom,
            }
        }
    } else {
        // 水平任务栏（顶部/底部）：嵌入屏幕内侧缘
        if taskbar.top == 0 {
            // 顶部任务栏 → 细条嵌入其下缘
            RECT {
                left: taskbar.left,
                top: taskbar.bottom - BAR_HEIGHT_PX,
                right: taskbar.right,
                bottom: taskbar.bottom,
            }
        } else {
            // 底部任务栏 → 细条嵌入其上缘
            RECT {
                left: taskbar.left,
                top: taskbar.top,
                right: taskbar.right,
                bottom: taskbar.top + BAR_HEIGHT_PX,
            }
        }
    };
    // 安全兜底：细条不能有负坐标或零尺寸
    if strip.left < 0 || strip.top < 0 || strip.right <= strip.left || strip.bottom <= strip.top {
        return None;
    }
    Some(strip)
}

pub struct Overlay {
    hwnd: HWND,
    context: softbuffer::Context<SoftbufferDisplay>,
    surface: Option<softbuffer::Surface<SoftbufferDisplay, WinHandle>>,
    width: u32,
    height: u32,
    visible: bool,
    // —— 阶段过渡插值状态（内埋于渲染模块，主线不感知）——
    last_target: Option<Rgb>,
    prev_color: Rgb,
    transition_start: Option<Instant>,
    // —— 边界脉冲状态（§4.3：阶段切换瞬间整条慢脉冲一次）——
    last_stage_seq: Option<u64>,
    pulse_start: Option<Instant>,
    // —— 暂停呼吸状态（§4.2：降亮 40% + 12s 慢呼吸）——
    pause_since: Option<Instant>,
}

impl Overlay {
    /// 创建独立顶层分层窗口。失败返回 None，由上层静默降级。
    pub fn new(taskbar: HWND) -> Option<Overlay> {
        unsafe {
            let instance = windows::Win32::System::LibraryLoader::GetModuleHandleW(None).ok()?;
            REGISTER.call_once(|| {
                let wc = WNDCLASSW {
                    style: CS_HREDRAW | CS_VREDRAW,
                    lpfnWndProc: Some(wnd_proc),
                    hInstance: instance.into(),
                    lpszClassName: CLASS_NAME,
                    ..Default::default()
                };
                RegisterClassW(&wc);
            });

            // 独立顶层窗口（WS_POPUP），不作为任务栏子窗口——
            // 跨线程 WS_CHILD 创建会失败，SetParent 对 LAYERED 也无效。
            // 用独立窗口 + 精确位置/Z序实现覆盖。
            let hwnd = CreateWindowExW(
                WS_EX_LAYERED | WS_EX_TRANSPARENT | WS_EX_NOACTIVATE | WS_EX_TOOLWINDOW,
                CLASS_NAME,
                None,
                WS_POPUP,
                0,
                0,
                1,
                1,
                None,
                None,
                instance,
                None,
            )
            .ok()?;

            // color-key 透明：COLOR_KEY 像素完全透明，透出任务栏原生底色
            SetLayeredWindowAttributes(
                hwnd,
                COLORREF(
                    COLOR_KEY.r as u32 | ((COLOR_KEY.g as u32) << 8) | ((COLOR_KEY.b as u32) << 16),
                ),
                0,
                LWA_COLORKEY,
            )
            .ok()?;

            // 关联任务栏句柄（供 show() 读取几何），但不建立父子关系
            let _ = SetPropW(hwnd, w!("BarmatoTaskbar"), taskbar);

            let context = softbuffer::Context::new(SoftbufferDisplay).ok()?;
            Some(Overlay {
                hwnd,
                context,
                surface: None,
                width: 0,
                height: 0,
                visible: false,
                last_target: None,
                prev_color: Rgb::new(0, 0, 0),
                transition_start: None,
                last_stage_seq: None,
                pulse_start: None,
                pause_since: None,
            })
        }
    }

    /// 父任务栏是否仍然有效
    pub fn is_alive(&self) -> bool {
        unsafe { IsWindow(self.hwnd) }.as_bool()
    }

    pub fn is_visible(&self) -> bool {
        self.visible
    }

    /// 是否有进行中的动画需要持续重绘：过渡插值 / 边界脉冲 / 暂停呼吸
    pub fn needs_continuous_render(&self) -> bool {
        self.transition_start.is_some() || self.pulse_start.is_some() || self.pause_since.is_some()
    }

    /// 是否需要高帧率：仅过渡与脉冲这类短动画需要；12s 慢呼吸用常规帧率已足够平滑
    pub fn needs_fast_frames(&self) -> bool {
        self.transition_start.is_some() || self.pulse_start.is_some()
    }

    /// 紧贴任务栏屏幕内侧显示 5px 细条。位置由 `bar_rect` 从任务栏矩形推导。
    pub fn show(&mut self) {
        unsafe {
            // 读取关联的任务栏句柄
            let taskbar = HWND(GetPropW(self.hwnd, w!("BarmatoTaskbar")).0 as *mut _);
            if taskbar.is_invalid() {
                return;
            }
            let mut r = RECT::default();
            if GetWindowRect(taskbar, &mut r).is_err() {
                return;
            }
            // 计算紧贴任务栏屏幕内侧的细条矩形
            let Some(strip) = bar_rect(r) else {
                return;
            };
            let w = (strip.right - strip.left).max(0);
            let h = (strip.bottom - strip.top).max(0);
            let _ = SetWindowPos(
                self.hwnd,
                HWND_TOPMOST,
                strip.left,
                strip.top,
                w,
                h,
                SWP_NOACTIVATE | SWP_SHOWWINDOW,
            );
            self.width = w as u32;
            self.height = h as u32;
            self.visible = true;
        }
    }

    /// 待机：隐藏窗口、释放表面，停止一切绘制（文档 §4.3）
    pub fn hide(&mut self) {
        unsafe {
            let _ = ShowWindow(self.hwnd, SW_HIDE);
        }
        self.surface = None;
        self.visible = false;
        // 复位全部动画状态：下个周期从纯净状态开始，不与上周期残色/残脉冲插值
        self.last_target = None;
        self.transition_start = None;
        self.last_stage_seq = None;
        self.pulse_start = None;
        self.pause_since = None;
    }

    /// 按快照重绘一帧。任何渲染失败都静默降级为隐藏（文档 §6.4 / §7），
    /// 上层发现 `is_visible()==false` 后会自动重新 `show()` 恢复。
    pub fn render(&mut self, snap: Snapshot, target: Rgb, now: Instant) {
        if !self.visible || snap.idle || self.width == 0 || self.height == 0 {
            return;
        }
        // 每帧重断言 HWND_TOPMOST：窗口打开/关闭时任务栏(Shell_TrayWnd)会
        // 重新断言自身 TOPMOST 把自己推到 topmost band 顶部，覆盖本 overlay
        // （overlay 嵌在任务栏矩形内，z 序低于任务栏即被完全遮挡）。
        // SWP_NOMOVE|SWP_NOSIZE 使此调用仅更新 z 序+显示状态，零几何开销。
        unsafe {
            let _ = SetWindowPos(
                self.hwnd,
                HWND_TOPMOST,
                0,
                0,
                0,
                0,
                SWP_NOACTIVATE | SWP_NOMOVE | SWP_NOSIZE | SWP_SHOWWINDOW,
            );
        }
        self.update_anim_state(snap, now);
        let color = self.resolve_color(snap, target, now);
        if self.draw(snap, color).is_err() {
            self.hide();
        }
    }

    /// 根据快照维护动画状态机（内埋，主线零感知）：
    /// - stage_seq 变化 → 触发一次边界脉冲（含周期启动，文档 §4.3）
    /// - paused 上升沿 → 记录呼吸起点；下降沿 → 停止呼吸（文档 §4.2）
    fn update_anim_state(&mut self, snap: Snapshot, now: Instant) {
        if self.last_stage_seq != Some(snap.stage_seq) {
            self.pulse_start = Some(now);
            self.last_stage_seq = Some(snap.stage_seq);
        }
        if snap.paused {
            if self.pause_since.is_none() {
                self.pause_since = Some(now);
            }
        } else {
            self.pause_since = None;
        }
    }

    /// 目标色 → 阶段过渡插值 → 边界脉冲 → 暂停降亮+呼吸，全部在此收敛（文档 §4.2 / §4.3）
    fn resolve_color(&mut self, snap: Snapshot, target: Rgb, now: Instant) -> Rgb {
        if self.last_target != Some(target) {
            if let Some(prev) = self.last_target {
                self.prev_color = prev;
                self.transition_start = Some(now);
            }
            self.last_target = Some(target);
        }
        let mut color = match self.transition_start {
            Some(t0) => {
                let t = now.duration_since(t0).as_secs_f64() / TRANSITION_SECS;
                if t >= 1.0 {
                    self.transition_start = None;
                    target
                } else {
                    self.prev_color.lerp(target, t)
                }
            }
            None => target,
        };
        // 边界脉冲（§4.3）：阶段切换瞬间整条明度提亮一次再回落，仅标记事件
        if let Some(t0) = self.pulse_start {
            let t = now.duration_since(t0).as_secs_f64() / PULSE_SECS;
            if t >= 1.0 {
                self.pulse_start = None;
            } else {
                color = color.lighten(pulse_gain(t));
            }
        }
        // 暂停（§4.2）：整体降亮 40%（仅变暗不换色相），叠加 12s ±10% 慢呼吸
        if snap.paused {
            color = color.darken(0.4);
            if let Some(t0) = self.pause_since {
                let g = breath_gain(now.duration_since(t0).as_secs_f64());
                color = if g >= 0.0 {
                    color.lighten(g)
                } else {
                    color.darken(-g)
                };
            }
        }
        color
    }

    fn draw(&mut self, snap: Snapshot, color: Rgb) -> Result<(), ()> {
        let w = NonZeroU32::new(self.width).ok_or(())?;
        let h = NonZeroU32::new(self.height).ok_or(())?;

        if self.surface.is_none() {
            self.surface = softbuffer::Surface::new(&self.context, WinHandle(self.hwnd)).ok();
        }
        let surface = self.surface.as_mut().ok_or(())?;
        surface.resize(w, h).map_err(|_| ())?;

        let mut buf = surface.buffer_mut().map_err(|_| ())?;
        // 底色常亮：细条铺阶段底色，余光即可感知（可感知性）；
        // 进度用同色相提亮色从左至右匀速叠加，表达当前阶段进度（文档 §4.2）。
        // 暂停/脉冲的整体明度修饰已在 resolve_color 中收敛，相对关系保持不变。
        let base = color.to_pixel();
        let fill = color.lighten(0.35).to_pixel();
        // 发光边缘色：比进度填充色更亮，在进度前沿处形成发光带
        let glow = color.lighten(0.55).to_pixel();
        let progress = snap.phase_progress.clamp(0.0, 1.0);
        let fill_w = ((self.width as f64) * progress) as usize;
        let width = self.width as usize;
        // 发光带范围：[fill_w - GLOW_WIDTH_PX, fill_w)，
        // 不超出已填充区域，不延伸到未填充区域
        let glow_start = fill_w.saturating_sub(GLOW_WIDTH_PX);
        let glow_end = fill_w; // 不超出已填充区域
        for row in buf.chunks_exact_mut(width) {
            // 已填充区域的非发光部分
            row[..glow_start].fill(fill);
            // 发光带（进度前沿）
            if glow_end > glow_start {
                row[glow_start..glow_end].fill(glow);
            }
            // 未填充区域
            row[fill_w..].fill(base);
        }
        // 4 枚几何刻点（文档 §4.2）：空间模式表达「第 N 个专注番茄」，
        // 纯位置编码、绝不显示数字；亮起与进度前沿同亮度，熄灭仅微弱可见
        let lit = glow;
        let dim = color.lighten(0.12).to_pixel();
        let height = self.height as usize;
        let y0 = height.saturating_sub(TICK_PX) / 2;
        for i in 0..TICK_COUNT {
            let cx = tick_center_x(i, width);
            let x0 = cx.saturating_sub(TICK_PX / 2);
            let x1 = (x0 + TICK_PX).min(width);
            if x0 >= x1 {
                continue;
            }
            let px = if (i as u8) < snap.pomodoro_index {
                lit
            } else {
                dim
            };
            for row in buf.chunks_exact_mut(width).skip(y0).take(TICK_PX) {
                row[x0..x1].fill(px);
            }
        }
        buf.present().map_err(|_| ())?;
        Ok(())
    }
}

impl Drop for Overlay {
    fn drop(&mut self) {
        unsafe {
            let _ = DestroyWindow(self.hwnd);
        }
    }
}

unsafe extern "system" fn wnd_proc(
    hwnd: HWND,
    msg: u32,
    wparam: WPARAM,
    lparam: LPARAM,
) -> LRESULT {
    // 覆盖层不处理任何消息（绘制由 softbuffer 直接提交），全部交给默认处理
    DefWindowProcW(hwnd, msg, wparam, lparam)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rect(l: i32, t: i32, r: i32, b: i32) -> RECT {
        RECT {
            left: l,
            top: t,
            right: r,
            bottom: b,
        }
    }

    #[test]
    fn bar_rect_bottom_taskbar() {
        // 底部任务栏（最常见）：细条嵌入其上缘（文档 §4.1 C 方案）
        let tb = rect(0, 912, 1707, 960);
        let strip = bar_rect(tb).unwrap();
        assert_eq!(strip, rect(0, 912, 1707, 917));
    }

    #[test]
    fn bar_rect_top_taskbar() {
        // 顶部任务栏：细条嵌入其下缘
        let tb = rect(0, 0, 1920, 48);
        let strip = bar_rect(tb).unwrap();
        assert_eq!(strip, rect(0, 43, 1920, 48));
    }

    #[test]
    fn bar_rect_left_taskbar() {
        // 左侧任务栏：细条嵌入其右缘
        let tb = rect(0, 0, 48, 1080);
        let strip = bar_rect(tb).unwrap();
        assert_eq!(strip, rect(43, 0, 48, 1080));
    }

    #[test]
    fn bar_rect_right_taskbar() {
        // 右侧任务栏：细条嵌入其左缘
        let tb = rect(1872, 0, 1920, 1080);
        let strip = bar_rect(tb).unwrap();
        assert_eq!(strip, rect(1872, 0, 1877, 1080));
    }

    /// 细条必须完全位于任务栏矩形内部（嵌入不变量）
    #[test]
    fn bar_rect_strip_is_inside_taskbar() {
        for tb in [
            rect(0, 912, 1707, 960),
            rect(0, 0, 1920, 48),
            rect(0, 0, 48, 1080),
            rect(1872, 0, 1920, 1080),
        ] {
            let s = bar_rect(tb).unwrap();
            assert!(s.left >= tb.left && s.right <= tb.right);
            assert!(s.top >= tb.top && s.bottom <= tb.bottom);
        }
    }

    #[test]
    fn bar_rect_invalid_zero_size() {
        assert_eq!(bar_rect(rect(0, 0, 0, 0)), None);
    }

    #[test]
    fn bar_rect_invalid_inverted() {
        assert_eq!(bar_rect(rect(100, 100, 50, 200)), None);
    }

    #[test]
    fn pulse_gain_single_breath() {
        // 边界脉冲：0 → 峰值 → 0，仅在区间内为正（文档 §4.3）
        assert_eq!(pulse_gain(0.0), 0.0);
        assert!((pulse_gain(0.5) - PULSE_PEAK).abs() < 1e-9);
        assert!(pulse_gain(1.0).abs() < 1e-9);
        assert!(pulse_gain(0.25) > 0.0 && pulse_gain(0.25) < PULSE_PEAK);
        // 越界钳制，不产生负增益
        assert_eq!(pulse_gain(-0.5), 0.0);
        assert!(pulse_gain(1.5).abs() < 1e-9);
    }

    #[test]
    fn breath_gain_period_and_amplitude() {
        // 暂停呼吸：12s 周期、±10% 振幅的正弦（文档 §4.2）
        assert_eq!(breath_gain(0.0), 0.0);
        // 1/4 周期（3s）达到正峰值
        assert!((breath_gain(3.0) - BREATH_AMPLITUDE).abs() < 1e-9);
        // 半周期回到 0，3/4 周期达到负峰值
        assert!(breath_gain(6.0).abs() < 1e-9);
        assert!((breath_gain(9.0) + BREATH_AMPLITUDE).abs() < 1e-9);
        // 整周期回到 0
        assert!(breath_gain(12.0).abs() < 1e-9);
    }

    #[test]
    fn tick_centers_evenly_distributed() {
        // 4 枚刻点均匀分布：1/8、3/8、5/8、7/8（文档 §4.2）
        let w = 800;
        assert_eq!(tick_center_x(0, w), 100);
        assert_eq!(tick_center_x(1, w), 300);
        assert_eq!(tick_center_x(2, w), 500);
        assert_eq!(tick_center_x(3, w), 700);
    }
}
