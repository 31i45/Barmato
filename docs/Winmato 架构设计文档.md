# Winmato 架构设计文档

> Win + Tomato — 极简环境式番茄钟 for Windows
>
> 版本：2.2（T 方案修订 — 边框永存 / 任务栏厚度自适应画布 / 待填充待机 / 暂停符号强化 / lib+bin 分层 / 更名 Winmato｜对应 47 项测试全绿 / clippy 零警告 / release 精简单文件）
>
> 配套规范：《Winmato 完整开发需求文档（AI 编程专用｜极简环境式正式版）》（下称"需求文档"，本文档以 §N 引用其章节）

---

## 1. 设计哲学（架构的最高约束）

所有架构决策从属于以下五条公理，冲突时按序裁决：

| # | 公理 | 架构含义 |
|---|------|----------|
| 1 | **深模块、浅接口** | 每个模块隐藏最大复杂度、暴露最小接口；主线 `main.rs` 只做粘合 |
| 2 | **慢变量色相、快变量灌注高度、更慢变量空间编码、运动只标记事件** | 阶段→格阵色相（慢变量）；阶段进度→当前格底部灌注高度（快变量）；番茄序号→2×2 格位（更慢，纯空间）；阶段切换→单次脉冲；暂停→低频呼吸 |
| 3 | **消除特殊场景** | 不为小众场景加分支（配置损坏整体回退、零时长兜底、休眠一次结算） |
| 4 | **静默优雅降级** | 任何系统级异常自愈或回退默认态，绝不弹窗、不崩溃、不打断专注 |
| 5 | **极致轻量** | 待机零绘制；运行时帧率按需协商；无 GC、无运行时、无浏览器依赖；托盘图标即唯一可视化表面，不持续跟踪外部几何（启动只读一次任务栏厚度） |

**T 方案第一性原理**：托盘图标是应用自有表面，不侵入系统元素（任务栏、开始按钮）。画布尺寸自适应任务栏厚度（启动时只读一次 `Shell_TrayWnd` 矩形，短边 − 双侧 4px 留白；失败回退 SM_CXSMICON），以任务栏内容许的最大尺寸承载全部可视化——2×2 格阵（四格 ≡ 四个专注番茄），格位 = 番茄序号（空间编码），灌注高度 = 当前阶段进度，色相 = 阶段，1px 边框永存。交互入口与可视化合二为一。

## 2. 系统总览

```
┌──────────────────── Windows 单线程消息循环 ────────────────────┐
│                                                                 │
│  托盘事件                    WM_TIMER 心跳                      │
│  (左键启停 / 右键退出)       250ms / 50ms 自适应                 │
│      │                           │                              │
│      ▼                           ▼                              │
│  ┌─────────┐  toggle()  ┌──────────────────┐                    │
│  │ Cycle-  │◄──────────►│  App (薄主线)    │                    │
│  │ Machine │  snapshot()│  main.rs         │                    │
│  │ cycle.rs│───────────►│                  │                    │
│  └─────────┘  Snapshot  └────────┬─────────┘                    │
│   纯状态机                       │                              │
│   零系统依赖                     ▼                              │
│                       ┌──────────────────┐                      │
│                       │  IconRenderer     │                      │
│                       │  icon.rs          │                      │
│                       │  Snapshot+Palette │                      │
│                       │    → RGBA → Icon  │                      │
│                       └────────┬─────────┘                      │
│                                │                                │
└────────────────────────────────┼────────────────────────────────┘
                                 ▼
                          托盘图标（任务栏厚度自适应）
                     2×2 格阵 = 四格 ≡ 四番茄，边框永存
```

**依赖方向**（单向，无环）：

```
main.rs ──► winmato lib ──► cycle.rs / icon.rs / colors.rs / config.rs
examples/gen_icon.rs ──► winmato lib（icon::idle_rgba / colors::Palette）
icon.rs ──► cycle.rs (Snapshot, Phase) / colors.rs (Palette, Rgb)
config.rs ──► colors.rs (Palette, Rgb)
colors.rs ──► cycle.rs (Phase，仅 for_phase 映射)
cycle.rs ──► (无内部依赖，纯叶子模块，仅 std::time::Duration)
```

领域模块收敛于库目标 `winmato`（src/lib.rs），二进制主线与示例都只依赖库——
`examples/gen_icon` 由此复用与托盘完全相同的渲染核心生成应用图标，杜绝第二实现。

模块间通信的唯一协议是 **`Snapshot` 值语义快照**——渲染层不感知计时，状态机不感知托盘。`Snapshot` 携带渲染所需的全部信息（阶段、进度、番茄序号、边界序号、暂停/待机标记），渲染器据此 + 调色板 + 时间戳一次性产出像素。

**已删除模块**（T 方案不再需要）：`quad.rs`、`start_button.rs`、`taskbar.rs`——分别对应旧方案的任务栏几何探测、开始按钮遮挡规避、四刻点空间编码，在 T 方案中由托盘图标内部格阵完全取代，零外部几何依赖。

## 3. 模块契约

### 3.1 cycle.rs — 周期状态机（纯业务，文档 §6.1）

| 项 | 内容 |
|----|------|
| 职责 | 番茄大周期时序、计时、暂停/继续、阶段流转、待机回归 |
| 依赖 | 仅 `std::time::Duration`，零 Windows API，可独立单元测试 |
| 状态 | `durations[8]` + `stage: 0..=8`（**8 = 待机哨兵**）+ `elapsed` + `paused` + `stage_seq: u64` + `completed: bool` |

