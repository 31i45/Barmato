//! 托盘图标渲染模块（T 方案，文档 §6.2）
//!
//! 托盘图标是应用的唯一可视化表面：在系统声明的标准小图标尺寸画布上
//! 渲染 2×2 格阵，每格对应一个专注番茄，保留「四格 ≡ 四番茄」的语义同构。
//!
//! 视觉不变量（文档 §4.2）：
//! - **边框永存**：每格恒有 1px 轮廓——结构归结构、内容归内容，
//!   填充只发生在边框内部（inset），任何进度下格子结构都完整可辨
//! - 专注 N：格 1..N−1 内区实心专注色，第 N 格内区底部灌注专注色
//! - 休息 N：格 1..N 内区实心专注色（已获得），第 N 格内区灌注休息色
//! - 待机：恒为四格空心「待填充」轮廓——无论上一轮是否完整走完，
//!   视觉零特殊分支；完成信息走 tooltip 通道。完成瞬间边界脉冲自动闪光
//! - 暂停：整体降亮 50% + 12s 正弦呼吸 + 居中放大暂停符号 ❚❚
//!   （阶段色、恒满不透明——暂停态中唯一满亮度元素，任何背景可辨）
//! - 阶段边界：2.5s alpha 脉冲；色相切换：0.6s lerp
//!
//! 画布尺寸自适应（文档 §4.1）：创建渲染器时启动时只读一次任务栏厚度
//! （`Shell_TrayWnd` 矩形短边 − 双侧 4px 留白），取任务栏内容许的最大尺寸；
//! 查询失败/过小回退 `SM_CXSMICON`（已含 DPI 缩放）。几何参数（方格/间距/偏移）
//! 按画布比例推导，所有尺寸下格阵语义不变。
//!
//! 渲染为纯函数：`(Snapshot, Palette, Instant) → RGBA 像素缓冲 → Icon`。
//! 画布尺寸启动时只读一次任务栏厚度（FFI 隔离，失败回退 SM_CXSMICON），
//! 无持续几何跟踪、无像素缓冲复用——完全可单元测试。

use std::time::Instant;

use tray_icon::Icon;
use windows::core::w;
use windows::Win32::Foundation::{HWND, RECT};
use windows::Win32::UI::WindowsAndMessaging::{
    FindWindowW, GetSystemMetrics, GetWindowRect, SM_CXSMICON,
};

use crate::colors::{Palette, Rgb};
use crate::cycle::{Phase, Snapshot};

/// 画布最小边长：低于此值格阵无法绘制（方格 < 5px）
const MIN_SIZE: i32 = 16;
/// 任务栏厚度到画布的双侧留白（px）：给托盘图标周围留呼吸空间
const TASKBAR_MARGIN: i32 = 4;

/// 动画常量（文档 §4.3）
const TRANSITION_SECS: f64 = 0.6;
const PULSE_SECS: f64 = 2.5;
const PULSE_PEAK: f64 = 0.15;
const BREATH_PERIOD_SECS: f64 = 12.0;
const BREATH_AMPLITUDE: f64 = 0.10;
const PAUSE_DIM: f64 = 0.5;

/// 边界脉冲：0 → 峰值 → 0 的单次正弦呼吸，t ∈ [0,1]
fn pulse_gain(t: f64) -> f64 {
    PULSE_PEAK * (std::f64::consts::PI * t.clamp(0.0, 1.0)).sin()
}

/// 暂停呼吸：±10%、12s 周期的低频正弦
fn breath_gain(elapsed_secs: f64) -> f64 {
    BREATH_AMPLITUDE * (std::f64::consts::TAU * elapsed_secs / BREATH_PERIOD_SECS).sin()
}

/// 全局 alpha 合成：暂停降亮 × 呼吸 × 边界脉冲，钳制到 [0, 255]
fn alpha_at(paused: bool, breath_t: Option<f64>, pulse_t: Option<f64>) -> u8 {
    let dim = if paused { PAUSE_DIM } else { 1.0 };
    let breath = breath_t.map_or(0.0, breath_gain);
    let pulse = pulse_t.map_or(0.0, pulse_gain);
    let a = 255.0 * dim * (1.0 + breath) * (1.0 + pulse);
    a.round().clamp(0.0, 255.0) as u8
}

