//! Barmato 薄主线（文档 §6：深模块、浅接口）
//!
//! 本文件只做粘合：单实例守卫 → 加载配置 → 隐藏宿主窗口 + 消息循环
//! → 托盘图标（左键启停 / 右键退出）→ 定时驱动周期状态机 → 按快照渲染。
//! 全部系统复杂度封装在各深模块内部。
//!
//! 职责分离不变量（文档 §3.1/§4.1/§4.2）：
//! **首选渲染 = 任务栏整体染色（色相 = 阶段，慢变量）；嵌入细条 = 阶段进度 + 4 刻点（快变量）；
//! 染色不可用则静默降级为纯细条，再降级为透明。交互入口为托盘图标左键，
//! 托盘图标同步镜像当前状态（阶段色相/暂停空心/待机暗灰）**。

#![windows_subsystem = "windows"]

mod colors;
mod config;
mod cycle;
mod overlay;
mod taskbar;
mod tint;

use std::time::Instant;

use cycle::{CycleMachine, Snapshot};
use overlay::Overlay;
use taskbar::Taskbar;

use tray_icon::menu::{Menu, MenuEvent, MenuId, MenuItem};
use tray_icon::{Icon, MouseButton, MouseButtonState, TrayIconBuilder, TrayIconEvent};
use windows::core::w;
use windows::Win32::Foundation::{
    GetLastError, ERROR_ALREADY_EXISTS, HWND, LPARAM, LRESULT, WPARAM,
};
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::System::Threading::CreateMutexW;
use windows::Win32::UI::HiDpi::{
    SetProcessDpiAwarenessContext, DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2,
};
use windows::Win32::UI::WindowsAndMessaging::*;

const TIMER_ID: usize = 1;
/// 常规心跳：驱动计时与进度（文档 §6.1）
const TICK_MS: u32 = 250;
/// 阶段过渡期心跳：提高帧率使色彩插值平滑（文档 §4.3）
const TICK_FAST_MS: u32 = 50;
/// 运行时每 1 秒静默校验一次任务栏几何（位置/尺寸/DPI/资源管理器变动，文档 §6.2 / §7）
const GEOMETRY_TICKS: u64 = 4;
/// 待机无渲染需求，几何探测降频至约 10 秒一次，极致轻量（文档 §4.3）
const GEOMETRY_TICKS_IDLE: u64 = 40;

/// 一条任务栏与其渲染器一一对应（默认仅主屏，文档 §4.1）
struct Bar {
    taskbar: Taskbar,
    overlay: Option<Overlay>,
    /// 染色可用性（文档 §4.1 降级阶梯）：None=未探测，Some(true)=染色生效，
    /// Some(false)=平台不支持已永久降级为纯细条（仅几何重建才重探测）
    tint_ok: Option<bool>,
    /// 当前已染上的颜色（None=原生外观）：染色是慢变量，仅阶段/主题/几何
    /// 变化才变色，同色重设徒增 DWM 重合成抖动（§4.3 无闪烁）——按色去重
    tint_applied: Option<colors::Rgb>,
}

struct App {
    machine: CycleMachine,
    config: config::AppConfig,
    bars: Vec<Bar>,
    tray: tray_icon::TrayIcon,
    /// 已落到托盘的图标状态（色相, 是否空心；待机=中性暗灰），仅在变化时重建
    last_icon: Option<(colors::Rgb, bool)>,
    last_snap: Snapshot,
    last_tick: Instant,
    tick_count: u64,
    geometry_dirty: bool,
    /// 系统任务栏主题（浅色=黑字）：决定染色亮度策略（文档 §4.1 染色自适应）
    taskbar_light: bool,
    taskbar_created_msg: u32,
    /// 当前定时器间隔；仅在变化时才调用 SetTimer，避免重复重置丢帧
    timer_interval: u32,
}

impl App {
    fn new(cfg: config::AppConfig, taskbar_created_msg: u32, tray: tray_icon::TrayIcon) -> App {
        let machine = CycleMachine::new(cfg.focus, cfg.short_break, cfg.long_break);
        let bars: Vec<Bar> = Taskbar::refresh_all(cfg.render_all_monitors)
            .into_iter()
            .map(|taskbar| Bar {
                taskbar,
                overlay: None,
                tint_ok: None,
                tint_applied: None,
            })
            .collect();
        // 启动自愈：清理上次进程异常退出可能残留的染色（文档 §7 静默修复）
        for b in &bars {
            tint::apply(b.taskbar.hwnd, None);
        }
        App {
            machine,
            config: cfg,
            bars,
            tray,
            last_icon: None,
            last_snap: Snapshot::idle(),
            last_tick: Instant::now(),
            tick_count: 0,
            geometry_dirty: false,
            taskbar_light: tint::system_taskbar_is_light(),
            taskbar_created_msg,
            timer_interval: TICK_MS,
        }
    }