**固定大周期**（文档 §2.1，不可乱序、不可跳过）：

```
stage:  0    1    2    3    4    5    6    7     8
        专注 短休 专注 短休 专注 短休 专注 长休  待机(哨兵)
        25m  5m  25m  5m  25m  5m  25m  20m
```

**对外接口（仅 3 个方法 + 快照）**：

- `toggle()`：唯一交互入口（§3.1）。点击语义由当前状态唯一决定：待机→启动（`stage_seq += 1`，启动也是边界事件）；运行→暂停；暂停→继续；**长休暂停→收束本轮**（`completed` 置位、回归待机、`stage_seq += 1`——长休是收尾阶段，4 枚番茄已全部获得，恢复收尾冷却毫无收益，再点击即「休息结束」）
- `tick(delta)`：推进计时。暂停/待机完全冻结；**单次 tick 可跨多阶段**（系统休眠恢复），循环一次结算到位，每跨一阶段 `stage_seq += 1`
- `snapshot()`：返回渲染层唯一需要的信息：

```rust
pub struct Snapshot {
    pub idle: bool,
    pub paused: bool,
    pub phase: Phase,            // Focus | ShortBreak | LongBreak
    pub phase_progress: f64,     // 当前阶段进度 0.0..=1.0（快变量 → 灌注高度）
    pub pomodoro_index: u8,      // 第几个专注番茄 1..=4（慢变量 → 格位；待机 0）
    pub stage_seq: u64,          // 边界事件序号，单调递增 → 脉冲触发源
    pub completed: bool,         // 本轮完成标记：tooltip 通道；待机视觉恒为四格待填充
}
```

**设计要点**：

- 进度语义是**当前阶段**而非全周期（§4.2 修订）：25min 填满整格，回答用户最高频的问题"这个番茄还剩多久"
- `pomodoro_index` 是纯空间编码（2×2 格阵哪格亮），**绝不显示数字**（§1.2 反目标）
- `stage_seq` 跨周期单调不回绕（待机快照仍携带真实序号），供渲染层做边沿检测
- `completed` 生命周期：`tick()` 周期自然收尾 → 置位并回归待机；**长休暂停后再点击 → 收束本轮**（4 枚番茄均已获得）同样置位；`toggle()` 启动新周期 → 清零；运行态恒 false——点击路径不存在中途放弃（唯一出口是长休收束与自然走完，二者都发生在 4 枚番茄收获之后），「完成 ⟺ 收获 4 枚番茄」语义严格成立
- 零时长配置兜底：`phase_progress = 1.0`，不 panic 不除零

### 3.2 icon.rs — 托盘图标渲染器（深模块，文档 §6.4）

| 项 | 内容 |
|----|------|
| 职责 | 把 `Snapshot` + `Palette` + `Instant` 翻译为任务栏厚度自适应尺寸的 RGBA 像素 → `Icon`；全部动画状态机内埋于此 |
| 依赖 | `FindWindowW("Shell_TrayWnd")` + `GetWindowRect`（启动时只读一次取任务栏厚度）+ `GetSystemMetrics(SM_CXSMICON)`（回退）+ `tray_icon::Icon` + `cycle::{Phase, Snapshot}` + `colors::{Palette, Rgb}` |
| 纯度 | `render()` 是渲染层唯一的语义决策点——`plan()` 纯函数推导 4 格规格，`render_buf()` 纯函数生成像素；任务栏厚度推导 `icon_size_from_taskbar()` 同为纯函数（矩形宽高 → 边长），FFI 查询隔离在 `detect_icon_size()`；动画状态封装在 `IconRenderer` 结构体内 |
| 测试 | 26 项单元测试覆盖任务栏厚度推导、几何自适应、plan 语义、边框永存、待填充待机同构、暂停符号（满不透明/阶段色）、应用图标同构、alpha 合成、脉冲/呼吸数学、像素边界、连续渲染不 panic |

**图标几何**（画布边长 = 任务栏厚度 − 2×`TASKBAR_MARGIN(4)`；`MIN_SIZE = 16` 兜底；查询失败回退 SM_CXSMICON）：

```
启动一次查询（detect_icon_size，FFI 隔离层）：
  FindWindowW("Shell_TrayWnd") → GetWindowRect → (width, height)
  → icon_size_from_taskbar(width, height)（纯函数）：
       thickness = min(width, height)     // 底/顶部短边=高，左/右侧短边=宽
       size      = thickness − 8          // 双侧各留 4px
       size < 16 → None → 回退 SM_CXSMICON（自动隐藏态等）

格阵参数化（geometry(size)，纯函数）：
sq  = size / 3          （单格边长；40px 时 → 13）
gap = max(size / 16, 1) （格间距；40px → 2，24px → 1）
off = (size − 2·sq − gap) / 2（格阵偏移，居中）

标定值：Win11@100% 任务栏厚 48 → size 40 → (13, 2, 6)
        Win11@150% 任务栏厚 72 → size 64 → (21, 4, 9)
        回退 32 → (10, 2, 5)、24 → (8, 1, 3)、16 → (5, 1, 2)
```

> **尺寸决策（产品判断）**：应用价值全靠视觉体验，图标在任务栏容许范围内应尽可能大，
> 故以任务栏厚度（应用 UI 容器的物理尺寸）而非系统小图标标称值定画布。
> 查询只在启动时做一次、纯读取、失败必回退——不订阅几何变化、不引入持续依赖；
> 格阵几何全部按比例参数化，任何尺寸下「四格 ≡ 四番茄」语义不变。