/// 由任务栏矩形推导画布边长（纯函数，FFI 隔离层之上）。
///
/// 取任务栏短边为厚度，减去双侧留白；过小则返回 None（由调用方回退）。
fn icon_size_from_taskbar(width: i32, height: i32) -> Option<i32> {
    let thickness = width.min(height);
    let size = thickness - TASKBAR_MARGIN * 2;
    if size < MIN_SIZE {
        None
    } else {
        Some(size)
    }
}

/// 探测画布尺寸（FFI 隔离层）：启动时只读一次任务栏厚度。
///
/// 优先用任务栏实际厚度（应用 UI 容器的物理尺寸），失败/过小时回退
/// 系统小图标标准尺寸 `SM_CXSMICON`（含 DPI 缩放）。
fn detect_icon_size() -> i32 {
    unsafe {
        if let Ok(hwnd) = FindWindowW(w!("Shell_TrayWnd"), None) {
            if hwnd != HWND::default() {
                let mut rect = RECT::default();
                if GetWindowRect(hwnd, &mut rect).is_ok() {
                    let width = rect.right - rect.left;
                    let height = rect.bottom - rect.top;
                    if let Some(size) = icon_size_from_taskbar(width, height) {
                        return size;
                    }
                }
            }
        }
        // 回退：系统小图标标准尺寸（已含 DPI 缩放）
        GetSystemMetrics(SM_CXSMICON).max(MIN_SIZE)
    }
}

/// 由画布边长推导格阵几何：(方格边长, 间距, 偏移)。
///
/// 比例设计（以 32px 标定）：方格 ≈ 边长/3，间距 ≈ 边长/16，
/// 格阵整体在画布内居中。s < 16 无法容纳可辨格阵 → None。
fn geometry(size: i32) -> Option<(i32, i32, i32)> {
    if size < MIN_SIZE {
        return None;
    }
    let sq = size / 3;
    let gap = (size / 16).max(1);
    let total = sq * 2 + gap;
    if total > size {
        return None;
    }
    Some((sq, gap, (size - total) / 2))
}

/// 单格视觉规格：base = 已完成底色（内区实心）；fill = 当前阶段灌注（色相, 高度比例）
#[derive(Clone, Copy, Default, PartialEq, Debug)]
struct SquareSpec {
    base: Option<Rgb>,
    fill: Option<(Rgb, f64)>,
}

/// 由快照推导 4 格视觉规格（纯函数，渲染层唯一的语义决策点）。
///
/// 当前格 = 第 pomodoro_index 格（1-based → 0-based 索引）：
/// - 专注 N：格 1..N−1 已有底色，第 N 格灌注专注色相
/// - 休息 N：格 1..N 已有底色（第 N 格是刚获得的番茄），第 N 格灌注休息色相
/// - 待机：恒为四格空心「待填充」——无论上一轮是否完整走完，视觉零特殊
///   分支；完成与否由 tooltip 通道表达（消除特殊场景，文档 §5）
fn plan(snap: Snapshot, palette: &Palette) -> [SquareSpec; 4] {
    let mut specs = [SquareSpec::default(); 4];
    if snap.idle {
        return specs;
    }
    if snap.pomodoro_index == 0 {
        return specs;
    }
    let idx = (snap.pomodoro_index - 1).min(3) as usize;
    let focus = palette.focus;
    let phase_color = palette.for_phase(snap.phase);
    for (i, s) in specs.iter_mut().enumerate() {
        if i < idx || (i == idx && snap.phase != Phase::Focus) {
            s.base = Some(focus);
        }
        if i == idx {
            s.fill = Some((phase_color, snap.phase_progress));
        }
    }
    specs
}

/// 在画布上设置单个像素（边界检查，RGBA 直写）
fn set_pixel(buf: &mut [u8], size: i32, x: i32, y: i32, color: Rgb, alpha: u8) {
    if x < 0 || x >= size || y < 0 || y >= size {
        return;
    }
    let i = ((y * size + x) * 4) as usize;
    buf[i] = color.r;
    buf[i + 1] = color.g;
    buf[i + 2] = color.b;
    buf[i + 3] = alpha;
}

/// 单格位置：画布上的原点与边长（几何参数打包，绘制函数共用）
#[derive(Clone, Copy)]
struct CellPos {
    x: i32,
    y: i32,
    sq: i32,
}

