# Barmato 架构设计文档

> Bar + Tomato — 极简环境式番茄钟 for Windows
>
> 版本：1.0（对应 Rust 1.98 / 31 项测试全绿 / clippy 零警告）
>
> 配套规范：《Barmato 完整开发需求文档（AI 编程专用｜极简环境式正式版）》（下称"需求文档"，本文档以 §N 引用其章节）

---

## 1. 设计哲学（架构的最高约束）

所有架构决策从属于以下五条公理，冲突时按序裁决：

| # | 公理 | 架构含义 |
|---|------|----------|
| 1 | **深模块、浅接口** | 每个模块隐藏最大复杂度、暴露最小接口；主线 `main.rs` 只做粘合 |
| 2 | **快变量质量编码、慢变量空间编码、运动只标记事件** | 阶段进度→整条填充；番茄序号→4 刻点位置；阶段切换→单次脉冲；暂停→低频呼吸 |
| 3 | **消除特殊场景** | 不为小众场景加分支（副屏默认关、多阶段跳变一次结算、配置损坏整体回退） |
| 4 | **静默优雅降级** | 任何系统级异常自愈或回退默认态，绝不弹窗、不崩溃、不打断专注 |
| 5 | **极致轻量** | 待机零绘制；运行时帧率按需协商；热路径零堆分配；无 GC、无运行时 |

## 2. 系统总览

```
┌────────────────────────── Windows 单线程消息循环 ──────────────────────────┐
│                                                                            │
│  托盘事件                    WM_TIMER 心跳                  系统广播        │
│  (左键/右键)                250ms / 50ms 自适应          (TaskbarCreated   │
│      │                           │                        WM_DISPLAYCHANGE)│
│      ▼                           ▼                              │          │
│  ┌─────────┐  toggle()  ┌──────────────────┐  refresh_geometry()│          │
│  │ Cycle-  │◄──────────►│  App (薄主线)     │◄──────────────────┘          │
│  │ Machine │  snapshot()│  main.rs         │                              │
│  │ cycle.rs│───────────►│                  │                              │
│  └─────────┘  Snapshot  └───┬──────────┬───┘                              │
│   纯状态机                   │          │                                  │
│   零系统依赖                 ▼          ▼                                  │
│                   ┌────────────┐  ┌──────────────────┐                    │
│                   │ Tray 镜像  │  │ Overlay 渲染层    │                    │
│                   │ make_icon  │  │ overlay.rs       │                    │
│                   └────────────┘  │ softbuffer 光栅化 │                    │
│                                   └────────┬─────────┘                    │
│                                            │ 5px 细条                     │
└────────────────────────────────────────────┼──────────────────────────────┘
                                             ▼
                                   紧贴任务栏屏幕内侧
                                   (TOPMOST + 鼠标穿透)
```

**依赖方向**（单向，无环）：

```
main.rs ──► cycle.rs / overlay.rs / colors.rs / config.rs / taskbar.rs
overlay.rs ──► cycle.rs(仅 Snapshot 类型) / colors.rs
config.rs ──► colors.rs(Palette/Rgb)
cycle.rs / taskbar.rs ──► (无内部依赖，纯叶子模块)
```

模块间通信的唯一协议是 **`Snapshot` 值语义快照**——渲染层与托盘不感知计时，状态机不感知窗口。

## 3. 模块契约

### 3.1 cycle.rs — 周期状态机（纯业务，文档 §6.1）

| 项 | 内容 |
|----|------|
| 职责 | 番茄大周期时序、计时、暂停/继续、阶段流转、待机回归 |
| 依赖 | 仅 `std::time::Duration`，零 Windows API，可独立单元测试 |
| 状态 | `durations[8]` + `stage: 0..=8`（**8 = 待机哨兵**）+ `elapsed` + `paused` + `stage_seq: u64` |

**固定大周期**（文档 §2.1，不可乱序、不可跳过）：

```
stage:  0    1    2    3    4    5    6    7     8
        专注 短休 专注 短休 专注 短休 专注 长休  待机(哨兵)
        25m  5m  25m  5m  25m  5m  25m  20m
```

