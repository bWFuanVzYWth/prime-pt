//! 主线程完整帧间隔归因。统计与按像素保峰复用引擎，宿主展示 PFT2 快照。
//!
//! 同步推进的源编译归入网格分项，包含解码、预算核算和合并。视图准备归入场景准备与提交。
//! AS 等待、属性分配及写入分别计时，单位是主线程墙钟。最近场景更新摘要独立于选中的历史帧。
//! Rust 通知去重统计从 Java 批量转发后开始。图隐藏时继续记录整帧，不构造快照。
//! 诊断读取不提交 GPU 命令或等待设备。

use crate::Error;
use simple_engine::frame_loop::timing::{FrameAttribution, FrameSample, FrameStats};
use std::time::Duration;

pub(crate) const STAGES: usize = 14;
pub(crate) const COLUMNS: usize = 512;
const DETAILS_OFFSET: usize = 344;
const DETAIL_BYTES: usize = 128;
pub(crate) const HEADER: usize = DETAILS_OFFSET + 3 * DETAIL_BYTES;
pub(crate) const BAR: usize = 120;

#[derive(Clone, Copy, Default)]
pub(crate) struct SceneInfo {
    pub dirty_regions: u64,
    pub instances: u64,
    pub triangle_bytes: u64,
    pub attribute_bytes: u64,
    pub as_wait_ns: u64,
    pub tlas_ns: u64,
    pub attribute_ns: u64,
}

#[derive(Default)]
pub(crate) struct Profile {
    stats: FrameStats<STAGES>,
    sequence: u64,
    // 视图准备、三角编码、属性打包、AS与共同等待、属性分配/写入、TLAS准备、提交/回收。
    pub scene: [u64; 7],
    // prime_plan内同步推进的建网属于网格项，从Java计入的规划墙钟中转移。
    pub plan_mesh_ns: u64,
    pub update: Option<SceneInfo>,
    last_update: Option<(u64, SceneInfo)>,
    pressure: prime_world::Pressure,
    work: prime_world::WorkStats,
    mesh_budget: usize,
}

impl Profile {
    pub(crate) fn set_work(&mut self, work: prime_world::WorkStats, mesh_budget: usize) {
        self.work = work;
        self.mesh_budget = mesh_budget;
    }
    pub(crate) fn set_pressure(&mut self, pressure: prime_world::Pressure) {
        let previous = self.pressure;
        if (
            previous.kind,
            previous.excluded_regions,
            previous.limit_bytes,
        ) != (
            pressure.kind,
            pressure.excluded_regions,
            pressure.limit_bytes,
        ) {
            eprintln!(
                "[prime_pt] mesh.pressure kind={:?} required={} limit={} excluded={} deferred={}",
                pressure.kind,
                pressure.required_bytes,
                pressure.limit_bytes,
                pressure.excluded_regions,
                pressure.deferred_requests
            );
        }
        self.pressure = pressure;
    }
    pub(crate) fn record(
        &mut self,
        elapsed_ns: u64,
        mut stages: [u64; STAGES],
    ) -> Result<(), Error> {
        if elapsed_ns > i64::MAX as u64 {
            return Err(Error::Input("PFT2帧间隔超过宿主有符号纳秒域"));
        }
        if elapsed_ns == 0 {
            self.stats.reset_samples();
            self.scene = [0; 7];
            self.plan_mesh_ns = 0;
            self.update = None;
            self.last_update = None;
            return Ok(());
        }
        let mesh = std::mem::take(&mut self.plan_mesh_ns).min(stages[2]);
        stages[2] -= mesh;
        stages[4] = stages[4].saturating_add(mesh);
        let scene = std::mem::take(&mut self.scene);
        let measured = scene
            .iter()
            .fold(0u64, |sum, value| sum.saturating_add(*value));
        stages[11] = stages[11].saturating_sub(measured);
        for (stage, ns) in [10, 5, 6, 7, 8, 9, 10].into_iter().zip(scene) {
            stages[stage] = stages[stage].saturating_add(ns);
        }
        self.stats.record_attributed(
            Duration::from_nanos(elapsed_ns),
            FrameAttribution::new(stages),
        )?;
        self.sequence = self.sequence.saturating_add(1).min(i64::MAX as u64);
        if let Some(update) = self.update.take()
            && self.stats.detail().latest.is_some()
        {
            self.last_update = Some((self.sequence, update));
        }
        Ok(())
    }