    /// 定时心跳：推进计时 → 校验几何 → 渲染 → 按过渡状态调整帧率
    fn on_timer(&mut self, hwnd: HWND) {
        let now = Instant::now();
        let delta = now.duration_since(self.last_tick);
        self.last_tick = now;
        self.machine.tick(delta);

        let snap = self.machine.snapshot();
        // 待机极致轻量（文档 §4.3）：无渲染需求时几何探测降频至约 10 秒一次
        self.tick_count += 1;
        let geo_period = if snap.idle {
            GEOMETRY_TICKS_IDLE
        } else {
            GEOMETRY_TICKS
        };
        if self.geometry_dirty || self.tick_count.is_multiple_of(geo_period) {
            self.refresh_geometry();
        }

        // 仅在快照变化或有进行中动画（过渡/脉冲/呼吸）时重绘：
        // 暂停且呼吸平静期以外不制造无谓重绘
        let animating = self.bars.iter().any(|b| {
            b.overlay
                .as_ref()
                .is_some_and(Overlay::needs_continuous_render)
        });
        if snap != self.last_snap || animating {
            self.apply(snap, now);
        }
        self.last_snap = snap;

        // 过渡/脉冲等短动画提高帧率保证平滑（文档 §4.3）；12s 慢呼吸用常规帧率即可。
        // 仅在间隔实际变化时才调用 SetTimer，避免重复重置导致丢帧
        let fast = self
            .bars
            .iter()
            .any(|b| b.overlay.as_ref().is_some_and(Overlay::needs_fast_frames));
        let interval = if fast { TICK_FAST_MS } else { TICK_MS };
        if interval != self.timer_interval {
            self.timer_interval = interval;
            unsafe {
                SetTimer(hwnd, TIMER_ID, interval, None);
            }
        }
    }

    /// 唯一用户交互（文档 §3.1）：托盘图标左键点击触发状态切换。
    /// 右键点击弹出菜单（仅退出），由 tray-icon crate 自行处理，不进入此方法。
    fn on_toggle(&mut self) {
        let now = Instant::now();
        self.machine.toggle();
        let snap = self.machine.snapshot();
        self.apply(snap, now);
        self.last_snap = snap;
    }

    /// 重新探测任务栏几何（文档 §6.2 / §7）。
    /// 仅当任务栏句柄/矩形变化时才重建对应覆盖层。
    fn refresh_geometry(&mut self) {
        let new_tbs = Taskbar::refresh_all(self.config.render_all_monitors);
        let mut new_bars: Vec<Bar> = Vec::with_capacity(new_tbs.len());
        for tb in new_tbs {
            // 按句柄匹配旧覆盖层：句柄与矩形均未变则复用，否则重建；
            // 句柄变化（任务栏重建）时染色探测也随之重置，下帧重新尝试染色
            let overlay = self
                .bars
                .iter_mut()
                .find(|b| b.taskbar.hwnd == tb.hwnd && rect_eq(&b.taskbar.rect, &tb.rect))
                .and_then(|b| b.overlay.take());
            let tint_state = self
                .bars
                .iter()
                .find(|b| b.taskbar.hwnd == tb.hwnd && rect_eq(&b.taskbar.rect, &tb.rect))
                .map_or((None, None), |b| (b.tint_ok, b.tint_applied));
            new_bars.push(Bar {
                taskbar: tb,
                overlay,
                tint_ok: tint_state.0,
                tint_applied: tint_state.1,
            });
        }
        self.bars = new_bars;
        self.geometry_dirty = false;
    }

