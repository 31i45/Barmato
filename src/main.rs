//! Winmato 薄主线（文档 §6：深模块、浅接口）
//!
//! 本文件只做粘合：单实例守卫 → 加载配置 → 隐藏宿主窗口 + 消息循环
//! → 托盘图标（左键启停 / 右键退出）→ 定时驱动周期状态机 → 按快照渲染。
//! 全部系统复杂度封装在各深模块内部。
//!
//! T 方案（文档 §4.1）：托盘图标本身即唯一可视化表面——画布尺寸自适应
//! 任务栏厚度（启动时只读一次，失败回退 SM_CXSMICON），其上渲染 2×2 格阵
//! （四格 ≡ 4 个专注番茄，格位 = 番茄序，灌注高度 = 当前阶段进度，色相 = 阶段；
//! 边框永存；暂停叠加居中暂停符号 ❚❚），由 icon.rs 的纯函数渲染器生成。
//! 交互入口与可视化合二为一。

#![windows_subsystem = "windows"]

use std::time::Instant;

use winmato::colors::Palette;
use winmato::config;
use winmato::cycle::{CycleMachine, Snapshot};
use winmato::icon::IconRenderer;

use tray_icon::menu::{Menu, MenuEvent, MenuId, MenuItem};
use tray_icon::{MouseButton, MouseButtonState, TrayIconBuilder, TrayIconEvent};
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
/// 托盘提示文案：常规待机/运行态 与 本轮完整完成后的待机态（§4.2 完成标记）
const TOOLTIP_NORMAL: &str = "左键启停 / 右键退出";
const TOOLTIP_COMPLETED: &str = "本轮已完成 · 左键启停 / 右键退出";

struct App {
    machine: CycleMachine,
    palette: Palette,
    renderer: IconRenderer,
    tray: tray_icon::TrayIcon,
    last_snap: Snapshot,
    last_tick: Instant,
    /// 当前定时器间隔；仅在变化时才调用 SetTimer，避免重复重置丢帧
    timer_interval: u32,
    /// 已落盘的 tooltip（完成态变化时才更新，避免重复 FFI）
    last_tooltip: Option<&'static str>,
}

impl App {
    fn new(cfg: config::AppConfig, tray: tray_icon::TrayIcon, renderer: IconRenderer) -> App {
        let machine = CycleMachine::new(cfg.focus, cfg.short_break, cfg.long_break);
        App {
            machine,
            palette: cfg.palette,
            renderer,
            tray,
            last_snap: Snapshot::idle(),
            last_tick: Instant::now(),
            timer_interval: TICK_MS,
            last_tooltip: None,
        }
    }

    /// 定时心跳：推进计时 → 渲染 → 按动画状态调整帧率
    fn on_timer(&mut self, hwnd: HWND) {
        let now = Instant::now();
        let delta = now.duration_since(self.last_tick);
        self.last_tick = now;
        self.machine.tick(delta);

        let snap = self.machine.snapshot();
        let animating = self.renderer.needs_continuous_render();
        if snap != self.last_snap || animating {
            self.apply(snap, now);
        }
        self.last_snap = snap;

        // 过渡/脉冲等短动画提高帧率保证平滑（文档 §4.3）；12s 慢呼吸用常规帧率即可。
        // 仅在间隔实际变化时才调用 SetTimer，避免重复重置导致丢帧
        let fast = self.renderer.needs_fast_frames();
        let interval = if fast { TICK_FAST_MS } else { TICK_MS };
        if interval != self.timer_interval {
            self.timer_interval = interval;
            unsafe {
                SetTimer(hwnd, TIMER_ID, interval, None);
            }
        }
    }

    /// 唯一用户交互（文档 §3.1）：托盘图标左键点击触发状态切换。
    fn on_toggle(&mut self) {
        let now = Instant::now();
        self.machine.toggle();
        let snap = self.machine.snapshot();
        self.apply(snap, now);
        self.last_snap = snap;
    }

    /// 把状态快照落到托盘图标上（文档 §4.2 / §4.3）
    fn apply(&mut self, snap: Snapshot, now: Instant) {
        let icon = self.renderer.render(snap, &self.palette, now);
        let _ = self.tray.set_icon(Some(icon));
        // 完成标记的文字补充：tooltip 仅在完成态翻转时更新（零视觉成本的信息通道）
        let tooltip = if snap.idle && snap.completed {
            TOOLTIP_COMPLETED
        } else {
            TOOLTIP_NORMAL
        };
        if self.last_tooltip != Some(tooltip) {
            let _ = self.tray.set_tooltip(Some(tooltip));
            self.last_tooltip = Some(tooltip);
        }
    }

    /// 退出前复位到待机图标（文档 §3.2：退出即放弃周期；§7 不留残迹）
    fn shutdown(&mut self) {
        self.renderer.reset();
        let icon = self
            .renderer
            .render(Snapshot::idle(), &self.palette, Instant::now());
        let _ = self.tray.set_icon(Some(icon));
    }
}