**格位与番茄序号的映射**：格 0 = 第 1 番茄、格 1 = 第 2、格 2 = 第 3、格 3 = 第 4（行优先：TL→TR→BL→BR）。

**T 方案视觉语义**（与 S 方案一致，文档 §4.2）：

| 状态 | 格 0..idx−1 | 格 idx（当前） | 格 idx+1..3 |
|------|-------------|----------------|-------------|
| 专注 N | 实心专注色 | 底部灌注专注色（高度=进度） | 灰色空心轮廓 |
| 休息 N | 实心专注色 | 专注色底 + 休息色底部灌注（高度=进度） | 灰色空心轮廓 |
| 待机 | 灰色空心轮廓 | 灰色空心轮廓 | 灰色空心轮廓 |
| 待机·本轮已完成 | 灰色空心轮廓（**同普通待机**） | 灰色空心轮廓 | 灰色空心轮廓 |

> 待机恒为四格空心「待填充」——完成与否不产生视觉特殊分支（消除特殊场景），
> `completed` 信息走 tooltip 通道。所有结束路径（自然走完 / 长休暂停后收束）
> 收束到同一待机态，下一次点击开启全新周期。

**结构层 / 内容层正交（边框永存不变量）**：每格 1px 轮廓是**结构层**，底色/灌注是**内容层**，二者正交。内容层绘制一律限于 inset 1px 的内区（`draw_solid`/`draw_fill` 从 `sx+1`/`sy+1` 到 `sx+sq−1`/`sy+sq−1`），结构层最后统一绘制——任何实心/灌注状态下边框都不被吞没，空心 / 实心 / 灌注三态在小画布上恒可分辨。

**语义不变量**：灌注永远从底部向上增长（`draw_fill` 从 `sy + sq − fh` 到 `sy + sq − 1`），最低 1px（`max(1.0)`），即使 progress=0.0 也有 1px 可见信号——"这个番茄已开始"。休息格的底色=专注色（已获得），灌注色=休息色（当前阶段），随休息进度从底部覆盖专注色。

**渲染管线**（每帧）：

```
Snapshot + Palette + Instant
   │
   │  1. 边沿检测：stage_seq 变化 → 启动 2.5s 脉冲（含周期收尾待机边界）
   │  2. 暂停检测：paused 上升沿 → 启动呼吸；下降沿 → 停止
   │  3. 色相过渡：目标色变化 → 记录 prev_color，启动 0.6s lerp
   ▼ plan()  —— 纯函数，由快照推导 4 格规格
   │  待机 → 恒为四格空心「待填充」（无 completed 分支，零特殊场景）
   │  运行：idx = pomodoro_index − 1 (0-based)
   │    格 i < idx 或 (i==idx 且非专注) → base = focus（已获得的番茄）
   │    格 i == idx → fill = (phase_color, phase_progress)
   ▼ render_buf() —— 纯函数，生成 RGBA 缓冲（内容层先、结构层后）
   │  逐格：base 有 → draw_solid（inset 内区）| fill 有 → draw_fill（inset 内区底部）
   │        base 无 fill 无 → draw_outline（1px 空心）
   │  结构层：全部格子最后统一 draw_outline → 边框永存
   │  fill 色用插值后的 fill_color（阶段过渡平滑）
   │  全局 alpha = 255 × dim(暂停0.5) × (1+呼吸) × (1+脉冲)
   │  暂停符号 ❚❚：fill_color、恒 255（不参与降亮/呼吸——对比锚点）
   ▼ Icon::from_rgba(buf, size, size)
```

**动画状态机**（内埋于 `IconRenderer`，主线零感知）：

| 动画 | 触发 | 常量 | 参数 | 帧率需求 |
|------|------|------|------|----------|
| 色相过渡插值 | 目标色变化 | `TRANSITION_SECS = 0.6` | lerp(prev→target, t/0.6) | 快帧（50ms） |
| 边界脉冲 | `stage_seq` 边沿检测 | `PULSE_SECS = 2.5`，`PULSE_PEAK = 0.15` | 0→+15%→0 单次正弦 | 快帧（50ms） |
| 暂停呼吸 | `paused` 上升沿 | `BREATH_PERIOD_SECS = 12.0`，`BREATH_AMPLITUDE = 0.10` | ±10% 正弦持续 | 常规帧（250ms，48 步/周期，步进 <1.3% 幅值，低于感知阈值） |

**alpha 合成公式**（`alpha_at`，纯函数）：

```
alpha = 255 × dim × (1 + breath) × (1 + pulse)，钳制 [0, 255]

dim     = paused ? 0.5 : 1.0      （PAUSE_DIM = 0.5，暂停降亮 50%）
breath  = 0.10 × sin(2π × t / 12) （暂停期间持续振荡）
pulse   = 0.15 × sin(π × t/2.5)   （边界后 2.5s 单次，t∈[0,1]）
```

alpha 作用于全部已绘制像素（实心格、灌注格、轮廓格），透明背景像素保持 (0,0,0,0)。暂停降亮通过降低 alpha 实现——方格半透明，与系统托盘底色混合，视觉上变暗。**例外：暂停符号 ❚❚ 恒满不透明（255）且取当前阶段色**——格阵整体降亮后，它是唯一满亮度元素，条宽 = size/10、条高 = size/2、条间距 = size/10，任意任务栏主题（明/暗）下都是第一可读元素。