    /// 把状态快照落到任务栏视觉与托盘图标上（文档 §4.2 / §4.3）
    fn apply(&mut self, snap: Snapshot, now: Instant) {
        // 托盘镜像（文档 §4.2）：运行=阶段色相实心圆点，暂停=空心圆环，待机=中性暗灰
        let icon_state = if snap.idle {
            (colors::IDLE_DOT, false)
        } else {
            (self.config.palette.for_phase(snap.phase), snap.paused)
        };
        if self.last_icon != Some(icon_state) {
            let _ = self
                .tray
                .set_icon(Some(make_icon(icon_state.0, icon_state.1)));
            self.last_icon = Some(icon_state);
        }
        // 待机：完全停止绘制并恢复任务栏原生外观（文档 §4.1/§4.3）
        if snap.idle {
            for b in self.bars.iter_mut() {
                if b.tint_applied.is_some() {
                    tint::apply(b.taskbar.hwnd, None);
                    b.tint_applied = None;
                }
                b.tint_ok = None;
                if let Some(o) = b.overlay.as_mut() {
                    o.hide();
                }
            }
            return;
        }
        let target = self.config.palette.for_phase(snap.phase);
        // 染色目标（文档 §4.1）：阶段色相经系统文字对比度自适应钳制（慢变量通道）
        let tint_color = colors::ensure_contrast(target, self.taskbar_light);
        for b in self.bars.iter_mut() {
            // 首选策略 D：任务栏整体染色，仅在目标色变化时调用 FFI（阶段切换/
            // 主题切换/几何重建才会变色）。探测失败（Some(false)）→ 永久降级为
            // 纯细条（C），不逐帧重试 FFI；仅几何重建（含资源管理器重启）才重置
            if b.tint_ok != Some(false) && b.tint_applied != Some(tint_color) {
                if tint::apply(b.taskbar.hwnd, Some(tint_color)) {
                    b.tint_ok = Some(true);
                    b.tint_applied = Some(tint_color);
                } else {
                    tint::apply(b.taskbar.hwnd, None);
                    b.tint_ok = Some(false);
                    b.tint_applied = None;
                }
            }
            // 细条（快变量通道）：进度填充 + 刻点 + 边界脉冲 + 暂停呼吸，
            // 染色生效时底色须与任务栏染色一致（§4.2 细条同色底色），
            // 染色降级时用原始阶段色独立承担全部视觉（最大可见度）
            let strip_color = if b.tint_applied.is_some() {
                tint_color
            } else {
                target
            };
            let need_new = match b.overlay.as_ref() {
                None => true,
                Some(o) => !o.is_alive(),
            };
            if need_new {
                b.overlay = Overlay::new(b.taskbar.hwnd);
            }
            let Some(o) = b.overlay.as_mut() else {
                continue; // 创建失败：静默降级，无渲染但不报错
            };
            // 渲染失败被降级隐藏后，这里自动重新显示恢复（文档 §7 静默恢复）
            if !o.is_visible() {
                o.show();
            }
            o.render(snap, strip_color, now);
        }
    }

    /// 退出前恢复任务栏原生外观（文档 §3.2：退出即放弃周期；§7 不留残迹）
    fn restore_all_tints(&mut self) {
        for b in &mut self.bars {
            if b.tint_applied.is_some() {
                tint::apply(b.taskbar.hwnd, None);
                b.tint_applied = None;
            }
            b.tint_ok = None;
        }
    }
}

fn rect_eq(a: &windows::Win32::Foundation::RECT, b: &windows::Win32::Foundation::RECT) -> bool {
    a.left == b.left && a.top == b.top && a.right == b.right && a.bottom == b.bottom
}

fn main() {
    unsafe {
        let _ = SetProcessDpiAwarenessContext(DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2);
    }

    // 单实例守卫（文档 §3.2：重启即回归待机）
    // GetLastError 必须在 CreateMutexW 后立即检查——
    // 中间任何 Win32/IO 调用都会覆盖线程的 LastError 值
    let Ok(_mutex) = (unsafe { CreateMutexW(None, false, w!("Barmato.SingleInstance")) }) else {
        return;
    };
    if unsafe { GetLastError() } == ERROR_ALREADY_EXISTS {
        return;
    }

    // 配置：缺失/损坏静默回退默认（文档 §6.5）
    let cfg = config::load();

    let instance = unsafe { GetModuleHandleW(None) }.unwrap_or_default();

    // 隐藏宿主窗口：仅作为消息循环载体，用户永不可见
    let class = w!("BarmatoHost");
    let wc = WNDCLASSW {
        lpfnWndProc: Some(wnd_proc),
        hInstance: instance.into(),
        lpszClassName: class,
        ..Default::default()
    };
    unsafe {
        RegisterClassW(&wc);
    }
    let Ok(hwnd) = (unsafe {
        CreateWindowExW(
            WINDOW_EX_STYLE(0),
            class,
            None,
            WINDOW_STYLE(0),
            0,
            0,
            0,
            0,
            None,
            None,
            instance,
            None,
        )
    }) else {
        return;
    };

    // 资源管理器重启广播（文档 §7）
    let taskbar_created_msg = unsafe { RegisterWindowMessageW(w!("TaskbarCreated")) };

    // 托盘：左键 = 启停切换，右键 = 菜单（仅退出）（文档 §3.1/§3.2）
    // 图标同时承担状态镜像（文档 §4.2）：初始待机 = 中性暗灰圆点
    let menu = Menu::new();
    let exit_item = MenuItem::with_id(MenuId::new("barmato.exit"), "退出", true, None);
    let exit_id = exit_item.id();
    let _ = menu.append(&exit_item);
    let Ok(tray) = TrayIconBuilder::new()
        .with_menu(Box::new(menu))
        .with_icon(make_icon(colors::IDLE_DOT, false))
        .with_tooltip("Barmato - 左键启停 / 右键退出")
        .with_title("Barmato")
        .with_menu_on_left_click(false) // 左键不弹菜单，专用于 toggle
        .build()
    else {
        return;
    };

    let mut app = App::new(cfg, taskbar_created_msg, tray);
    // 上下文挂载到窗口自身（Win32 正统做法），无全局可变状态
    unsafe {
        SetWindowLongPtrW(hwnd, GWLP_USERDATA, &mut app as *mut App as isize);
    }

    unsafe {
        SetTimer(hwnd, TIMER_ID, TICK_MS, None);
    }

    let mut msg = MSG::default();
    unsafe {
        while GetMessageW(&mut msg, None, 0, 0).as_bool() {
            // 菜单事件：退出
            if let Ok(ev) = MenuEvent::receiver().try_recv() {
                if ev.id() == exit_id {
                    // 退出即不留残迹（文档 §3.2/§7）：先恢复任务栏原生外观
                    let app = &mut *(GetWindowLongPtrW(hwnd, GWLP_USERDATA) as *mut App);
                    app.restore_all_tints();
                    PostQuitMessage(0);
                    // 跳过本帧 DispatchMessageW——若 msg 是 WM_TIMER，
                    // on_timer→apply 会重新染色，覆盖刚恢复的原生外观
                    continue;
                }
            }
            // 托盘图标事件：左键 Down → toggle
            if let Ok(TrayIconEvent::Click {
                button: MouseButton::Left,
                button_state: MouseButtonState::Down,
                ..
            }) = TrayIconEvent::receiver().try_recv()
            {
                let app = &mut *(GetWindowLongPtrW(hwnd, GWLP_USERDATA) as *mut App);
                app.on_toggle();
            }
            let _ = TranslateMessage(&msg);
            DispatchMessageW(&msg);
        }
        let _ = KillTimer(hwnd, TIMER_ID);
        let _ = SetWindowLongPtrW(hwnd, GWLP_USERDATA, 0);
    }
}

