//! 周期状态机模块（文档 §6.1）
//!
//! 纯业务逻辑：番茄大周期时序、计时、状态流转、暂停/继续。
//! 零 Windows API 依赖、无 UI 耦合，可独立单元测试。
//!
//! 状态空间最小化（文档 §5）：
//! - 待机（stage == 8 哨兵值）
//! - 运行-专注 / 运行-短休 / 运行-长休（由 stage 索引决定）
//! - 暂停为叠加标记，不新增独立阶段
//!
//! 进度语义（文档 §4.2）：快照对外提供**当前阶段**进度（phase_progress）；
//! 全周期位置由「第几个专注番茄」（pomodoro_index，4 枚刻点的空间编码）表达；
//! 阶段边界事件通过单调递增的 stage_seq 暴露给渲染层（边界脉冲触发源，§4.3）。

use std::time::Duration;

/// 番茄阶段（文档 §2.1 / §4.2）
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Phase {
    Focus,
    ShortBreak,
    LongBreak,
}

/// 固定大周期：专注→短休 ×3 → 专注 → 长休（文档 §2.1，顺序不可乱、不可跳）
const STAGE_LAYOUT: [Phase; 8] = [
    Phase::Focus,
    Phase::ShortBreak,
    Phase::Focus,
    Phase::ShortBreak,
    Phase::Focus,
    Phase::ShortBreak,
    Phase::Focus,
    Phase::LongBreak,
];

/// 对外快照：渲染层与托盘镜像唯一需要的信息
#[derive(Clone, Copy, PartialEq, Debug)]
pub struct Snapshot {
    pub idle: bool,
    pub paused: bool,
    pub phase: Phase,
    /// 当前阶段进度 0.0..=1.0（细条整条宽度 = 当前阶段时长，文档 §4.2）
    pub phase_progress: f64,
    /// 第几个专注番茄：运行态 1..=4（短休/长休保持已完成计数），待机为 0。
    /// 渲染为刻点的空间模式，绝不显示数字（文档 §1.2 / §4.2）
    pub pomodoro_index: u8,
    /// 阶段序号：每次阶段推进（含待机→首个专注）单调递增，
    /// 渲染层据此探测阶段边界并触发边界脉冲（文档 §4.3）
    pub stage_seq: u64,
}

impl Snapshot {
    /// 待机快照。消费方无需知晓阶段概念即可表达空闲态（信息隐藏）
    pub fn idle() -> Self {
        Snapshot {
            idle: true,
            paused: false,
            phase: Phase::Focus,
            phase_progress: 0.0,
            pomodoro_index: 0,
            stage_seq: 0,
        }
    }
}

pub struct CycleMachine {
    durations: [Duration; 8],
    /// 0..=8；8 表示待机（周期结束自动回归，文档 §2.1-9）
    stage: usize,
    elapsed: Duration,
    paused: bool,
    /// 单调递增阶段序号（含启动），快照携带以供边界事件检测
    stage_seq: u64,
}

impl CycleMachine {
    pub fn new(focus: Duration, short_break: Duration, long_break: Duration) -> Self {
        let mut durations = [Duration::ZERO; 8];
        for (i, phase) in STAGE_LAYOUT.iter().enumerate() {
            durations[i] = match phase {
                Phase::Focus => focus,
                Phase::ShortBreak => short_break,
                Phase::LongBreak => long_break,
            };
        }
        Self {
            durations,
            stage: STAGE_LAYOUT.len(),
            elapsed: Duration::ZERO,
            paused: false,
            stage_seq: 0,
        }
    }

    /// 唯一交互入口（文档 §3.1）：待机→启动；运行→暂停；暂停→继续
    pub fn toggle(&mut self) {
        if self.idle() {
            self.stage = 0;
            self.elapsed = Duration::ZERO;
            self.paused = false;
            self.stage_seq += 1; // 待机→首个专注同样是一次阶段边界
        } else {
            self.paused = !self.paused;
        }
    }

    /// 推进计时。暂停/待机时完全冻结（文档 §3.1 / §4.2）
    pub fn tick(&mut self, delta: Duration) {
        if self.idle() || self.paused {
            return;
        }
        self.elapsed += delta;
        // 单次 tick 可能跨越多个阶段（如系统休眠恢复），循环结算
        while self.stage < STAGE_LAYOUT.len() {
            let dur = self.durations[self.stage];
            if self.elapsed < dur {
                break;
            }
            self.elapsed -= dur;
            self.stage += 1;
            self.stage_seq += 1;
        }
        if self.idle() {
            // 周期收尾：自动回归待机（文档 §2.1-9）
            self.elapsed = Duration::ZERO;
            self.paused = false;
        }
    }