/// w 像素空心边框（结构层：任何格子恒有，文档 §4.2 边框永存不变量）。
/// w = 1 时与标定实现逐像素一致。
fn draw_outline(buf: &mut [u8], size: i32, c: CellPos, color: Rgb, alpha: u8, w: i32) {
    for dy in 0..c.sq {
        for dx in 0..c.sq {
            if dx < w || dx >= c.sq - w || dy < w || dy >= c.sq - w {
                set_pixel(buf, size, c.x + dx, c.y + dy, color, alpha);
            }
        }
    }
}

/// 内区实心填充（内容层：inset w，永不覆盖边框）
fn draw_solid(buf: &mut [u8], size: i32, c: CellPos, color: Rgb, alpha: u8, w: i32) {
    for dy in w..c.sq - w {
        for dx in w..c.sq - w {
            set_pixel(buf, size, c.x + dx, c.y + dy, color, alpha);
        }
    }
}

/// 内区底部灌注，高度 fh（自内区底边向上，文档 §4.2 灌注语义）
fn draw_fill(buf: &mut [u8], size: i32, c: CellPos, fh: i32, color: Rgb, alpha: u8, w: i32) {
    let inner = c.sq - 2 * w;
    let fh = fh.clamp(0, inner);
    let top = c.y + w + inner - fh;
    for dy in top..c.y + w + inner {
        for dx in w..c.sq - w {
            set_pixel(buf, size, c.x + dx, dy, color, alpha);
        }
    }
}

/// 生成 RGBA 像素缓冲（纯函数）：透明背景 + 2×2 格阵。
///
/// 绘制顺序：每格先画内容（base 整内区 → fill 底部灌注），再画 1px 边框——
/// 边框永存不变量的实现点。暂停态在格阵之上叠加居中暂停符号（❚❚）。
fn render_buf(
    size: i32,
    specs: &[SquareSpec; 4],
    outline: Rgb,
    fill_color: Rgb,
    alpha: u8,
    paused: bool,
    line_w: i32,
) -> Vec<u8> {
    let mut buf = vec![0u8; (size * size * 4) as usize];
    let Some((sq, gap, off)) = geometry(size) else {
        return buf; // 画布过小：全透明（构造方保证不发生）
    };
    let positions = [
        CellPos { x: off, y: off, sq }, // TL = 番茄 1
        CellPos {
            x: off + sq + gap,
            y: off,
            sq,
        }, // TR = 番茄 2
        CellPos {
            x: off,
            y: off + sq + gap,
            sq,
        }, // BL = 番茄 3
        CellPos {
            x: off + sq + gap,
            y: off + sq + gap,
            sq,
        }, // BR = 番茄 4
    ];
    for (spec, &c) in specs.iter().zip(positions.iter()) {
        // 内容层：底色 + 灌注（均限于内区）
        if let Some(color) = spec.base {
            draw_solid(&mut buf, size, c, color, alpha, line_w);
        }
        if let Some((_, frac)) = spec.fill {
            let inner = c.sq - 2 * line_w;
            // 最低 1px：启动瞬间即有可见反馈（进度 0 ≠ 无窗口）
            let fh = ((inner as f64) * frac.clamp(0.0, 1.0)).round().max(1.0) as i32;
            draw_fill(&mut buf, size, c, fh, fill_color, alpha, line_w);
        }
        // 结构层：边框永存（内容之后绘制，视觉上包住内容）
        draw_outline(&mut buf, size, c, outline, alpha, line_w);
    }
    // 暂停符号：格阵之上居中叠加 ❚❚——阶段色、恒满不透明，
    // 不参与降亮/呼吸，是暂停态中的对比锚点（非文字、通用暂停语义）
    if paused {
        draw_pause_glyph(&mut buf, size, fill_color, 255);
    }
    buf
}

/// 暂停符号：画布居中绘制两条等宽竖条（❚❚）。
///
/// 尺寸按画布比例放大到存在感优先：条宽 = size/10、条高 = size/2、
/// 条间距 = size/10。颜色取当前阶段色、恒满不透明——格阵整体降亮后，
/// 满亮度符号在任意任务栏主题下都是第一可读元素。
fn draw_pause_glyph(buf: &mut [u8], size: i32, color: Rgb, alpha: u8) {
    let bar_w = (size / 10).max(2);
    let bar_h = (size / 2).max(4);
    let gap = (size / 10).max(2);
    let total_w = bar_w * 2 + gap;
    let start_x = (size - total_w) / 2;
    let start_y = (size - bar_h) / 2;
    for dy in 0..bar_h {
        for dx in 0..bar_w {
            set_pixel(buf, size, start_x + dx, start_y + dy, color, alpha);
            set_pixel(
                buf,
                size,
                start_x + bar_w + gap + dx,
                start_y + dy,
                color,
                alpha,
            );
        }
    }
}