**应用图标生成**（`examples/gen_icon`）：exe 文件图标由**同一渲染核心**逐尺寸生成（16/24/32/48/64/128/256，每尺寸独立推导几何而非缩放位图，小尺寸线条精确 1px）——应用图标 ≡ 托盘待机图标（四格待填充），`idle_rgba` 纯函数为唯一入口，`idle_rgba_matches_tray_idle` 测试钉死同构性。ICO 容器为手写 32 位 BMP 项（零第三方依赖），运行 `cargo run --example gen_icon` 覆盖 `Winmato.ico`。

**对主线的帧率协商接口**：

- `needs_continuous_render()` → 过渡/脉冲/呼吸任一进行中 → 主线持续重绘
- `needs_fast_frames()` → 仅过渡/脉冲 → 主线切 50ms 心跳（呼吸不需要快帧）
- `reset()` → 复位全部动画状态（待机/退出时调用）

**静默降级**：`render()` 内部无错误路径——缓冲尺寸 = size×size×4 字节（启动时由任务栏厚度一次性决定，进程生命周期内恒定），`Icon::from_rgba` 不可能失败；`tray.set_icon()` 的返回值被 `let _ =` 忽略，失败时下帧自动重试。

### 3.3 colors.rs — 色彩运算（纯函数）

- `Rgb`：`new` / `from_hex`（非法返回 None）/ `lerp`（阶段过渡线性插值）
- `IDLE_OUTLINE = (110, 110, 110)`：待机/未完成格的空心轮廓色——低存在感但**恒可见**，保障首次运行可发现入口
- `Palette` 出厂默认（T 方案色彩自由——不受 alpha 叠加蓝色 logo 的限制，饱和度可适当提高以在小尺寸画布中保持辨识度）：

| 阶段 | RGB | 注释 HSL | 色相间隔 |
|------|-----|----------|----------|
| Focus | `(45, 140, 83)` | H145 S47% L33% 深绿 | — |
| ShortBreak | `(71, 179, 179)` | H175 S45% L50% 薄荷青 | +30° |
| LongBreak | `(85, 133, 200)` | H210 S40% L55% 天蓝 | +35° |

- `Palette::for_phase(Phase)` — 阶段→色值的映射
- `Palette::idle_outline()` — 返回 `IDLE_OUTLINE` 常量

### 3.4 config.rs — 配置加载（静默降级，文档 §6.5）

- 只读加载 `%APPDATA%\Winmato\config.json`；单位为**秒**（发烧友可用 25s 快速验证全周期）
- 合法区间 1..=36000s；任何缺失/损坏/越界/非法色值 → **整体或字段级静默回退出厂默认**，不报错不写回
- 默认值单一事实来源：`DEFAULT_FOCUS_SECS` / `DEFAULT_SHORT_BREAK_SECS` / `DEFAULT_LONG_BREAK_SECS` 常量
- 支持字段级部分覆盖（只改 focus 不动其余）
- 无 `render_all_monitors` 字段——T 方案只有托盘图标，与显示器数量无关

### 3.5 main.rs — 薄主线（粘合层）

**只做五件事**，全部复杂度下沉到深模块：

1. **单实例守卫**：命名互斥量 `Winmato.SingleInstance`；`GetLastError` 紧随 `CreateMutexW` 检查（任何中间 Win32 调用都会覆盖 LastError）
2. **DPI 声明**：`PerMonitorV2`，一次声明全程有效
3. **隐藏宿主窗口 + 消息循环**：`WNDCLASSW`（`hbrBackground` 需 `HBRUSH`，故保留 `Win32_Graphics_Gdi` feature）注册 `WinmatoHost` 类 → `CreateWindowExW` 创建零尺寸隐藏窗口作消息循环载体；`App` 上下文挂 `GWLP_USERDATA`（Win32 正统，无全局可变状态）
4. **托盘图标**（T 方案唯一可视化表面）：
   - `TrayIconBuilder` 同线程构建：初始图标 = 待机态（四格空心灰色轮廓）
   - 左键 Down → `on_toggle()`，右键菜单仅"退出"
   - `with_menu_on_left_click(false)` 确保左键不弹菜单
   - `promote_tray_icon()`：构建后立即调用，修改注册表 `HKCU\Control Panel\NotifyIconSettings\<id>` 的 `IsPromoted=1`，将图标从 Windows 11 隐藏溢出区提升到任务栏可见区（失败静默降级，图标仍在溢出区可用）
   - tooltip 二态（仅状态翻转时调用 `set_tooltip`，`last_tooltip` 缓存去重）：常规 "左键启停 / 右键退出"（`TOOLTIP_NORMAL`）；待机 + 完成标记 "本轮已完成 · 左键启停 / 右键退出"（`TOOLTIP_COMPLETED`）。暂停态不新增 tooltip 文案，改用**居中放大暂停符号 ❚❚**（阶段色、恒满不透明，通用暂停语义、非文字、不破坏四格几何语言）+ 降亮 50% + 12s 慢呼吸三重表达
5. **心跳调度**（`on_timer`）：

```
tick(Δt) → snapshot()
  → 重绘判定：snapshot 变化 OR renderer.needs_continuous_render()
  → 帧率协商：renderer.needs_fast_frames() → SetTimer(50ms)，否则 250ms
     （仅间隔实际变化时才调 SetTimer，避免重复重置丢帧）
```

**`App` 结构体**：

