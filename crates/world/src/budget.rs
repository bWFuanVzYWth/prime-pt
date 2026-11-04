//! CPU quad payload的容量策略；不冒充源缓存、metadata、RSS或GPU显存总上限。
/// 可配置的最终常驻、过渡和单region逻辑payload上限，单位为字节。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Budget {
    /// 当前目标域已发布quad与获准候选的正增长总量。发布前不能用候选缩小额换取容量。
    /// 压缩源后台接纳上限另留 `min(resident/64, staging/8)`，且至少容纳单region上限。
    pub resident_bytes: usize,
    /// 所有实际独占quad可在resident之外使用的过渡容量，含域外旧覆盖、候选及退役。
    /// 压缩源后台不能使用其中 `min(region, staging/4)` 的交互输出余量。
    pub staging_bytes: usize,
    /// 单个region可提交的payload；不能大于staging_bytes。
    pub region_bytes: usize,
}
impl Default for Budget {
    fn default() -> Self {
        Self {
            resident_bytes: 1024 * 1024 * 1024,
            staging_bytes: 256 * 1024 * 1024,
            region_bytes: 256 * 1024 * 1024,
        }
    }
}

/// 当前压力原因；已明确不可容纳的region与临时等待回收分开报告。
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
#[repr(u32)]
pub enum PressureKind {
    /// 没有拒绝或等待。
    #[default]
    None = 0,
    /// 当前目标域的已知最终payload超出常驻预算。
    Resident = 1,
    /// 过渡联合payload等待额外容量或可安全发布的替换；未回复ticket及旧覆盖保持有效。
    Transition = 2,
    /// 单region输出超过独立region预算。
    Region = 3,
}

/// 不抛异常的CPU容量状态，供宿主图表、日志及配置界面消费。
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Pressure {
    /// 当前最近一次容量阻碍。
    pub kind: PressureKind,
    /// 当前已知需要的逻辑payload字节数。
    pub required_bytes: usize,
    /// 对应原因的配置上限。
    pub limit_bytes: usize,
    /// 明确因预算暂缓替换的region数量；有旧mesh时仍保留它，不充当空源。
    pub excluded_regions: usize,
    /// 因等待回收暂停返回的待捕获请求数。
    pub deferred_requests: usize,
}

/// 一次渲染推进的确定性工作额度；`plan` 开始新一轮，后续源包共享余额。
///
/// slab 是不可抢占的四层内核，额度不承诺固定墙钟耗时。零值只暂停对应工作域；
/// 缺源、预算不足或已完成的任务不占用 slab token，也不得阻挡其余可执行任务。
/// 整包输入验证/实际状态差分、窗口真实前沿枚举和State扩容另按其实际输入规模计费，
/// 不伪装为单个管理器访问或单个slab；宿主捕获仍最多64条在途请求。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct WorkBudget {
    /// 最多执行的 slab 内核数量，uniform air 的零输出证明不消耗此项。
    pub mesh_slabs: usize,
    /// 交给通用管理器的逻辑访问额度。
    pub planner_visits: usize,
    /// 最多准备并发布的 region 变化数量。
    pub publications: usize,
    /// 几何发布批次允许重建的 quad 数，单个超额 region 单独推进。
    pub publish_geometry_quads: usize,
    /// 属性发布批次允许写入的 quad 数，单个超额 region 单独推进。
    pub publish_attribute_quads: usize,
    /// 源就绪、颜色续接、通知/失效和退休各队列每轮的独立推进额度。
    pub maintenance: usize,
}

impl Default for WorkBudget {
    fn default() -> Self {
        Self {
            mesh_slabs: 128,
            planner_visits: 1024,
            publications: 64,
            publish_geometry_quads: 131_072,
            publish_attribute_quads: 262_144,
            maintenance: 256,
        }
    }
}