**对外接口（仅 3 个方法 + 快照）**：

- `toggle()`：唯一交互入口（§3.1）。待机→启动（`stage_seq += 1`，启动也是边界事件）；运行↔暂停
- `tick(delta)`：推进计时。暂停/待机完全冻结；**单次 tick 可跨多阶段**（系统休眠恢复），循环一次结算到位，每跨一阶段 `stage_seq += 1`
- `snapshot()`：返回渲染层唯一需要的信息：

```rust
pub struct Snapshot {
    pub idle: bool,
    pub paused: bool,
    pub phase: Phase,            // Focus | ShortBreak | LongBreak
    pub phase_progress: f64,     // 当前阶段进度 0.0..=1.0（快变量 → 整条填充）
    pub pomodoro_index: u8,      // 第几个专注番茄 1..=4（慢变量 → 刻点；待机 0）
    pub stage_seq: u64,          // 边界事件序号，单调递增 → 脉冲触发源
}
```

**设计要点**：

- 进度语义是**当前阶段**而非全周期（§4.2 修订）：25min 填满整条，回答用户最高频的问题"这个番茄还剩多久"
- `pomodoro_index` 是纯空间编码（刻点亮几个），**绝不显示数字**（§1.2 反目标）
- `stage_seq` 跨周期单调不回绕（待机快照仍携带真实序号），供渲染层做边沿检测
- 零时长配置兜底：`phase_progress = 1.0`，不 panic 不除零

### 3.2 overlay.rs — 渲染绘制层（深模块，文档 §6.4）

| 项 | 内容 |
|----|------|
| 职责 | 把 `Snapshot` + 阶段底色翻译为 5px 细条像素；全部动画状态机内埋于此 |
| 窗口 | 独立顶层 `WS_POPUP` + `WS_EX_LAYERED \| WS_EX_TRANSPARENT \| WS_EX_NOACTIVATE \| WS_EX_TOOLWINDOW` |
| 透明 | `SetLayeredWindowAttributes` + color-key（`#FF00FF`）→ 该色像素完全透明，透出任务栏原生底色 |
| Z 序 | `HWND_TOPMOST`；鼠标穿透（`WS_EX_TRANSPARENT`）→ 系统图标与交互永不受影响（§4.1） |
| 光栅 | softbuffer 软件光栅化，无 GPU/浏览器依赖 |

**几何自适应**（`bar_rect`）：从任务栏矩形推导细条位置——底部任务栏→细条在其上方；顶部→下方；左侧→右侧；右侧→左侧。主屏坐标系原点 (0,0) 保证 top==0 / left==0 判定精确。

**渲染管线**（每帧）：

```
Snapshot + 阶段底色
   │
   ▼ resolve_color() —— 全部明度/色彩修饰在此收敛
   │  1. 阶段切换插值：0.6s lerp（颜色不突变）
   │  2. 边界脉冲：2.5s 单次正弦，明度提亮 ≤15%（标记事件边界）
   │  3. 暂停：darken(0.4) + 12s 周期 ±10% 正弦呼吸
   ▼ draw()
   │  逐行填充：[0, fill_w-发光带) 提亮色 | 发光带 8px | [fill_w, w) 底色
   │  叠加 4 枚 2×2 刻点（1/8、3/8、5/8、7/8 处；亮=前沿亮度，灭=微弱可见）
   ▼ softbuffer present()
```

**动画状态机（内埋，主线零感知）**：

| 动画 | 触发 | 参数 | 帧率需求 |
|------|------|------|----------|
| 阶段过渡插值 | 目标色变化 | 0.6s | 快帧（50ms） |
| 边界脉冲 | `stage_seq` 边沿检测 | 2.5s，峰值 +15%，单次 | 快帧（50ms） |
| 暂停呼吸 | `paused` 上升沿 | 12s 周期 ±10%，持续 | 常规帧（250ms，48 步/周期，步进 <1.3% 幅值，低于感知阈值） |

**对主线的帧率协商接口**：