```rust
struct App {
    machine: CycleMachine,      // 周期状态机
    palette: Palette,           // 调色板（ durations 启动时已消费，仅配色需要存活）
    renderer: IconRenderer,     // 图标渲染器（封装全部动画状态）
    tray: tray_icon::TrayIcon,  // 托盘图标句柄
    last_snap: Snapshot,        // 上一帧快照（变化检测）
    last_tick: Instant,         // 上次心跳时间戳（Δt 计算）
    timer_interval: u32,        // 当前定时器间隔（避免重复 SetTimer）
    last_tooltip: Option<&'static str>, // 已落盘 tooltip（完成态翻转时才 FFI）
}
```

**消息处理**（`wnd_proc`）：

| 消息 | 响应 |
|------|------|
| `WM_TIMER`（TIMER_ID=1） | `on_timer(hwnd)`：推进计时 → 快照 → 按需渲染 → 帧率协商 |
| `WM_DESTROY` | `shutdown()`（复位待机图标）+ `PostQuitMessage(0)` |
| 其他 | `DefWindowProcW` |

**无系统广播处理**：T 方案不监听 `WM_DISPLAYCHANGE`、`WM_SETTINGCHANGE`、`TaskbarCreated`——托盘图标与任务栏几何无关，显示器变化、DPI 变化、explorer 重启均不影响图标渲染。`tray-icon` crate 内部处理 explorer 重启时的图标重注册。

**`shutdown` 方法**：调用 `renderer.reset()` 复位全部动画状态 → 用 `Snapshot::idle()` 渲染待机图标 → `tray.set_icon()`。退出即回归待机态，不留残迹。

## 4. 核心时序：一次左键点击的生命周期

```
用户左键托盘
  → TrayIconEvent::Click(Left, Down)
  → app.on_toggle()
      → machine.toggle()          # 待机→stage 0，stage_seq+1
      → snapshot()                # idle=false, phase=Focus, progress=0, idx=1
      → apply(snap, now)
          → renderer.render(snap, palette, now)
              → stage_seq 边沿检测 → pulse_start = now（脉冲启动）
              → 暂停检测：非暂停 → pause_since = None
              → 目标色 = palette.focus；首帧无过渡 → fill_color = focus
              → 脉冲 t=0 → pulse_gain(0)=0 → alpha=255（无增益）
              → plan() → 格 0 有 fill(focus, 0.0)，格 1..3 全空
              → render_buf() → 格 0 内区底部 1px focus 填充 + 全格 1px 边框，格 1..3 灰色轮廓
              → Icon::from_rgba(buf, size, size)
          → tray.set_icon(Some(icon))
  → 主线检测 needs_fast_frames()=true（脉冲进行中）→ 切 50ms 心跳
  → 2.5s 脉冲播放完 → needs_fast_frames()=false → 回 250ms
  → 此后每 250ms：progress 单调递增 → 快照变化 → 重绘一帧（4fps 恒定）
  → 阶段边界（专注→短休）：tick 跨 stage → snapshot 变化（phase=ShortBreak、progress 归零、seq+1）
      → 色相过渡 0.6s lerp（focus→short_break）+ 边界脉冲 2.5s + 格阵模式更新
      → 格 0 底色=focus（已获得），格 0 灌注=short_break（从底部增长）
  → ... 重复 F1→SB1→F2→SB2→F3→SB3→F4→LB ...
  → 长休结束：stage=8 哨兵 → completed=true → idle 快照（stage_seq+1 触发 2.5s 收尾脉冲）
      → 四格空心待填充待机 + tooltip 切换为"本轮已完成 · 左键启停 / 右键退出"
  → 下次左键 → toggle() → stage_seq+1、completed 清零 → 新周期开始
      （设计决策：不自动开始新周期——尊重用户节奏主权、番茄工作法正统、反心智负担）
```

**暂停时序**：

```
运行中 → 用户左键 → toggle() → paused=true
  → snapshot.paused=true
  → apply(snap, now)
      → renderer 检测 paused 上升沿 → pause_since = now（呼吸启动）
      → alpha = 255 × 0.5 × (1+breath) × (1+0) ≈ 128 ± 呼吸振荡
      → 全部方格半透明（降亮 50% + 12s 正弦呼吸）
      → 暂停符号 ❚❚：阶段色、恒 255——唯一满亮度对比锚点
      → 长休暂停后再点击 → 收束本轮（completed 置位）→ 待填充待机
  → needs_continuous_render()=true（呼吸进行中）→ 持续重绘
  → needs_fast_frames()=false（呼吸不需快帧）→ 保持 250ms 心跳
  → 用户再次左键 → toggle() → paused=false → pause_since=None（呼吸停止）
```

## 5. 线程与并发模型

**全程单线程**。Win32 消息循环驱动一切；`tray-icon` 事件和菜单事件经 crate 内部窗口过程转发到同线程 channel 接收器（`TrayIconEvent::receiver().try_recv()` / `MenuEvent::receiver().try_recv()`），主线在 `GetMessageW` 循环中 `try_recv` 轮询。无锁、无通道发送端、无后台线程——状态机的所有可变状态（`stage/elapsed/paused/stage_seq`）只被 `WM_TIMER` 心跳触碰，`IconRenderer` 的动画状态只被 `render()` 触碰，二者均在同一线程的 `on_timer`/`on_toggle` 中顺序执行，天然无数据竞争。

## 6. 内存与资源管理（Rust 所权，无 GC）