    /// PFT2 v5：344B头、三个128B详情、至多512个120B保峰柱；不持有宿主借用。
    pub(crate) fn write(&mut self, output: &mut [u8], columns: usize) -> Result<usize, Error> {
        if columns == 0 || columns > COLUMNS || output.len() < HEADER + columns * BAR {
            return Err(Error::Input("PFT2输出容量或柱数无效"));
        }
        let mut winners = [0; COLUMNS];
        let count = self.stats.compact_sample_indices(columns, &mut winners);
        let size = HEADER + count * BAR;
        let output = &mut output[..size];
        output.fill(0);
        put_u32(output, 0, 0x3254_4650);
        put_u32(output, 4, 5);
        put_u32(output, 8, STAGES as u32);
        put_u32(output, 12, count as u32);
        let snapshot = self.stats.snapshot();
        put_u64(output, 16, snapshot.sample_count as u64);
        put_u64(output, 24, self.sequence);
        for (offset, value) in [32, 40, 48, 56].into_iter().zip([
            snapshot.average_fps,
            snapshot.mean_ms,
            snapshot.p90_ms,
            snapshot.p99_ms,
        ]) {
            put_u64(output, offset, value.to_bits());
        }
        let detail = self.stats.detail();
        let mut mask = 0;
        for (index, sample) in [detail.latest, detail.latest_stall, detail.slowest]
            .into_iter()
            .enumerate()
        {
            if let Some(sample) = sample {
                mask |= 1 << index;
                write_sample(output, DETAILS_OFFSET + index * DETAIL_BYTES, 16, sample);
            }
        }
        put_u32(output, 88, mask);
        put_u32(output, 160, self.pressure.kind as u32);
        put_u32(output, 164, self.pressure.excluded_regions as u32);
        put_u64(output, 168, self.pressure.required_bytes as u64);
        put_u64(output, 176, self.pressure.limit_bytes as u64);
        put_u64(output, 184, self.pressure.deferred_requests as u64);
        let current = self.work.current;
        for (index, value) in [
            self.work.turn,
            self.mesh_budget as u64,
            current.mesh_slabs as u64,
            self.work.queued_slabs as u64,
            current.waiting_sources as u64,
            current.waiting_budget as u64,
            current.publications as u64,
            current.source_requests as u64,
            current.source_hits as u64,
            current.reused_slabs as u64,
            current.cancelled_slabs as u64,
            current.raw_invalidations as u64,
            current.unique_invalidations as u64,
            self.work.total.mesh_slabs as u64,
            self.work.total.publications as u64,
            current.empty_sections as u64,
            self.work.unavailable_sections as u64,
            self.work.inflight_jobs as u64,
            self.work.pending_captures as u64,
        ]
        .into_iter()
        .enumerate()
        {
            put_u64(output, 192 + index * 8, value.min(i64::MAX as u64));
        }
        if let Some((sequence, info)) = self.last_update {
            put_u32(output, 92, 1);
            for (index, value) in [
                sequence,
                info.dirty_regions,
                info.instances,
                info.triangle_bytes,
                info.attribute_bytes,
                info.as_wait_ns,
                info.tlas_ns,
                info.attribute_ns,
            ]
            .into_iter()
            .enumerate()
            {
                put_u64(output, 96 + index * 8, value);
            }
        }
        for (column, index) in winners[..count].iter().copied().enumerate() {
            let sample = self.stats.sample(index).expect("保峰索引来自同一统计窗口");
            write_sample(output, HEADER + column * BAR, 8, sample);
        }
        Ok(size)
    }
}

fn put_u32(output: &mut [u8], offset: usize, value: u32) {
    output[offset..offset + 4].copy_from_slice(&value.to_le_bytes());
}

fn put_u64(output: &mut [u8], offset: usize, value: u64) {
    output[offset..offset + 8].copy_from_slice(&value.to_le_bytes());
}