/// 应用图标（exe 资源）画布的 RGBA 像素：四格空心「待填充」。
///
/// 与托盘待机同一渲染核心、同一几何与语义；唯一差异是边线宽度随画布
/// 比例取整加粗（`brand_line_w`）——应用图标会在资源管理器等场景大幅
/// 放大显示，恒定 1px 在大尺寸下退化为不可见的发丝线。逐尺寸重推几何，
/// 加粗后边缘依然锐利无模糊（examples/gen_icon 使用）。
pub fn app_icon_rgba(size: i32, palette: &Palette) -> Vec<u8> {
    let specs = plan(Snapshot::idle(), palette);
    render_buf(
        size,
        &specs,
        palette.idle_outline(),
        palette.focus,
        255,
        false,
        brand_line_w(size),
    )
}

/// 应用图标边线宽：以 32px 档标定为 1px，随画布比例取整加粗
/// （16/24/32 → 1、48/64 → 2、128 → 4、256 → 8），
/// 任何尺寸下四格空心轮廓都完整显现。
fn brand_line_w(size: i32) -> i32 {
    ((size as f64) / 32.0).round().max(1.0) as i32
}

/// 托盘图标渲染器：封装全部动画状态与画布尺寸，对外只暴露 render()。
pub struct IconRenderer {
    size: i32,
    last_target: Option<Rgb>,
    prev_color: Rgb,
    transition_start: Option<Instant>,
    last_stage_seq: Option<u64>,
    pulse_start: Option<Instant>,
    pause_since: Option<Instant>,
}

impl IconRenderer {
    /// 按系统任务栏厚度创建画布（启动时只读一次；失败回退 SM_CXSMICON）
    pub fn new() -> Self {
        Self::with_size(detect_icon_size())
    }

    /// 指定画布尺寸创建（测试入口；运行时统一走 new()）
    pub fn with_size(size: i32) -> Self {
        Self {
            size: size.max(MIN_SIZE),
            last_target: None,
            prev_color: Rgb::new(0, 0, 0),
            transition_start: None,
            last_stage_seq: None,
            pulse_start: None,
            pause_since: None,
        }
    }

    /// 是否有进行中的动画需要持续重绘
    pub fn needs_continuous_render(&self) -> bool {
        self.transition_start.is_some() || self.pulse_start.is_some() || self.pause_since.is_some()
    }

    /// 是否需要高帧率（短动画：过渡 + 脉冲）
    pub fn needs_fast_frames(&self) -> bool {
        self.transition_start.is_some() || self.pulse_start.is_some()
    }

    /// 复位全部动画状态（退出/回归待机时调用）
    pub fn reset(&mut self) {
        self.last_target = None;
        self.transition_start = None;
        self.last_stage_seq = None;
        self.pulse_start = None;
        self.pause_since = None;
    }

    /// 按快照渲染一帧托盘图标。
    pub fn render(&mut self, snap: Snapshot, palette: &Palette, now: Instant) -> Icon {
        // 阶段边界 → 触发脉冲（完成瞬间也触发：待机徽章闪光一次）
        if self.last_stage_seq != Some(snap.stage_seq) {
            self.pulse_start = Some(now);
            self.last_stage_seq = Some(snap.stage_seq);
        }
        // 暂停呼吸
        if snap.paused {
            if self.pause_since.is_none() {
                self.pause_since = Some(now);
            }
        } else {
            self.pause_since = None;
        }

        // 灌注色相过渡插值（0.6s 无突变）
        let target = if snap.idle {
            palette.focus
        } else {
            palette.for_phase(snap.phase)
        };
        if self.last_target != Some(target) {
            if let Some(prev) = self.last_target {
                self.prev_color = prev;
                self.transition_start = Some(now);
            }
            self.last_target = Some(target);
        }
        let fill_color = match self.transition_start {
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

        // 脉冲生命周期结算
        if let Some(t0) = self.pulse_start {
            if now.duration_since(t0).as_secs_f64() >= PULSE_SECS {
                self.pulse_start = None;
            }
        }
        let pulse_t = self
            .pulse_start
            .map(|t0| now.duration_since(t0).as_secs_f64() / PULSE_SECS);
        let breath_t = self
            .pause_since
            .map(|t0| now.duration_since(t0).as_secs_f64());
        let alpha = alpha_at(snap.paused, breath_t, pulse_t);

        let specs = plan(snap, palette);
        let buf = render_buf(
            self.size,
            &specs,
            palette.idle_outline(),
            fill_color,
            alpha,
            snap.paused,
            1,
        );
        Icon::from_rgba(buf, self.size as u32, self.size as u32).expect("icon render")
    }
}

impl Default for IconRenderer {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn snap(idle: bool, phase: Phase, progress: f64, index: u8) -> Snapshot {
        Snapshot {
            idle,
            paused: false,
            phase,
            phase_progress: progress,
            pomodoro_index: index,
            stage_seq: 1,
            completed: false,
        }
    }

