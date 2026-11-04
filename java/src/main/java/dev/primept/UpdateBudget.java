package dev.primept;

/** 场景更新工作量档位；捕获只按 section 数截断，wireId 与 native 一致。 */
enum UpdateBudget {
    SMOOTH("smooth", 0, 16),
    BALANCED("balanced", 1, 32),
    THROUGHPUT("throughput", 2, 64);

    final String key;
    final int wireId;
    final int captures;

    UpdateBudget(String key, int wireId, int captures) {
        this.key = key;
        this.wireId = wireId;
        this.captures = captures;
    }

    static UpdateBudget parse(String key) {
        for (var budget : values()) if (budget.key.equals(key)) return budget;
        throw new IllegalArgumentException("未知场景更新额度");
    }
}