fn write_sample(output: &mut [u8], offset: usize, stages: usize, sample: FrameSample<STAGES>) {
    put_u64(output, offset, sample.elapsed_ns);
    for (index, value) in sample.attribution.0.into_iter().enumerate() {
        put_u64(output, offset + stages + index * 8, value);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn word(bytes: &[u8], offset: usize) -> u64 {
        u64::from_le_bytes(bytes[offset..offset + 8].try_into().unwrap())
    }

    #[test]
    fn nested_scene_wall_time_is_removed_from_recording_before_stacking() {
        let mut profile = Profile {
            scene: [10, 15, 5, 20, 5, 10, 5],
            ..Profile::default()
        };
        let mut stages = [0; STAGES];
        stages[11] = 100;
        profile.record(200, stages).unwrap();
        let mut bytes = vec![0; HEADER + BAR];
        profile.write(&mut bytes, 1).unwrap();
        let attribution: [u64; STAGES] =
            std::array::from_fn(|i| word(&bytes, DETAILS_OFFSET + 16 + i * 8));
        assert_eq!(
            attribution,
            [0, 0, 0, 0, 0, 15, 5, 20, 5, 10, 15, 30, 0, 100]
        );
        assert_eq!(
            attribution.iter().sum::<u64>(),
            word(&bytes, DETAILS_OFFSET)
        );
    }

    #[test]
    fn meshing_inside_plan_moves_to_mesh_without_changing_the_frame_total() {
        let mut profile = Profile {
            plan_mesh_ns: 60,
            ..Profile::default()
        };
        let mut stages = [0; STAGES];
        stages[2] = 100;
        stages[4] = 40;
        profile.record(140, stages).unwrap();
        let mut bytes = vec![0; HEADER + BAR];
        profile.write(&mut bytes, 1).unwrap();
        assert_eq!(word(&bytes, DETAILS_OFFSET + 16 + 2 * 8), 40);
        assert_eq!(word(&bytes, DETAILS_OFFSET + 16 + 4 * 8), 100);
        assert_eq!(word(&bytes, DETAILS_OFFSET), 140);
        assert_eq!(profile.plan_mesh_ns, 0);
    }

    #[test]
    fn chart_keeps_the_exact_nanoseconds_of_the_peak_sample() {
        let mut profile = Profile::default();
        for ns in [1, 100_000_001, 3] {
            profile.record(ns, [0; STAGES]).unwrap();
        }
        let mut bytes = vec![0; HEADER + BAR];
        assert_eq!(profile.write(&mut bytes, 1).unwrap(), HEADER + BAR);
        assert_eq!(word(&bytes, HEADER), 100_000_001);
        assert_eq!(word(&bytes, HEADER + 8 + 13 * 8), 100_000_001);
        assert_eq!(&bytes[64..88], &[0; 24]);
    }

    #[test]
    #[ignore = "生成PFT2供Gradle frameChartFixture验证生产native writer"]
    fn native_profile_fixture() {
        let mut profile = Profile::default();
        profile.set_work(
            prime_world::WorkStats {
                unavailable_sections: 90,
                inflight_jobs: 14,
                pending_captures: 7,
                ..Default::default()
            },
            128,
        );
        for ns in [10_000_000, 80_000_000, 12_000_000] {
            let mut stages = [0; STAGES];
            if ns == 80_000_000 {
                stages[7] = 65_000_000;
                stages[8] = 1_000_000;
                stages[9] = 2_000_000;
                profile.update = Some(SceneInfo {
                    dirty_regions: 2,
                    instances: 64,
                    triangle_bytes: 6 * 1024 * 1024,
                    attribute_bytes: 3 * 1024 * 1024,
                    as_wait_ns: 1_000_000,
                    tlas_ns: 2_000_000,
                    attribute_ns: 1_000_000,
                });
            }
            profile.record(ns, stages).unwrap();
        }
        profile.stats.refresh();
        let mut bytes = vec![0; HEADER + COLUMNS * BAR];
        let size = profile.write(&mut bytes, 240).unwrap();
        let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../target/native-frame-chart.pft");
        std::fs::write(path, &bytes[..size]).unwrap();
    }
}