    fn snap_completed() -> Snapshot {
        Snapshot {
            idle: true,
            paused: false,
            phase: Phase::Focus,
            phase_progress: 0.0,
            pomodoro_index: 0,
            stage_seq: 9,
            completed: true,
        }
    }

    /// 读取缓冲中 (x, y) 处像素的 RGBA
    fn px(buf: &[u8], size: i32, x: i32, y: i32) -> (u8, u8, u8, u8) {
        let i = ((y * size + x) * 4) as usize;
        (buf[i], buf[i + 1], buf[i + 2], buf[i + 3])
    }

    // —— 几何推导 ——

    #[test]
    fn geometry_at_calibrated_sizes() {
        // 以 32px 标定：方格 10、间距 2、偏移 5（格阵 22px 居中）
        assert_eq!(geometry(32), Some((10, 2, 5)));
        // 24px（150% DPI 标准小图标）：方格 8、间距 1、偏移 3
        assert_eq!(geometry(24), Some((8, 1, 3)));
        // 16px 最小可用：方格 5、间距 1、偏移 2
        assert_eq!(geometry(16), Some((5, 1, 2)));
    }

    #[test]
    fn geometry_rejects_tiny_canvas() {
        assert_eq!(geometry(15), None);
        assert_eq!(geometry(8), None);
        assert_eq!(geometry(0), None);
        assert_eq!(geometry(-1), None);
    }

    /// 任务栏厚度推导画布：短边 − 双侧 8px 留白；过小返回 None（回退 SM_CXSMICON）
    #[test]
    fn taskbar_size_deduction() {
        // Win11@100% 任务栏 48×1920 → 厚 48 → 画布 40
        assert_eq!(icon_size_from_taskbar(1920, 48), Some(40));
        // Win11@150% 任务栏 72×2560 → 厚 72 → 画布 64
        assert_eq!(icon_size_from_taskbar(2560, 72), Some(64));
        // 侧边任务栏 1920×64 → 厚 64 → 画布 56
        assert_eq!(icon_size_from_taskbar(64, 1920), Some(56));
        // 厚度过小 → None（回退 SM_CXSMICON）
        assert_eq!(icon_size_from_taskbar(1920, 20), None); // 20-8=12 < 16
        assert_eq!(icon_size_from_taskbar(10, 10), None);
    }

    // —— plan 语义 ——

    #[test]
    fn plan_idle_all_empty() {
        let specs = plan(Snapshot::idle(), &Palette::defaults());
        assert!(specs.iter().all(|s| s.base.is_none() && s.fill.is_none()));
    }

    #[test]
    fn plan_idle_completed_is_still_hollow() {
        // 完成与否不改变待机视觉（零特殊分支）：完成信息走 tooltip 通道
        let p = Palette::defaults();
        assert_eq!(plan(snap_completed(), &p), plan(Snapshot::idle(), &p));
    }

    #[test]
    fn plan_focus1_fills_first_square() {
        let p = Palette::defaults();
        let specs = plan(snap(false, Phase::Focus, 0.5, 1), &p);
        assert_eq!(specs[0].fill, Some((p.focus, 0.5)));
        assert!(specs[0].base.is_none());
        assert!(specs[1..]
            .iter()
            .all(|s| s.base.is_none() && s.fill.is_none()));
    }