- `needs_continuous_render()` → 过渡/脉冲/呼吸任一进行中 → 主线持续重绘
- `needs_fast_frames()` → 仅过渡/脉冲 → 主线切 50ms 心跳（呼吸不需要快帧）

**关键决策：为何独立顶层窗口而非 `WS_CHILD`/`SetParent`**
任务栏属于 explorer.exe 线程，跨线程创建 `WS_CHILD` 返回 NULL；`SetParent` 对 `WS_EX_LAYERED` 窗口无法建立父子关系。独立窗口 + 精确 Z 序是唯一可靠方案。

**静默降级**：`render()` 任一步失败 → `hide()`（隐藏 + 释放 surface + 复位全部动画状态）；主线发现 `is_visible()==false` 自动 `show()` 恢复（§7）。

### 3.3 colors.rs — 色彩运算（纯函数）

- `Rgb`：`from_hex`（非法返回 None）/ `darken`（仅变暗不换色相）/ `lighten`（向白提亮保持色相）/ `lerp`（阶段过渡）/ `to_pixel`（softbuffer 0RGB 格式）
- `COLOR_KEY = #FF00FF`：透明键；配置层强制拦截阶段配色取该值（否则填充不可见）
- `IDLE_DOT = #5A5A5A`：待机托盘圆点——低存在感但**恒可见、恒可点击**，保障 §3.1 交互不变量（首次运行可发现入口）
- `Palette` 出厂默认（HSL 校准，低饱和低对比，余光可辨）：

| 阶段 | 色值 | HSL | 色相间隔 |
|------|------|-----|----------|
| Focus | `#3A6B4C` | H145 S31% L33% | — |
| ShortBreak | `#59A8A6` | H185 S30% L50% | +40° |
| LongBreak | `#6585B4` | H215 S28% L55% | +30° |

### 3.4 config.rs — 配置加载（静默降级，文档 §6.5）

- 只读加载 `%APPDATA%\Barmato\config.json`；单位为**秒**（发烧友可用 25s 快速验证全周期）
- 合法区间 1..=36000s；任何缺失/损坏/越界/非法色值 → **整体或字段级静默回退出厂默认**，不报错不写回
- 默认值单一事实来源：`DEFAULT_*_SECS` 常量，杜绝两处字面量漂移
- 支持字段级部分覆盖（只改 focus 不动其余）；拒绝与 COLOR_KEY 相同的色值

### 3.5 taskbar.rs — 任务栏几何探测（文档 §6.2）

- `EnumWindows` 按类名收集：主屏 `Shell_TrayWnd`、副屏 `Shell_SecondaryTrayWnd`
- 只产出渲染覆盖层需要的**唯一信息**：`hwnd + rect`（交互已迁移至托盘，不再做空白区判定）
- 主屏排序在前；`all_monitors=false` 时截断为仅主屏（§4.1 默认，砍掉多屏边界 case）
- 无效矩形（翻转/零尺寸）过滤返回 None

### 3.6 main.rs — 薄主线（粘合层）

**只做五件事**，全部复杂度下沉到深模块：

1. **单实例守卫**：命名互斥量 `Barmato.SingleInstance`；`GetLastError` 紧随 `CreateMutexW` 检查（任何中间 Win32 调用都会覆盖 LastError）
2. **DPI 声明**：`PerMonitorV2`，一次声明全程有效
3. **消息循环**：隐藏宿主窗口作载体；`App` 上下文挂 `GWLP_USERDATA`（Win32 正统，无全局可变状态）
4. **托盘**：tray-icon 同线程窗口；左键 Down → `toggle()`，右键菜单仅"退出"；**图标即状态镜像**（§4.2）：
   - 运行 = 阶段色相实心圆点（对阶段色提亮 20% 保证明暗托盘均可辨）
   - 暂停 = 空心圆环（色相不变）
   - 待机 = 中性暗灰圆点
   - 图标状态缓存 `last_icon`，仅变化时重建（4KB 分配即换即释）
5. **心跳调度**（`on_timer`）：