    pub fn idle(&self) -> bool {
        self.stage >= STAGE_LAYOUT.len()
    }

    pub fn snapshot(&self) -> Snapshot {
        if self.idle() {
            // 待机仍携带真实 stage_seq，保证跨周期单调不回绕
            return Snapshot {
                stage_seq: self.stage_seq,
                ..Snapshot::idle()
            };
        }
        let dur = self.durations[self.stage];
        // 当前阶段进度：细条整条 = 当前阶段时长（文档 §4.2）
        let phase_progress = if dur.is_zero() {
            1.0
        } else {
            (self.elapsed.as_secs_f64() / dur.as_secs_f64()).clamp(0.0, 1.0)
        };
        // 第几个专注番茄：已历经（含当前）的专注阶段个数（刻点空间编码）
        let pomodoro_index = STAGE_LAYOUT[..=self.stage]
            .iter()
            .filter(|p| **p == Phase::Focus)
            .count() as u8;
        Snapshot {
            idle: false,
            paused: self.paused,
            phase: STAGE_LAYOUT[self.stage],
            phase_progress,
            pomodoro_index,
            stage_seq: self.stage_seq,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const F: Duration = Duration::from_secs(25 * 60);
    const S: Duration = Duration::from_secs(5 * 60);
    const L: Duration = Duration::from_secs(20 * 60);

    fn machine() -> CycleMachine {
        CycleMachine::new(F, S, L)
    }

    /// 8 阶段期望序列：阶段、时长、刻点序号（文档 §2.1 / §4.2 刻点空间编码）
    const STAGES: [(Phase, Duration, u8); 8] = [
        (Phase::Focus, F, 1),
        (Phase::ShortBreak, S, 1),
        (Phase::Focus, F, 2),
        (Phase::ShortBreak, S, 2),
        (Phase::Focus, F, 3),
        (Phase::ShortBreak, S, 3),
        (Phase::Focus, F, 4),
        (Phase::LongBreak, L, 4),
    ];

    #[test]
    fn starts_idle() {
        let s = machine().snapshot();
        assert!(s.idle);
        assert_eq!(s.phase_progress, 0.0);
        assert_eq!(s.pomodoro_index, 0); // 待机无刻点（文档 §4.2）
        assert_eq!(s.stage_seq, 0);
    }

    #[test]
    fn toggle_starts_focus() {
        let mut m = machine();
        m.toggle();
        let s = m.snapshot();
        assert!(!s.idle && !s.paused);
        assert_eq!(s.phase, Phase::Focus);
        // 阶段进度从 0.0 开始（细条 = 当前阶段，文档 §4.2）
        assert_eq!(s.phase_progress, 0.0);
        assert_eq!(s.pomodoro_index, 1); // 第 1 枚刻点亮起
        assert_eq!(s.stage_seq, 1); // 待机→首个专注也是边界事件
    }

    #[test]
    fn pause_freezes_time() {
        let mut m = machine();
        m.toggle();
        m.tick(Duration::from_secs(60));
        m.toggle(); // 暂停
        let s1 = m.snapshot();
        m.tick(Duration::from_secs(500));
        let s2 = m.snapshot();
        assert!(s1.paused && s2.paused);
        assert_eq!(s1.phase_progress, s2.phase_progress);
        assert_eq!(s1.stage_seq, s2.stage_seq); // 暂停不是阶段边界
    }

    /// 阶段顺序、时长、刻点序号、边界序号全量验证（文档 §2.1 / §4.2 / §4.3）
    #[test]
    fn stage_order_timing_and_ticks() {
        let mut m = machine();
        m.toggle();
        let mut expected_seq = 1u64;
        for (phase, dur, ticks) in STAGES {
            let snap = m.snapshot();
            assert_eq!(snap.phase, phase);
            assert_eq!(snap.pomodoro_index, ticks, "phase {:?}", phase);
            assert_eq!(snap.stage_seq, expected_seq);
            // 阶段起始处，当前阶段进度精确为 0
            assert_eq!(snap.phase_progress, 0.0);
            m.tick(dur);
            expected_seq += 1;
        }
        let end = m.snapshot();
        assert!(end.idle);
        assert_eq!(end.stage_seq, expected_seq); // 长休结束也推进了边界序号
    }

    #[test]
    fn resume_continues_from_frozen_point() {
        let mut m = machine();
        m.toggle();
        m.tick(Duration::from_secs(100));
        m.toggle(); // 暂停
        m.toggle(); // 继续
        assert!(!m.snapshot().paused);
        m.tick(F - Duration::from_secs(100));
        let s = m.snapshot();
        assert_eq!(s.phase, Phase::ShortBreak);
        assert_eq!(s.phase_progress, 0.0);
        assert_eq!(s.pomodoro_index, 1);
    }

    #[test]
    fn restart_after_completion() {
        let mut m = machine();
        m.toggle();
        m.tick(F + S + F + S + F + S + F + L + Duration::from_secs(1));
        assert!(m.snapshot().idle);
        let seq_before = m.snapshot().stage_seq;
        m.toggle();
        let s = m.snapshot();
        assert_eq!(s.phase, Phase::Focus);
        assert_eq!(s.phase_progress, 0.0);
        assert_eq!(s.pomodoro_index, 1);
        assert_eq!(s.stage_seq, seq_before + 1); // 重启也是新边界，序号单调不回绕
    }

    /// 当前阶段进度在阶段内部单调递增，且中点约为 0.5
    #[test]
    fn phase_progress_monotonic_within_stage() {
        let mut m = machine();
        m.toggle();
        for (phase, dur, _ticks) in STAGES {
            let quarters = dur / 4;
            let mut prev = 0.0f64;
            for k in 1..4 {
                m.tick(quarters);
                let snap = m.snapshot();
                assert_eq!(snap.phase, phase);
                let expected = k as f64 / 4.0;
                assert!(
                    (snap.phase_progress - expected).abs() < 1e-9,
                    "phase {:?}: progress {} != expected {}",
                    phase,
                    snap.phase_progress,
                    expected
                );
                assert!(snap.phase_progress > prev);
                prev = snap.phase_progress;
            }
            m.tick(dur - quarters * 3);
        }
        assert!(m.snapshot().idle);
    }

    /// 阶段进度在边界处重置：阶段末尾趋近 1.0，跨入下一阶段后归零，
    /// 且 stage_seq 恰好 +1（文档 §4.3 边界脉冲触发源）
    #[test]
    fn phase_progress_resets_at_boundaries() {
        let mut m = machine();
        m.toggle();
        for (phase, dur, _ticks) in STAGES {
            // 推进到阶段最后一瞬间
            m.tick(dur - Duration::from_millis(1));
            let snap_end = m.snapshot();
            assert_eq!(snap_end.phase, phase);
            let end_expected = (dur - Duration::from_millis(1)).as_secs_f64() / dur.as_secs_f64();
            assert!((snap_end.phase_progress - end_expected).abs() < 1e-6);
            // 跨越边界：进度归零、序号 +1
            let seq_before = snap_end.stage_seq;
            m.tick(Duration::from_millis(1));
            let snap_next = m.snapshot();
            if !snap_next.idle {
                assert_eq!(snap_next.phase_progress, 0.0);
            }
            assert_eq!(snap_next.stage_seq, seq_before + 1);
        }
        assert!(m.snapshot().idle);
    }

    /// 单次 tick 跨越多阶段（系统休眠恢复）：一次结算到位，
    /// 刻点与边界序号同步跳过（文档 §7 优雅降级）
    #[test]
    fn multi_stage_skip_in_single_tick() {
        let mut m = machine();
        m.toggle();
        m.tick(F + S + Duration::from_secs(10));
        let s = m.snapshot();
        assert_eq!(s.phase, Phase::Focus);
        assert_eq!(s.pomodoro_index, 2); // 直接进入第 2 个专注番茄
        assert_eq!(s.stage_seq, 3); // 启动 1 次 + 跳过 2 个阶段
        assert!((s.phase_progress - 10.0 / F.as_secs_f64()).abs() < 1e-9);
    }

    /// 零时长配置不 panic、不除零（配置层已拦截，状态机兜底）
    #[test]
    fn zero_durations_do_not_panic() {
        let mut m = CycleMachine::new(Duration::ZERO, Duration::ZERO, Duration::ZERO);
        m.toggle();
        let s = m.snapshot();
        assert_eq!(s.phase_progress, 1.0);
        m.tick(Duration::from_secs(1));
        assert!(m.snapshot().idle);
    }
}