    #[test]
    fn plan_break1_base_and_fill() {
        let p = Palette::defaults();
        let specs = plan(snap(false, Phase::ShortBreak, 0.3, 1), &p);
        assert_eq!(specs[0].base, Some(p.focus));
        assert_eq!(specs[0].fill, Some((p.short_break, 0.3)));
        assert!(specs[1..]
            .iter()
            .all(|s| s.base.is_none() && s.fill.is_none()));
    }

    #[test]
    fn plan_focus4_three_bases() {
        let p = Palette::defaults();
        let specs = plan(snap(false, Phase::Focus, 0.1, 4), &p);
        for s in &specs[..3] {
            assert_eq!(s.base, Some(p.focus));
            assert!(s.fill.is_none());
        }
        assert_eq!(specs[3].fill, Some((p.focus, 0.1)));
        assert!(specs[3].base.is_none());
    }

    #[test]
    fn plan_long_break_all_bases() {
        let p = Palette::defaults();
        let specs = plan(snap(false, Phase::LongBreak, 0.6, 4), &p);
        for s in &specs {
            assert_eq!(s.base, Some(p.focus));
        }
        assert_eq!(specs[3].fill, Some((p.long_break, 0.6)));
    }

    // —— 像素级不变量 ——

    /// 边框永存：满格填充时边框像素仍为轮廓色、内区为专注色
    #[test]
    fn border_persists_when_filled() {
        let size = 32;
        let (sq, _gap, off) = geometry(size).unwrap();
        let p = Palette::defaults();
        let specs = plan(snap(false, Phase::Focus, 1.0, 1), &p);
        let buf = render_buf(size, &specs, p.idle_outline(), p.focus, 255, false, 1);
        // TL 格四角与边中点：全部是轮廓色
        for (x, y) in [
            (off, off),
            (off + sq - 1, off),
            (off, off + sq - 1),
            (off + sq - 1, off + sq - 1),
            (off + sq / 2, off),
        ] {
            assert_eq!(
                px(&buf, size, x, y).0,
                p.idle_outline().r,
                "border at {x},{y}"
            );
        }
        // 内区中心：专注色
        let c = px(&buf, size, off + sq / 2, off + sq / 2);
        assert_eq!((c.0, c.1, c.2), (p.focus.r, p.focus.g, p.focus.b));
    }

    /// 空心格：只有边框，内区全透明
    #[test]
    fn empty_square_hollow() {
        let size = 32;
        let (sq, _gap, off) = geometry(size).unwrap();
        let p = Palette::defaults();
        let buf = render_buf(
            size,
            &[SquareSpec::default(); 4],
            p.idle_outline(),
            p.focus,
            255,
            false,
            1,
        );
        // BR 格边框存在
        assert_eq!(
            px(&buf, size, off + sq + 2, off + sq + 2).0,
            p.idle_outline().r
        );
        // BR 格内区透明
        let c = px(&buf, size, off + sq + 2 + sq / 2, off + sq + 2 + sq / 2);
        assert_eq!(c, (0, 0, 0, 0));
    }

    /// 灌注高度按进度精确：50% → 内区下半填充、上半透明
    #[test]
    fn fill_height_follows_progress() {
        let size = 32;
        let (sq, _gap, off) = geometry(size).unwrap();
        let p = Palette::defaults();
        let specs = plan(snap(false, Phase::Focus, 0.5, 1), &p);
        let buf = render_buf(size, &specs, p.idle_outline(), p.focus, 255, false, 1);
        let inner_top = off + 1;
        let inner_bottom = off + sq - 2; // 内区最后一行
                                         // 内区中部偏上（第 3 行 / 共 8 行）应透明
        let mid_up = px(&buf, size, off + sq / 2, inner_top + 2);
        assert_eq!(mid_up, (0, 0, 0, 0));
        // 内区底部一行应有填充
        let bottom = px(&buf, size, off + sq / 2, inner_bottom);
        assert_eq!(bottom.0, p.focus.r);
        // 50% → 填充 4 行：内区第 5..8 行有色（bottom-3 也应有色）
        let near_bottom = px(&buf, size, off + sq / 2, inner_bottom - 3);
        assert_eq!(near_bottom.0, p.focus.r);
    }