unsafe extern "system" fn wnd_proc(
    hwnd: HWND,
    msg: u32,
    wparam: WPARAM,
    lparam: LPARAM,
) -> LRESULT {
    let ptr = unsafe { GetWindowLongPtrW(hwnd, GWLP_USERDATA) };
    if ptr == 0 {
        return DefWindowProcW(hwnd, msg, wparam, lparam);
    }
    let app = &mut *(ptr as *mut App);
    match msg {
        WM_TIMER if wparam.0 == TIMER_ID => {
            app.on_timer(hwnd);
            LRESULT(0)
        }
        WM_DISPLAYCHANGE | WM_SETTINGCHANGE => {
            // 几何可能变化（显示模式/DPI/任务栏重排）；系统主题切换亦经
            // WM_SETTINGCHANGE 广播 → 重读任务栏主题，并把上次快照归零，
            // 强制下帧以新主题重应用染色（待机本无染色，快照相等自然跳过）
            app.geometry_dirty = true;
            app.taskbar_light = tint::system_taskbar_is_light();
            app.last_snap = Snapshot::idle();
            DefWindowProcW(hwnd, msg, wparam, lparam)
        }
        m if m != 0 && m == app.taskbar_created_msg => {
            // 资源管理器重启：强制重建任务栏几何与覆盖层
            app.geometry_dirty = true;
            app.refresh_geometry();
            LRESULT(0)
        }
        WM_DESTROY => {
            // 宿主窗口被销毁（含系统关机路径）同样恢复任务栏原生外观
            app.restore_all_tints();
            PostQuitMessage(0);
            LRESULT(0)
        }
        _ => DefWindowProcW(hwnd, msg, wparam, lparam),
    }
}

/// 程序生成 32×32 托盘图标（文档 §4.2 托盘镜像），无外部资源依赖：
/// - 实心 = 运行（色相随阶段）；空心圆环 = 暂停；中性暗灰 = 待机
/// - 图标色相对输入色提亮 20%，保证在深色/浅色托盘背景上均可辨
fn make_icon(color: colors::Rgb, hollow: bool) -> Icon {
    let size = 32u32;
    let mut rgba = vec![0u8; (size * size * 4) as usize];
    let c = color.lighten(0.2);
    let (center, r_out) = (15.5_f64, 13.0_f64);
    // 空心圆环的内半径；实心时内半径为 0
    let r_in = if hollow { r_out - 4.0 } else { 0.0 };
    for y in 0..size {
        for x in 0..size {
            let (dx, dy) = (x as f64 - center, y as f64 - center);
            let d2 = dx * dx + dy * dy;
            if d2 <= r_out * r_out && d2 >= r_in * r_in {
                let i = ((y * size + x) * 4) as usize;
                rgba[i] = c.r;
                rgba[i + 1] = c.g;
                rgba[i + 2] = c.b;
                rgba[i + 3] = 255;
            }
        }
    }
    Icon::from_rgba(rgba, size, size).expect("tray icon")
}