```
tick(Δt) → snapshot()
  → 几何校验：运行期每 1s / 待机期每 10s（极致轻量）或 geometry_dirty 立即
  → 重绘判定：snapshot 变化 OR 任一 overlay needs_continuous_render()
  → 帧率协商：任一 overlay needs_fast_frames() → SetTimer(50ms)，否则 250ms
     （仅间隔实际变化时才调 SetTimer，避免重复重置丢帧）
```

**系统广播处理**（wnd_proc）：

| 消息 | 响应 |
|------|------|
| `WM_DISPLAYCHANGE` / `WM_SETTINGCHANGE` | 置 `geometry_dirty`，下个心跳重建 |
| `TaskbarCreated`（explorer 重启） | 立即 `refresh_geometry()`，覆盖层按句柄+矩形匹配复用或重建 |
| `WM_TIMER` | `on_timer` |

**几何重建策略**：按 `hwnd + rect` 精确匹配旧覆盖层——均未变则复用（零开销），任一变化才重建覆盖层。

## 4. 核心时序：一次左键点击的生命周期

```
用户左键托盘
  → TrayIconEvent::Click(Left, Down)
  → app.on_toggle()
      → machine.toggle()          # 待机→stage 0，stage_seq+1
      → snapshot()                # idle=false, phase=Focus, progress=0, idx=1
      → apply(snap)
          → 托盘镜像：IDLE_DOT → Focus 绿色实心（set_icon）
          → 逐 Bar：覆盖层缺失/失效则重建 → show() → render()
              → update_anim_state: stage_seq 边沿 → 脉冲启动
              → resolve_color: 无过渡（首帧）→ 无脉冲峰值（t=0 增益 0）
              → draw: 进度 0 → 整条底色 + 4 刻点（1 亮 3 灭）
  → 主线检测 needs_fast_frames()=true → 切 50ms 心跳
  → 2.5s 脉冲正弦播放完 → needs_fast_frames()=false → 回 250ms
  → 此后每 250ms：progress 单调递增 → 快照变化 → 重绘一帧（4fps 恒定）
  → 阶段边界：tick 跨 stage → snapshot 变化（phase 变、progress 归零、seq+1）
      → 过渡插值 0.6s + 边界脉冲 2.5s + 刻点模式更新 + 托盘换色
  → 长休结束：stage=8 哨兵 → idle 快照 → hide() 全部覆盖层 + 托盘回暗灰
```

## 5. 线程与并发模型

**全程单线程**。Win32 消息循环驱动一切；tray-icon 事件经其内部窗口过程转发到同线程接收器，主线在循环中 `try_recv` 轮询。无锁、无通道、无后台线程——状态机的所有可变状态（`stage/elapsed/paused/stage_seq`）只被心跳触碰，天然无数据竞争。softbuffer surface 仅在同线程触碰。

## 6. 内存与资源管理（Rust 所有权，无 GC）

| 资源 | 获取 | 释放 | 泄漏路径核查 |
|------|------|------|--------------|
| 每帧像素缓冲 | softbuffer 内部复用 | — | **热路径零堆分配** |
| 托盘图标 | `make_icon` 仅状态变化时分配 4KB | `set_icon` 替换即析构 | 无累积 |
| 覆盖层 surface | `show` 时惰性创建 | `hide` 释放 / `Overlay::drop` 销毁 HWND | explorer 重启重建时旧实例 take+drop |
| 配置 | 启动加载一次 | 进程退出 | 无 |
| `stage_seq` | u64 递增 | — | 5.8 万年不回绕 |

待机态：零渲染 + 几何探测降频 10s/次——CPU 占用趋近于零。

## 7. 测试策略（31 项，按模块分层）

