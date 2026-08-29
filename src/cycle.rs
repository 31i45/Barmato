//! 周期状态机模块（文档 §6.1）
//!
//! 纯业务逻辑：番茄大周期时序、计时、状态流转、暂停/继续。
//! 零 Windows API 依赖、无 UI 耦合，可独立单元测试。
//!
//! 状态空间最小化（文档 §5）：
//! - 待机（stage == 8 哨兵值）
//! - 运行-专注 / 运行-短休 / 运行-长休（由 stage 索引决定）
//! - 暂停为叠加标记，不新增独立阶段

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

/// 对外快照：渲染层唯一需要的信息
#[derive(Clone, Copy, PartialEq, Debug)]
pub struct Snapshot {
    pub idle: bool,
    pub paused: bool,
    pub phase: Phase,
    /// 全周期连续进度 0.0..=1.0（细条整条宽度 = 一个完整番茄周期）
    pub progress: f64,
}

impl Snapshot {
    /// 待机快照。消费方无需知晓阶段概念即可表达空闲态（信息隐藏）
    pub fn idle() -> Self {
        Snapshot {
            idle: true,
            paused: false,
            phase: Phase::Focus,
            progress: 0.0,
        }
    }
}

pub struct CycleMachine {
    durations: [Duration; 8],
    /// 全周期总时长（8 阶段之和），用于计算连续进度
    total: Duration,
    /// 0..=8；8 表示待机（周期结束自动回归，文档 §2.1-9）
    stage: usize,
    elapsed: Duration,
    paused: bool,
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
        let total = durations.iter().sum();
        Self {
            durations,
            total,
            stage: STAGE_LAYOUT.len(),
            elapsed: Duration::ZERO,
            paused: false,
        }
    }

    /// 唯一交互入口（文档 §3.1）：待机→启动；运行→暂停；暂停→继续
    pub fn toggle(&mut self) {
        if self.idle() {
            self.stage = 0;
            self.elapsed = Duration::ZERO;
            self.paused = false;
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
            Snapshot::idle()
        } else {
            // 全周期连续进度：已完成阶段总时长 + 当前阶段已过时长
            let completed: Duration = self.durations[..self.stage].iter().sum();
            let progress = if self.total.is_zero() {
                1.0
            } else {
                ((completed + self.elapsed).as_secs_f64() / self.total.as_secs_f64())
                    .clamp(0.0, 1.0)
            };
            Snapshot {
                idle: false,
                paused: self.paused,
                phase: STAGE_LAYOUT[self.stage],
                progress,
            }
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

    #[test]
    fn starts_idle() {
        assert!(machine().snapshot().idle);
    }

    #[test]
    fn toggle_starts_focus() {
        let mut m = machine();
        m.toggle();
        let s = m.snapshot();
        assert!(!s.idle && !s.paused);
        assert_eq!(s.phase, Phase::Focus);
        // 全周期进度从 0.0 开始
        assert_eq!(s.progress, 0.0);
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
        assert_eq!(s1.progress, s2.progress);
    }

    #[test]
    fn stage_order_and_timing() {
        let mut m = machine();
        m.toggle();
        let expected = [
            (Phase::Focus, F),
            (Phase::ShortBreak, S),
            (Phase::Focus, F),
            (Phase::ShortBreak, S),
            (Phase::Focus, F),
            (Phase::ShortBreak, S),
            (Phase::Focus, F),
            (Phase::LongBreak, L),
        ];
        // 全周期连续进度：每个阶段起始处的 progress 应等于
        // 该阶段之前所有阶段时长之和 / 总时长
        let total: Duration = F * 4 + S * 3 + L;
        let mut elapsed_before = Duration::ZERO;
        for (phase, dur) in expected {
            let snap = m.snapshot();
            assert_eq!(snap.phase, phase);
            // 阶段起始处 progress 应精确等于已完成比例
            let expected_progress =
                elapsed_before.as_secs_f64() / total.as_secs_f64();
            assert!((snap.progress - expected_progress).abs() < 1e-9,
                "stage {:?}: progress {} != expected {}",
                phase, snap.progress, expected_progress);
            elapsed_before += dur;
            m.tick(dur);
        }
        assert!(m.snapshot().idle);
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
        assert_eq!(m.snapshot().phase, Phase::ShortBreak);
    }

    #[test]
    fn restart_after_completion() {
        let mut m = machine();
        m.toggle();
        m.tick(F + S + F + S + F + S + F + L + Duration::from_secs(1));
        assert!(m.snapshot().idle);
        m.toggle();
        let s = m.snapshot();
        assert_eq!(s.phase, Phase::Focus);
        assert_eq!(s.progress, 0.0);
    }

    /// 全周期进度单调递增验证：每个阶段内部 progress 必须大于阶段起始值
    #[test]
    fn progress_monotonic_across_cycle() {
        let mut m = machine();
        m.toggle();
        // 在每个阶段中间点采样，验证 progress 单调递增
        let mut prev_progress = 0.0f64;
        let stages = [
            (Phase::Focus, F),
            (Phase::ShortBreak, S),
            (Phase::Focus, F),
            (Phase::ShortBreak, S),
            (Phase::Focus, F),
            (Phase::ShortBreak, S),
            (Phase::Focus, F),
            (Phase::LongBreak, L),
        ];
        for (phase, dur) in stages {
            let half = dur / 2;
            m.tick(half);
            let snap = m.snapshot();
            assert_eq!(snap.phase, phase);
            assert!(snap.progress > prev_progress,
                "phase {:?}: progress {} should be > {}", phase, snap.progress, prev_progress);
            prev_progress = snap.progress;
            m.tick(dur - half);
        }
        // 周期结束后应回归待机
        assert!(m.snapshot().idle);
    }

    /// 验证全周期进度在阶段边界处的连续性：
    /// 前一阶段结束时的 progress 应等于下一阶段开始时的 progress
    #[test]
    fn progress_continuous_at_stage_boundaries() {
        let mut m = machine();
        m.toggle();
        let total: Duration = F * 4 + S * 3 + L;
        let stages = [
            (Phase::Focus, F),
            (Phase::ShortBreak, S),
            (Phase::Focus, F),
            (Phase::ShortBreak, S),
            (Phase::Focus, F),
            (Phase::ShortBreak, S),
            (Phase::Focus, F),
            (Phase::LongBreak, L),
        ];
        let mut elapsed_before = Duration::ZERO;
        for (phase, dur) in stages {
            // 推进到阶段最后一瞬间
            m.tick(dur - Duration::from_millis(1));
            let snap_end = m.snapshot();
            assert_eq!(snap_end.phase, phase);
            let expected_end =
                (elapsed_before + dur - Duration::from_millis(1)).as_secs_f64()
                    / total.as_secs_f64();
            assert!((snap_end.progress - expected_end).abs() < 1e-6,
                "phase {:?} end: progress {} != expected {}",
                phase, snap_end.progress, expected_end);
            // 跨越阶段边界
            m.tick(Duration::from_millis(1));
            elapsed_before += dur;
        }
        assert!(m.snapshot().idle);
    }
}