| 资源 | 获取 | 释放 | 泄漏路径核查 |
|------|------|------|--------------|
| 每帧像素缓冲 | `vec![0u8; size²×4]`（启动时由任务栏厚度决定；Win11@100% 为 40²×4=6.4KB） | `Icon::from_rgba` 接管所有权 → `tray.set_icon` 替换旧 Icon 时 drop | 即换即释，无累积 |
| 托盘平台图标 | `Icon::from_rgba` 构造 | `tray.set_icon` 替换时 tray-icon crate 内部释放旧句柄 | 无累积 |
| 配置 | 启动加载一次 | 进程退出时 `App` 析构 | 无 |
| 互斥量 | `CreateMutexW` | 进程退出时 OS 自动释放 | 无（命名互斥量随进程生命周期） |
| `stage_seq` | u64 递增 | — | 5.8 万年不回绕 |
| 注册表句柄 | `promote_tray_icon` 中 `RegOpenKeyExW` | `RegCloseKey` 显式关闭 | 每个子键 open→close 配对，循环退出时 root 也 close |

**待机态**：零渲染——`needs_continuous_render()` 返回 false + 快照恒为 `Snapshot::idle()` 不变化 → `on_timer` 跳过 `apply`，仅推进空 tick。CPU 占用趋近于零（250ms 一次空心跳）。

**每帧 size²×4 堆分配**：`render_buf` 中 `vec![0u8; (size*size*4) as usize]` 是热路径上唯一的堆分配（Win11@100% 任务栏下 40²×4 = 6.4KB；回退 SM_CXSMICON 路径为 24²×4 ≈ 2.3KB）。数 KB 在现代分配器（jemalloc/Windows Heap）下为 sub-bin 级别，分配+释放耗时纳秒级，4-20fps 下完全无感。复用缓冲需引入 `IconRenderer` 内部可变缓冲+生命周期管理，复杂度收益比为负——即换即释是最简方案。

## 7. 测试策略（47 项，按模块分层）

| 模块 | 数量 | 覆盖要点 |
|------|------|----------|
| cycle.rs | 13 | 待机初始 / 启动语义 / 暂停冻结 / **8 阶段全序+时长+刻点+seq** / 冻结点续跑 / 周期收尾重启 / **阶段内单调+四分点精确值** / **边界归零+seq 步进** / **休眠恢复跨阶段一次结算** / 零时长兜底 / **完成标记生命周期**（自然到期置位 → 重启清除）/ **运行中不置位** / **长休暂停收束**（暂停后再点击 → 待机+completed → 下次点击开新周期） |
| icon.rs | 26 | **任务栏厚度推导**（底部 48→40 / 侧边取短边 / 150% 72→64 / 自动隐藏薄矩形回退 None）/ **几何自适应**（32/24/16 标定值 / 拒绝过小画布 / with_size 钳制）/ **plan 语义**（待机全空 / **完成待机同构空心** / 专注1格填充 / 休息1格底+填充 / 专注4格三底一填 / 长休4格全底+填充）/ **待填充待机**（完成与否不改变待机视觉，像素级空心）/ **边框永存**（填充态四角+边中点采样=轮廓色、内区中心=阶段色）/ 空格空心 / 灌注高度随进度 / **暂停符号**（满不透明不随格阵降亮、阶段色、非暂停无）/ **应用图标同构**（idle_rgba ≡ 托盘待机渲染）/ **alpha 合成三项**（常规满值 / 暂停降亮 / 脉冲增益）/ **脉冲单次正弦**（0→峰→0）/ **呼吸周期幅度四相位** / 像素多尺寸不越界（16/24/32/48）/ 过小画布全透明 / 待机渲染不 panic / reset 清状态 / **连续渲染全周期不 panic**（含暂停与 completed 收尾） |
| colors.rs | 2 | hex 解析含非法拒绝 / lerp 端点 |
| config.rs | 6 | 空/损坏/部分覆盖/秒级单位/越界回退/非法色回退 |

测试哲学：**状态机的所有不变量用精确值断言钉死**（如四分点 `k/4.0` 精确相等——0.25/0.5/0.75 在 f64 中二进制精确表示）；动画数学独立成纯函数（`pulse_gain`/`breath_gain`/`alpha_at`）使时序逻辑可脱离窗口系统测试；`plan()` 纯函数使格阵语义可脱离 Icon 构造测试；`render_buf()` 纯函数使像素级断言（边框永存、灌注高度、待填充待机、暂停符号）无需窗口系统；`icon_size_from_taskbar()` 纯函数使任务栏厚度 → 图标边长的推导无需真实任务栏；连续渲染测试模拟完整周期 F1→SB1→F2→SB2→F3→SB3→F4→LB→idle（含一次暂停、以 completed 收尾），验证所有动画路径不 panic。

## 8. 技术选型依据

