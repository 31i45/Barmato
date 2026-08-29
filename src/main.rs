//! Barmato 薄主线（文档 §6：深模块、浅接口）
//!
//! 本文件只做粘合：单实例守卫 → 加载配置 → 隐藏宿主窗口 + 消息循环
//! → 托盘图标（左键启停 / 右键退出）→ 定时驱动周期状态机 → 按快照渲染。
//! 全部系统复杂度封装在各深模块内部。
//!
//! 职责分离不变量（文档 §3.1/§4.2）：
//! **渲染覆盖整条任务栏宽度，细条整条代表一个完整番茄周期（8 阶段）；
//! 交互入口为托盘图标左键，稳定可靠、不受任务栏布局影响**。

#![windows_subsystem = "windows"]

mod colors;
mod config;
mod cycle;
mod overlay;
mod taskbar;

use std::time::Instant;

use cycle::{CycleMachine, Snapshot};
use overlay::Overlay;
use taskbar::Taskbar;

use tray_icon::menu::{Menu, MenuEvent, MenuId, MenuItem};
use tray_icon::{Icon, MouseButton, MouseButtonState, TrayIconBuilder, TrayIconEvent};
use windows::core::w;
use windows::Win32::Foundation::{GetLastError, ERROR_ALREADY_EXISTS, HWND, LPARAM, LRESULT, WPARAM};
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

/// 一条任务栏与其覆盖层一一对应（默认仅主屏，文档 §4.1）
struct Bar {
    taskbar: Taskbar,
    overlay: Option<Overlay>,
}

struct App {
    machine: CycleMachine,
    config: config::AppConfig,
    bars: Vec<Bar>,
    last_snap: Snapshot,
    last_tick: Instant,
    tick_count: u64,
    geometry_dirty: bool,
    taskbar_created_msg: u32,
    /// 当前定时器间隔；仅在变化时才调用 SetTimer，避免重复重置丢帧
    timer_interval: u32,
}

impl App {
    fn new(cfg: config::AppConfig, taskbar_created_msg: u32) -> App {
        let machine = CycleMachine::new(cfg.focus, cfg.short_break, cfg.long_break);
        let bars = Taskbar::refresh_all(cfg.render_all_monitors)
            .into_iter()
            .map(|taskbar| Bar {
                taskbar,
                overlay: None,
            })
            .collect();
        App {
            machine,
            config: cfg,
            bars,
            last_snap: Snapshot::idle(),
            last_tick: Instant::now(),
            tick_count: 0,
            geometry_dirty: false,
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
        if self.geometry_dirty || self.tick_count % geo_period == 0 {
            self.refresh_geometry();
        }

        // 仅在快照变化或过渡插值中重绘：暂停时时间冻结、快照不变，跳过无谓重绘
        let transitioning = self.bars.iter().any(|b| {
            b.overlay
                .as_ref()
                .map(|o| o.is_transitioning())
                .unwrap_or(false)
        });
        if snap != self.last_snap || transitioning {
            self.apply(snap, now);
        }
        self.last_snap = snap;

        // 过渡期提高帧率，保证色彩插值平滑（文档 §4.3）；
        // 仅在间隔实际变化时才调用 SetTimer，避免重复重置导致丢帧
        let interval = if transitioning { TICK_FAST_MS } else { TICK_MS };
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
            // 按句柄匹配旧覆盖层：句柄与矩形均未变则复用，否则重建
            let overlay = self
                .bars
                .iter_mut()
                .find(|b| b.taskbar.hwnd == tb.hwnd && rect_eq(&b.taskbar.rect, &tb.rect))
                .and_then(|b| b.overlay.take());
            new_bars.push(Bar { taskbar: tb, overlay });
        }
        self.bars = new_bars;
        self.geometry_dirty = false;
    }

    /// 把状态快照落到任务栏视觉上（文档 §4.2 / §4.3）
    fn apply(&mut self, snap: Snapshot, now: Instant) {
        // 待机：完全停止绘制（文档 §4.3）
        if snap.idle {
            for b in self.bars.iter_mut() {
                if let Some(o) = b.overlay.as_mut() {
                    o.hide();
                }
            }
            return;
        }
        let target = self.config.palette.for_phase(snap.phase);
        for b in self.bars.iter_mut() {
            // 覆盖层缺失或父任务栏已销毁（资源管理器重启）→ 静默重建（文档 §7）
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
            o.render(snap, target, now);
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
    let _mutex = match unsafe { CreateMutexW(None, false, w!("Barmato.SingleInstance")) } {
        Ok(m) => m,
        Err(_) => return,
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

    let mut app = App::new(cfg, taskbar_created_msg);
    // 上下文挂载到窗口自身（Win32 正统做法），无全局可变状态
    unsafe {
        SetWindowLongPtrW(hwnd, GWLP_USERDATA, &mut app as *mut App as isize);
    }

    // 托盘：左键 = 启停切换，右键 = 菜单（仅退出）（文档 §3.1/§3.2）
    let menu = Menu::new();
    let exit_item = MenuItem::with_id(MenuId::new("barmato.exit"), "退出", true, None);
    let exit_id = exit_item.id();
    let _ = menu.append(&exit_item);
    let Ok(_tray) = TrayIconBuilder::new()
        .with_menu(Box::new(menu))
        .with_icon(make_icon())
        .with_tooltip("Barmato - 左键启停 / 右键退出")
        .with_title("Barmato")
        .with_menu_on_left_click(false) // 左键不弹菜单，专用于 toggle
        .build()
    else {
        return;
    };

    unsafe {
        SetTimer(hwnd, TIMER_ID, TICK_MS, None);
    }

    let mut msg = MSG::default();
    unsafe {
        while GetMessageW(&mut msg, None, 0, 0).as_bool() {
            // 菜单事件：退出
            if let Ok(ev) = MenuEvent::receiver().try_recv() {
                if ev.id() == exit_id {
                    PostQuitMessage(0);
                }
            }
            // 托盘图标事件：左键 Down → toggle
            if let Ok(ev) = TrayIconEvent::receiver().try_recv() {
                if let TrayIconEvent::Click {
                    button: MouseButton::Left,
                    button_state: MouseButtonState::Down,
                    ..
                } = ev
                {
                    let app =
                        &mut *(GetWindowLongPtrW(hwnd, GWLP_USERDATA) as *mut App);
                    app.on_toggle();
                }
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
            app.geometry_dirty = true;
            DefWindowProcW(hwnd, msg, wparam, lparam)
        }
        m if m != 0 && m == app.taskbar_created_msg => {
            // 资源管理器重启：强制重建任务栏几何与覆盖层
            app.geometry_dirty = true;
            app.refresh_geometry();
            LRESULT(0)
        }
        WM_DESTROY => {
            PostQuitMessage(0);
            LRESULT(0)
        }
        _ => DefWindowProcW(hwnd, msg, wparam, lparam),
    }
}

/// 程序生成 32×32 托盘图标（低饱和番茄绿圆点），无外部资源依赖
fn make_icon() -> Icon {
    let size = 32u32;
    let mut rgba = vec![0u8; (size * size * 4) as usize];
    let (c, r) = (15.5_f64, 14.0_f64);
    for y in 0..size {
        for x in 0..size {
            let (dx, dy) = (x as f64 - c, y as f64 - c);
            if dx * dx + dy * dy <= r * r {
                let i = ((y * size + x) * 4) as usize;
                rgba[i] = 72;
                rgba[i + 1] = 110;
                rgba[i + 2] = 90;
                rgba[i + 3] = 255;
            }
        }
    }
    Icon::from_rgba(rgba, size, size).expect("tray icon")
}