    /// 本轮走完后的待机与普通待机同构：四格空心待填充（像素级验证）
    #[test]
    fn completed_idle_renders_hollow() {
        let size = 32;
        let (sq, _gap, off) = geometry(size).unwrap();
        let p = Palette::defaults();
        let buf = render_buf(
            size,
            &plan(snap_completed(), &p),
            p.idle_outline(),
            p.focus,
            255,
            false,
            1,
        );
        // TL 格中心透明（空心）
        assert_eq!(px(&buf, size, off + sq / 2, off + sq / 2), (0, 0, 0, 0));
    }

    /// 画布过小时返回全透明缓冲（防御路径）
    #[test]
    fn tiny_canvas_renders_transparent() {
        let buf = render_buf(
            8,
            &[SquareSpec::default(); 4],
            Rgb::new(0, 0, 0),
            Rgb::new(0, 0, 0),
            255,
            false,
            1,
        );
        assert!(buf.iter().all(|&b| b == 0));
    }

    /// 多尺寸渲染不越界、缓冲长度精确（16/24/32/48）
    #[test]
    fn render_multi_sizes_in_bounds() {
        let p = Palette::defaults();
        for size in [16, 24, 32, 48] {
            let specs = plan(snap(false, Phase::Focus, 0.5, 1), &p);
            let buf = render_buf(size, &specs, p.idle_outline(), p.focus, 255, false, 1);
            assert_eq!(buf.len(), (size * size * 4) as usize);
            // 四角必透明（格阵居中，不会触及画布角）
            for (x, y) in [(0, 0), (size - 1, 0), (0, size - 1), (size - 1, size - 1)] {
                assert_eq!(
                    px(&buf, size, x, y),
                    (0, 0, 0, 0),
                    "size {size} corner {x},{y}"
                );
            }
        }
    }

    /// 暂停符号：暂停态居中出现阶段色满不透明双竖条；非暂停态无
    #[test]
    fn pause_glyph_present_when_paused() {
        let size = 32;
        let p = Palette::defaults();
        let specs = plan(snap(false, Phase::Focus, 0.5, 1), &p);
        // 左棒上一点（几何：bar_w=3, bar_h=16, gap=3 → start_x=11, start_y=8）
        let glyph_x = 12;
        let glyph_y = 16;
        // 暂停态：格阵以 alpha=128 降亮传入，符号仍恒满不透明、取阶段色
        let buf_paused = render_buf(size, &specs, p.idle_outline(), p.focus, 128, true, 1);
        let c = px(&buf_paused, size, glyph_x, glyph_y);
        assert_eq!((c.0, c.1, c.2), (p.focus.r, p.focus.g, p.focus.b));
        assert_eq!(c.3, 255, "glyph 恒满不透明，不随格阵降亮");
        // 非暂停态：同一位置透明（格阵间隙，无符号）
        let buf_running = render_buf(size, &specs, p.idle_outline(), p.focus, 255, false, 1);
        assert_eq!(
            px(&buf_running, size, glyph_x, glyph_y),
            (0, 0, 0, 0),
            "no glyph when running"
        );
    }

    /// 应用图标 32px 标定档与托盘待机逐像素一致（同核同语义，边线宽同为 1px）
    #[test]
    fn app_icon_matches_tray_idle_at_32px() {
        let p = Palette::defaults();
        assert_eq!(
            app_icon_rgba(32, &p),
            render_buf(
                32,
                &plan(Snapshot::idle(), &p),
                p.idle_outline(),
                p.focus,
                255,
                false,
                1
            )
        );
    }

    /// 大尺寸应用图标：边线按比例取整加粗（256px → 8px）且四格保持空心
    #[test]
    fn app_icon_bold_outline_stays_hollow() {
        assert_eq!(brand_line_w(16), 1);
        assert_eq!(brand_line_w(32), 1);
        assert_eq!(brand_line_w(48), 2);
        assert_eq!(brand_line_w(64), 2);
        assert_eq!(brand_line_w(128), 4);
        let size = 256;
        let (sq, gap, off) = geometry(size).unwrap();
        let p = Palette::defaults();
        let buf = app_icon_rgba(size, &p);
        assert_eq!(brand_line_w(size), 8);
        // 顶边框带内采样 = 轮廓色（恒 1px 时代该点位于透明内区）
        assert_eq!(px(&buf, size, off + sq / 2, off + 4).0, p.idle_outline().r);
        // 四格内区中心全部透明——空心「待填充」语义在任何尺寸下不变
        for (cx, cy) in [
            (off + sq / 2, off + sq / 2),
            (off + sq + gap + sq / 2, off + sq / 2),
            (off + sq / 2, off + sq + gap + sq / 2),
            (off + sq + gap + sq / 2, off + sq + gap + sq / 2),
        ] {
            assert_eq!(px(&buf, size, cx, cy), (0, 0, 0, 0), "hollow at {cx},{cy}");
        }
    }