| 选择 | 替代方案 | 否决理由 |
|------|----------|----------|
| 纯 Rust + windows-rs | Electron / Tauri | 浏览器运行时数 MB 驻留，违背极致轻量 |
| tray-icon crate | 自写 NOTIFYICONDATA | 托盘菜单/事件/图标管理样板多，crate 已是极简封装且同线程 |
| 软件像素渲染（`vec![0u8; size²×4]` → `Icon::from_rgba`） | GPU 渲染（wgpu/D3D） | size²×4（6.4KB 级）的填充量用 CPU 逐像素写即可，GPU 初始化/上下文管理复杂度不成比例 |
| 任务栏厚度驱动画布尺寸 | 固定 32×32 / 仅用 SM_CXSMICON | 应用价值全靠视觉，图标应取任务栏内容许的最大尺寸；FFI 查询隔离 + 纯函数推导 + 失败回退，风险为零 |
| `IconRenderer` 封装动画状态 | 主线管理动画 | 动画状态（过渡/脉冲/呼吸的生命周期）内埋于渲染器，主线只查 `needs_continuous_render`/`needs_fast_frames` 两个布尔——深模块浅接口 |
| `plan()` 纯函数分离语义决策 | 渲染时内联判断 | 语义逻辑（哪格亮、哪格填）与像素绘制分离，可独立单元测试 |
| `serde_json` 只读配置 | 注册表 / 数据库 | 逃逸口定位（§2.2）：无 UI、文件即文档、损坏即回退 |
| `GWLP_USERDATA` 挂载 App | 全局可变静态 | Win32 正统做法，无 `static mut`，无线程安全顾虑 |
| 注册表 `IsPromoted` 提升图标 | 不提升 | Windows 11 默认将新图标放入隐藏溢出区，用户可能找不到；提升后直接在任务栏可见区 |

## 9. 异常处理矩阵（文档 §7 落地）

| 异常 | 检测 | 响应 | 用户感知 |
|------|------|------|----------|
| 配置缺失/损坏/越界 | 解析失败/字段非法 | 字段级或整体回退默认 | 无 |
| 多开 | 命名互斥量 `Winmato.SingleInstance` | `GetLastError == ERROR_ALREADY_EXISTS` → 第二实例静默 `return` | 无 |
| 系统休眠恢复 | tick Δt 巨大 | 循环结算跨过的所有阶段，一次到位 | 进度直达正确位置 |
| 托盘图标构建失败 | `TrayIconBuilder::build()` 返回 `Err` | `main()` 静默 `return`——程序不启动 | 无（程序未运行） |
| 宿主窗口创建失败 | `CreateWindowExW` 返回 `Err` | `main()` 静默 `return` | 无 |
| `set_icon` 失败 | `let _ = tray.set_icon(...)` 忽略返回值 | 下帧自动重试（快照变化或动画进行时必然重绘） | 短暂图标不更新 |
| 图标提升失败 | `promote_tray_icon` 中任一步返回 | 静默降级——图标留在溢出区仍可用 | 图标在隐藏区（需展开 chevron） |
| 任务栏几何查询失败 | `FindWindowW`/`GetWindowRect` 异常或厚度 < 16 | `detect_icon_size()` 回退 SM_CXSMICON | 图标略小，语义不变 |
| 零时长配置 | `dur.is_zero()` | `phase_progress = 1.0`（不除零不 panic） | 阶段瞬间结束 |
| explorer 重启 | tray-icon crate 内部处理 | crate 自动重注册图标 | 短暂消失后恢复 |

**无 panic 路径**：`Icon::from_rgba(buf, size, size).expect("icon render")` 是唯一的 `.expect()`，但缓冲尺寸恒为 size×size×4 字节（启动时由任务栏厚度一次性决定，进程内不变），`from_rgba` 不可能失败——这是"不可达"路径的类型断言，非运行时风险。

## 10. 已知有意取舍（记录在案）

1. **每帧 size²×4 堆分配**：`render_buf` 中 `vec![0u8; (size*size*4) as usize]` 是热路径唯一的堆分配（6.4KB 级）。数 KB 在现代分配器下为 sub-bin 级别，纳秒级分配释放；复用缓冲需在 `IconRenderer` 内引入可变 `Vec` + 生命周期管理，复杂度收益比为负——即换即释是最简方案。

2. **暂停呼吸 4fps**：48 步/12s 周期、步进 <1.3% 幅值，低于慢变感知阈值；提帧率只增功耗不增体验。

3. **`plan()` 存储 `phase_color` 但渲染时用 `fill_color`**：`plan()` 的 `SquareSpec.fill = Some((phase_color, progress))` 中 `phase_color` 仅用于测试断言（`assert_eq!(specs[0].fill, Some((p.focus, 0.5)))`）；`render()` 实际使用插值后的 `fill_color`（经 0.6s lerp）绘制灌注——`plan()` 不关心色彩过渡，`render()` 才做插值，职责分离。

4. **灌注最低 1px**：`frac.clamp(0.0, 1.0)` 后 `max(1.0)` 确保 progress=0.0 时仍有 1px 可见填充——"番茄已开始"的视觉信号，避免用户误以为未启动。

5. **`promote_tray_icon` 尽力而为**：注册表枚举匹配 `ExecutablePath` 后设 `IsPromoted=1`，但 Windows 版本差异、权限限制、路径大小写差异均可能导致失败——失败时图标仍在隐藏溢出区可用（展开 chevron 即可见），不阻断核心功能。

6. **不监听系统广播**：`wnd_proc` 仅处理 `WM_TIMER` 和 `WM_DESTROY`，不处理 `WM_DISPLAYCHANGE`/`WM_SETTINGCHANGE`/`TaskbarCreated`——托盘图标与显示器/任务栏/explorer 无关，无需响应这些事件。explorer 重启时的图标重注册由 `tray-icon` crate 内部处理。

7. **`render_all_monitors` 字段已删除**：T 方案只有托盘图标，与显示器数量无关。旧配置文件中如存在该字段，serde 默认忽略未知字段，不影响解析。