/// 当前推进轮的实际工作量；用于吞吐与公平性验收，不是 GPU timestamp。
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct WorkProgress {
    /// 本轮接收的原始来源通知数量。
    pub raw_invalidations: usize,
    /// 合并后实际处理的独立来源通知数量。
    pub unique_invalidations: usize,
    /// 本轮新签发的来源捕获数量。
    pub source_requests: usize,
    /// 依赖判断直接命中已缓存来源的次数。
    pub source_hits: usize,
    /// 本轮进入可执行队列的 slab 数量。
    pub ready_slabs: usize,
    /// 真正执行的 slab 内核数量。
    pub mesh_slabs: usize,
    /// 已证明无需内核的空 section 数量。
    pub empty_sections: usize,
    /// 共享只读输入的一次工作池提交数量。
    pub mesh_waves: usize,
    /// 编辑后保留并复用的候选 slab 数量。
    pub reused_slabs: usize,
    /// 因实际依赖变化取消的候选 slab 数量。
    pub cancelled_slabs: usize,
    /// 本轮遇到缺失源的任务数量。
    pub waiting_sources: usize,
    /// 本轮遇到容量等待的任务数量。
    pub waiting_budget: usize,
    /// 已发布的 region 变化数量。
    pub publications: usize,
    /// 已退休的压缩源数量。
    pub retired_sources: usize,
    /// 全槽预算扫描次数；增量账实现保持为零。
    pub budget_scans: usize,
    /// 全槽预算估算访问数；不包含通用管理器自身的逻辑步。
    pub budget_slot_visits: usize,
}

/// 不遍历状态即可读取的推进诊断；单轮与会话累计量分别保存。
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct WorkStats {
    /// 由 `plan` 开始的推进轮序号。
    pub turn: u64,
    /// 当前轮累计；同轮后续源包与颜色回填继续记在此处。
    pub current: WorkProgress,
    /// 已结束轮次与当前轮合计；世界 clear 不改变会话累计身份。
    pub total: WorkProgress,
    /// 当前真正可执行的队列长度。
    pub queued_slabs: usize,
    /// 当前需求域中已确认缺源的 section 数，含已完成但尚未发布的候选。
    pub unavailable_sections: usize,
    /// 等待源、建网、颜色或容量的活跃 region 工作数。
    pub inflight_jobs: usize,
    /// 已签发但尚未收到宿主回应的源捕获数。
    pub pending_captures: usize,
}

/// O(1)可观测所有权账与间接输入规模；各项不是进程RSS或GPU显存估算。
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct PayloadStats {
    /// 活跃或等待事件的独占job元数据数量，不受每轮8个新签发额度误限。
    pub inflight_jobs: usize,
    /// 已签发仍待宿主回复的源捕获数，最多64。
    pub pending_captures: usize,
    /// 当前/候选/job/退役实际独占的quad数量，乘96得到受预算控制的逻辑payload。
    pub owned_quads: usize,
    /// 当前目标域若发布全部已知候选后的quad数量；借用继承不重复计费。
    pub projected_quads: usize,
    /// 有效或待差分旧压缩源的协议payload字节数，不含分配余量。
    pub source_bytes: usize,
    /// 实际源region页数量，Air只保页内位图与版本。
    pub source_pages: usize,
    /// 源页内实际存在的section位数量。
    pub source_sections: usize,
    /// 源与在途工作的真实依赖边数。
    pub dependency_edges: usize,
    /// 管理器已分配节点容量，按实际阻碍增长。
    pub state_capacity: usize,
    /// 实际占用节点数，包含在途和待回收节点。
    pub occupied_nodes: usize,
    /// 管理器自身及预留缓冲字节数，包含资源内联字段，不含其间接Vec。
    pub state_reserved_bytes: usize,
}

impl WorkProgress {
    pub(crate) const fn combined(self, other: Self) -> Self {
        Self {
            raw_invalidations: self
                .raw_invalidations
                .saturating_add(other.raw_invalidations),
            unique_invalidations: self
                .unique_invalidations
                .saturating_add(other.unique_invalidations),
            source_requests: self.source_requests.saturating_add(other.source_requests),
            source_hits: self.source_hits.saturating_add(other.source_hits),
            ready_slabs: self.ready_slabs.saturating_add(other.ready_slabs),
            mesh_slabs: self.mesh_slabs.saturating_add(other.mesh_slabs),
            empty_sections: self.empty_sections.saturating_add(other.empty_sections),
            mesh_waves: self.mesh_waves.saturating_add(other.mesh_waves),
            reused_slabs: self.reused_slabs.saturating_add(other.reused_slabs),
            cancelled_slabs: self.cancelled_slabs.saturating_add(other.cancelled_slabs),
            waiting_sources: self.waiting_sources.saturating_add(other.waiting_sources),
            waiting_budget: self.waiting_budget.saturating_add(other.waiting_budget),
            publications: self.publications.saturating_add(other.publications),
            retired_sources: self.retired_sources.saturating_add(other.retired_sources),
            budget_scans: self.budget_scans.saturating_add(other.budget_scans),
            budget_slot_visits: self
                .budget_slot_visits
                .saturating_add(other.budget_slot_visits),
        }
    }
}