    // —— 动画数学 ——

    #[test]
    fn alpha_normal_is_full() {
        assert_eq!(alpha_at(false, None, None), 255);
    }

    #[test]
    fn alpha_paused_dimmed() {
        assert_eq!(alpha_at(true, Some(0.0), None), 128); // 255×0.5 四舍五入
    }

    #[test]
    fn alpha_pulse_boosts() {
        let boosted = alpha_at(false, None, Some(0.5));
        assert_eq!(boosted, (255.0_f64 * 1.15).round() as u8);
        assert_eq!(alpha_at(false, None, Some(0.0)), 255);
    }

    #[test]
    fn pulse_gain_single_breath() {
        assert_eq!(pulse_gain(0.0), 0.0);
        assert!((pulse_gain(0.5) - PULSE_PEAK).abs() < 1e-9);
        assert!(pulse_gain(1.0).abs() < 1e-9);
    }

    #[test]
    fn breath_gain_period_and_amplitude() {
        assert_eq!(breath_gain(0.0), 0.0);
        assert!((breath_gain(3.0) - BREATH_AMPLITUDE).abs() < 1e-9);
        assert!(breath_gain(6.0).abs() < 1e-9);
        assert!((breath_gain(9.0) + BREATH_AMPLITUDE).abs() < 1e-9);
        assert!(breath_gain(12.0).abs() < 1e-9);
    }

    // —— 渲染器生命周期 ——

    #[test]
    fn with_size_clamps_minimum() {
        assert_eq!(IconRenderer::with_size(8).size, MIN_SIZE);
        assert_eq!(IconRenderer::with_size(32).size, 32);
    }

    #[test]
    fn idle_renders_without_fill() {
        let mut r = IconRenderer::with_size(32);
        let _ = r.render(Snapshot::idle(), &Palette::defaults(), Instant::now());
        // 不 panic 即通过
    }

    #[test]
    fn reset_clears_animation() {
        let mut r = IconRenderer::with_size(32);
        let _ = r.render(
            snap(false, Phase::Focus, 0.5, 1),
            &Palette::defaults(),
            Instant::now(),
        );
        r.reset();
        assert!(!r.needs_continuous_render());
        assert!(!r.needs_fast_frames());
    }

    /// 连续渲染不 panic（过渡 + 脉冲 + 呼吸 + 完成收尾全路径）
    #[test]
    fn continuous_render_no_panic() {
        let mut r = IconRenderer::with_size(24);
        let t0 = Instant::now();
        let p = Palette::defaults();
        // 模拟一个完整周期：F1→SB1→…→F4→LB→idle(完成)
        let phases = [
            (false, Phase::Focus, 0.0, 1),
            (false, Phase::Focus, 0.5, 1),
            (false, Phase::ShortBreak, 0.0, 1),
            (false, Phase::ShortBreak, 0.5, 1),
            (false, Phase::Focus, 0.0, 2),
            (false, Phase::Focus, 0.5, 2),
            (false, Phase::ShortBreak, 0.0, 2),
            (false, Phase::Focus, 0.0, 3),
            (false, Phase::Focus, 0.5, 3),
            (false, Phase::ShortBreak, 0.0, 3),
            (false, Phase::Focus, 0.0, 4),
            (false, Phase::Focus, 0.5, 4),
            (false, Phase::LongBreak, 0.0, 4),
            (false, Phase::LongBreak, 0.5, 4),
            (true, Phase::Focus, 0.0, 0), // idle + 完成标记
        ];
        for (i, (idle, phase, prog, idx)) in phases.iter().enumerate() {
            let s = Snapshot {
                idle: *idle,
                paused: i == 5, // 暂停一次
                phase: *phase,
                phase_progress: *prog,
                pomodoro_index: *idx,
                stage_seq: i as u64 + 1,
                completed: *idle, // 结尾待机携带完成标记
            };
            let _ = r.render(s, &p, t0 + std::time::Duration::from_millis(i as u64 * 250));
        }
    }
}