8. **`alpha` 降亮而非 `darken` 色值**：暂停时通过降低 alpha（透明度）实现视觉变暗——方格半透明与系统托盘底色混合。这比修改 RGB 色值更自然：恢复时只需 `pause_since=None`，无需色彩状态回退；且 alpha 同时受呼吸和脉冲调制，统一在 `alpha_at` 纯函数中结算。

9. **`Win32_Graphics_Gdi` feature 保留**：`WNDCLASSW` 结构体的 `hbrBackground: HBRUSH` 字段需要该 feature（虽用 `..Default::default()` 跳过显式赋值，类型定义仍需 Gdi）。移除会导致编译错误。

10. **`Win32_UI_Accessibility` / `Win32_System_Com` feature 已移除**：旧方案用 UIA 探测开始按钮、用 `CoInitializeEx` 初始化 COM——T 方案零外部几何依赖，二者均不需要。Cargo.toml 的 windows features 从 10 个精简到 8 个。

11. **完成态不加角标、不换图标、不实心**：为"周期完成"新增视觉词汇（角标/独立图标/实心变体）违反极简主义与 §1.2 反目标精神。待机**恒为四格空心「待填充」**——所有结束路径收束到同一视觉，零特殊分支（消除特殊场景）；完成信息由 tooltip 文字与完成瞬间已内置的 2.5s 边界脉冲表达——零新增概念，信息量足够。

12. **周期完成后不自动开始新周期**：自动循环剥夺用户节奏主权（长休后未必立即进入下一轮工作）、偏离番茄工作法正统（Cirillo 法中每轮由用户显式发起）、引入"随时可能开始计时"的心智负担。保持"左键 = 启停"单一交互语义，完成后用户自行决定何时开启新一轮。

13. **画布取任务栏厚度而非固定尺寸/SM_CXSMICON**：产品价值全靠视觉体验，图标应在任务栏容许范围内尽可能大——边长 = `min(taskbar_w, taskbar_h) − 8px`，底/顶/侧任务栏方向无关。代价仅是启动时一次只读查询：FFI 隔离在 `detect_icon_size()`、推导为纯函数 `icon_size_from_taskbar()`、任何失败回退 SM_CXSMICON，不订阅几何变化、不引入持续依赖；几何全部比例参数化，格阵语义在任意尺寸不变。

14. **v2.1 更名 Barmato → Winmato**：T 方案后 "Bar（任务栏）" 语义已不存在，载体是 Windows 托盘图标；一对一替换 Bar → Win、保留 -mato，品牌延续且构名自解释。全部标识同步更名：crate/可执行文件 `winmato`、配置目录 `%APPDATA%\Winmato`、互斥量 `Winmato.SingleInstance`、窗口类 `WinmatoHost`、菜单 id `winmato.exit`、托盘 title `Winmato`。

## 11. 不变量总清单（改代码前必读）

1. 四格 ≡ 四个专注番茄，格位 = 番茄序号（2×2 行优先），**绝不显示数字/文字**
2. 托盘图标是唯一交互入口且**恒可见**（待机恒为四格灰色空心「待填充」轮廓），左键启停 / 右键退出
3. 灌注永远从底部向上增长，最低 1px（progress=0 也有可见信号）
4. **边框永存**：每格 1px 轮廓为结构层、底色/灌注为内容层，内容层仅画 inset 1px 内区——任何填充状态下边框不被吞没
5. 休息格底色 = 专注色（已获得），灌注色 = 休息色（当前阶段）——绝不混淆
6. 脉冲每边界一次（stage_seq 边沿触发，含周期收尾待机边界）；呼吸仅暂停期；待机零动画
7. `stage_seq` 单调递增，跨周期不回绕
8. `completed` 仅在 4 枚番茄全部收获后置位（自然走完长休，或长休暂停后收束）、`toggle` 启动清零、运行态恒 false——点击路径不存在中途放弃；待机视觉不因 completed 分叉
9. 任何异常路径不弹窗、不 panic、不阻塞消息循环
10. 反目标清单（§1.2）神圣不可侵犯：无弹窗、无数字、无声音、无进度条
11. 像素全部在 size×size 画布内（`set_pixel` 边界检查，越界静默跳过）
12. 配置缺失/损坏/越界 → 静默回退默认，不报错不写回
13. 画布边长由任务栏厚度一次性推导：`size = min(taskbar_w, taskbar_h) − 2×TASKBAR_MARGIN(4)`，失败回退 SM_CXSMICON；格阵几何比例参数化：`SQ = size/3`、`GAP = max(size/16, 1)`、`OFF = (size − 2·SQ − GAP)/2`；标定 `geometry(40)=(13,2,6)`、`geometry(32)=(10,2,5)`、`geometry(24)=(8,1,3)`、`geometry(16)=(5,1,2)`；size<16 → 全透明（MIN_SIZE=16 兜底）
14. 动画常量不可漂移：`TRANSITION_SECS=0.6`、`PULSE_SECS=2.5`、`PULSE_PEAK=0.15`、`BREATH_PERIOD_SECS=12.0`、`BREATH_AMPLITUDE=0.10`、`PAUSE_DIM=0.5`
15. 默认配色不可漂移：Focus `(45,140,83)`、ShortBreak `(71,179,179)`、LongBreak `(85,133,200)`、IDLE_OUTLINE `(110,110,110)`
16. 帧率不变量：`TICK_MS=250`（常规）、`TICK_FAST_MS=50`（过渡/脉冲期）；仅在间隔实际变化时才调 `SetTimer`