/// 将托盘图标从隐藏溢出区提升到任务栏可见区（Windows 11）。
///
/// Windows 11 默认将新通知图标放入隐藏溢出区（chevron 折叠区）。
/// 提升需修改注册表 `HKCU\Control Panel\NotifyIconSettings\<id>`
/// 的 `IsPromoted` 值为 1。该值在 `Shell_NotifyIconW(NIM_ADD)` 后由
/// 系统同步创建，故在托盘图标创建后立即调用即可可靠找到。
///
/// 失败时静默降级——图标仍可用，只是留在溢出区（文档 §7 优雅降级）。
fn promote_tray_icon() {
    use windows::core::PCWSTR;
    use windows::Win32::System::Registry::{
        RegCloseKey, RegEnumKeyExW, RegOpenKeyExW, RegQueryValueExW, RegSetValueExW, HKEY,
        HKEY_CURRENT_USER, KEY_READ, KEY_WRITE, REG_DWORD, REG_EXPAND_SZ, REG_SZ, REG_VALUE_TYPE,
    };

    let exe_path = match std::env::current_exe() {
        Ok(p) => match p.to_str() {
            Some(s) => s.to_lowercase(),
            None => return,
        },
        Err(_) => return,
    };

    unsafe {
        let mut root = HKEY::default();
        if RegOpenKeyExW(
            HKEY_CURRENT_USER,
            w!("Control Panel\\NotifyIconSettings"),
            0,
            KEY_READ | KEY_WRITE,
            &mut root,
        )
        .is_err()
        {
            return;
        }

        let mut index = 0u32;
        loop {
            let mut name = [0u16; 256];
            let mut name_len = name.len() as u32;
            let result = RegEnumKeyExW(
                root,
                index,
                windows::core::PWSTR(name.as_mut_ptr()),
                &mut name_len,
                None,
                windows::core::PWSTR::null(),
                None,
                None,
            );
            if result.is_err() {
                break;
            }
            index += 1;

            let mut sub = HKEY::default();
            if RegOpenKeyExW(
                root,
                PCWSTR(name.as_ptr()),
                0,
                KEY_READ | KEY_WRITE,
                &mut sub,
            )
            .is_err()
            {
                continue;
            }

            let mut buf = [0u16; 512];
            let mut buf_len = buf.len() as u32;
            let mut reg_type = REG_VALUE_TYPE::default();
            let qres = RegQueryValueExW(
                sub,
                w!("ExecutablePath"),
                None,
                Some(&mut reg_type),
                Some(buf.as_mut_ptr() as *mut u8),
                Some(&mut buf_len),
            );

            if qres.is_ok() && (reg_type == REG_SZ || reg_type == REG_EXPAND_SZ) {
                let len = (buf_len / 2) as usize;
                let stored = String::from_utf16_lossy(&buf[..len])
                    .trim_end_matches('\0')
                    .to_lowercase();
                if stored == exe_path {
                    let _ = RegSetValueExW(
                        sub,
                        w!("IsPromoted"),
                        0,
                        REG_DWORD,
                        Some(&1u32.to_ne_bytes()),
                    );
                }
            }
            let _ = RegCloseKey(sub);
        }
        let _ = RegCloseKey(root);
    }
}

fn main() {
    unsafe {
        let _ = SetProcessDpiAwarenessContext(DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2);
    }

    // 单实例守卫（文档 §3.2：重启即回归待机）
    // GetLastError 必须在 CreateMutexW 后立即检查——
    // 中间任何 Win32/IO 调用都会覆盖线程的 LastError 值
    let Ok(_mutex) = (unsafe { CreateMutexW(None, false, w!("Winmato.SingleInstance")) }) else {
        return;
    };
    if unsafe { GetLastError() } == ERROR_ALREADY_EXISTS {
        return;
    }

    // 配置：缺失/损坏静默回退默认（文档 §6.5）
    let cfg = config::load();

    let instance = unsafe { GetModuleHandleW(None) }.unwrap_or_default();

    // 隐藏宿主窗口：仅作为消息循环载体，用户永不可见
    let class = w!("WinmatoHost");
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

    // 托盘：左键 = 启停切换，右键 = 菜单（仅退出）（文档 §3.1/§3.2）
    // 初始图标 = 待机态（四格空心灰色轮廓）
    let mut renderer = IconRenderer::new();
    let initial_icon = renderer.render(Snapshot::idle(), &cfg.palette, Instant::now());

    let menu = Menu::new();
    let exit_item = MenuItem::with_id(MenuId::new("winmato.exit"), "退出", true, None);
    let exit_id = exit_item.id();
    let _ = menu.append(&exit_item);
    let Ok(tray) = TrayIconBuilder::new()
        .with_menu(Box::new(menu))
        .with_icon(initial_icon)
        .with_tooltip(TOOLTIP_NORMAL)
        .with_title("Winmato")
        .with_menu_on_left_click(false)
        .build()
    else {
        return;
    };

    // 将托盘图标提升到任务栏可见区
    promote_tray_icon();

    let mut app = App::new(cfg, tray, renderer);
    unsafe {
        SetWindowLongPtrW(hwnd, GWLP_USERDATA, &mut app as *mut App as isize);
    }

    unsafe {
        SetTimer(hwnd, TIMER_ID, TICK_MS, None);
    }

    let mut msg = MSG::default();
    unsafe {
        while GetMessageW(&mut msg, None, 0, 0).as_bool() {
            if let Ok(ev) = MenuEvent::receiver().try_recv() {
                if ev.id() == exit_id {
                    let app = &mut *(GetWindowLongPtrW(hwnd, GWLP_USERDATA) as *mut App);
                    app.shutdown();
                    PostQuitMessage(0);
                    continue;
                }
            }
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
        WM_DESTROY => {
            app.shutdown();
            PostQuitMessage(0);
            LRESULT(0)
        }
        _ => DefWindowProcW(hwnd, msg, wparam, lparam),
    }
}