| 模块 | 数量 | 覆盖要点 |
|------|------|----------|
| cycle.rs | 10 | 待机初始 / 启动语义 / 暂停冻结 / **8 阶段全序+时长+刻点+seq** / 冻结点续跑 / 周期收尾重启 / **阶段内单调+四分点精确值** / **边界归零+seq 步进** / **休眠恢复跨阶段一次结算** / 零时长兜底 |
| overlay.rs | 9 | 四向任务栏几何推导 / 零尺寸与翻转矩形拒绝 / 脉冲 0→峰→0 单次正弦 / 呼吸 12s 周期 ±10% 四相位 / 刻点 1/8..7/8 均匀分布 |
| colors.rs | 5 | hex 解析含非法拒绝 / darken 只降亮度 / lerp 端点 / lighten 单调 / 0RGB 像素格式 |
| config.rs | 7 | 空/损坏/部分覆盖/秒级单位/越界回退/非法色回退/透明键拦截 |

测试哲学：**状态机的所有不变量用精确值断言钉死**（如四分点 `k/4.0` 精确相等——0.25/0.5/0.75 在 f64 中二进制精确表示）；动画数学独立成纯函数（`pulse_gain`/`breath_gain`/`tick_center_x`）使时序逻辑可脱离窗口系统测试。

## 8. 技术选型依据

| 选择 | 替代方案 | 否决理由 |
|------|----------|----------|
| 纯 Rust + windows-rs | Electron / Tauri | 浏览器运行时数 MB 驻留，违背极致轻量 |
| softbuffer 软件光栅 | GPU 渲染（wgpu/D3D） | 5px×屏宽的填充量用 CPU 逐行 fill 即可，GPU 初始化/上下文管理复杂度不成比例 |
| tray-icon crate | 自写 NOTIFYICONDATA | 托盘菜单/事件样板多，crate 已是极简封装且同线程 |
| 分层窗口 color-key | UpdateLayeredWindow / DirectComposition | color-key 一次设定永久生效，每帧只需写像素 |
| serde_json 只读配置 | 注册表 / 数据库 | 逃逸口定位（§2.2）：无 UI、文件即文档、损坏即回退 |

## 9. 异常处理矩阵（文档 §7 落地）

| 异常 | 检测 | 响应 | 用户感知 |
|------|------|------|----------|
| 资源管理器重启 | `TaskbarCreated` 广播 | 立即重建几何与覆盖层 | 短暂无感闪烁 |
| 任务栏位置/尺寸/DPI 变更 | 1s 周期轮询 + 系统广播 | 句柄/矩形比对，变化才重建 | 无 |
| 渲染失败（surface/缓冲） | `render()` 返回 Err | `hide()` 降级；主线检测后自动 `show()` 恢复 | 短暂消失后恢复 |
| 配置缺失/损坏/越界 | 解析失败 | 字段级或整体回退默认 | 无 |
| 多开 | 命名互斥量 | 第二实例静默退出 | 无 |
| 系统休眠恢复 | tick Δt 巨大 | 循环结算跨过的所有阶段，一次到位 | 进度直达正确位置 |
| 覆盖层父窗口失效 | `is_alive()` | `apply` 时静默重建 | 无 |

## 10. 已知有意取舍（记录在案）

1. **explorer 重启重建后多触发一次边界脉冲**：修复需跨重建持久化动画状态，复杂度收益比为负；一次脉冲兼作"渲染恢复"自检信号。
2. **副屏启发式**：`bar_rect` 对主屏精确（原点判定），副屏任务栏位置可能偏差——但副屏默认关闭渲染（§4.1），影响被产品决策预先消除。
3. **暂停呼吸 4fps**：48 步/12s 周期、步进 <1.3% 幅值，低于慢变感知阈值；提帧率只增功耗不增体验。

## 11. 不变量总清单（改代码前必读）

1. 细条**整条 = 当前阶段时长**，绝不回退为全周期映射
2. 4 刻点纯空间编码，**绝不出现数字/文字**
3. 托盘图标是唯一交互入口且**恒可见**（待机暗灰），镜像三态：实心彩点 / 空心 / 暗灰
4. 脉冲每边界一次；呼吸仅暂停期；待机零绘制
5. `stage_seq` 单调递增，跨周期不回绕
6. 阶段配色永不等于 `COLOR_KEY`
7. 任何异常路径不弹窗、不 panic、不阻塞消息循环
8. 反目标清单（§1.2）神圣不可侵犯
