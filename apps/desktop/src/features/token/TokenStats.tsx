import { BarChart3, RefreshCw } from "lucide-react";
import { useCallback, useEffect, useMemo, useRef, useState } from "react";
import { Button, Field, Panel } from "../../components/ui";
import { compactPath, formatTime, formatTokens } from "../../lib/format";
import type { CompanionStatus, TokenUsageBucket, TokenUsageEvent, TokenUsageSummary } from "../../types/domain";

export function TokenStats({
  active,
  status,
  onLoad,
}: {
  active: boolean;
  status: CompanionStatus;
  onLoad: (codexDir?: string) => Promise<TokenUsageSummary>;
}) {
  const [codexDir, setCodexDir] = useState(status.codex.codexDir);
  const [stats, setStats] = useState<TokenUsageSummary | null>(null);
  const [loading, setLoading] = useState(false);
  const [refreshing, setRefreshing] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const inFlightRef = useRef(false);

  const load = useCallback(async (mode: "manual" | "silent" = "manual") => {
    if (inFlightRef.current) return;
    inFlightRef.current = true;
    const showFullLoading = mode === "manual" || !stats;
    if (showFullLoading) {
      setLoading(true);
    } else {
      setRefreshing(true);
    }
    setError(null);
    try {
      setStats(await onLoad(codexDir));
    } catch (unknownError) {
      setError(String(unknownError));
    } finally {
      inFlightRef.current = false;
      setLoading(false);
      setRefreshing(false);
    }
  }, [codexDir, onLoad, stats]);

  useEffect(() => {
    if (!active || stats || loading) return;
    const timer = window.setTimeout(() => {
      void load("silent");
    }, 300);
    return () => window.clearTimeout(timer);
  }, [active, load, loading, stats]);

  useEffect(() => {
    if (!active) return;
    const timer = window.setInterval(() => {
      void load("silent");
    }, 30_000);
    return () => window.clearInterval(timer);
  }, [active, load]);

  const maxModelTokens = useMemo(() => maxTokens(stats?.byModel), [stats]);
  const maxProviderTokens = useMemo(() => maxTokens(stats?.byProvider), [stats]);
  const maxDayTokens = useMemo(() => maxTokens(stats?.byDay), [stats]);
  const todayKey = useMemo(() => new Date().toLocaleDateString("sv-SE"), []);
  const todayBucket = stats?.byDay.find((bucket) => bucket.key === todayKey);
  const unpricedModelText = stats?.unpricedModels.join("、") ?? "";
  const todayCostLabel = todayBucket?.unpricedEvents ? "今日已定价成本" : "今日估算成本";
  const totalCostLabel = stats?.unpricedEvents ? "已定价估算成本" : "总估算成本";

  function handleRefresh(): void {
    void load();
  }

  return (
    <div className="content-grid">
      <Panel eyebrow="用量" title="Token 统计">
        <Field label="Codex 目录">
          <input value={codexDir} onChange={(event) => setCodexDir(event.target.value)} />
        </Field>
        <div className="actions">
          <Button disabled={loading} onClick={handleRefresh} variant="secondary">
            <RefreshCw aria-hidden="true" size={15} /> {loading ? "扫描中" : "重新扫描"}
          </Button>
          {refreshing ? <span className="inline-muted">后台更新中</span> : null}
        </div>
        {error ? <div className="error-banner">{error}</div> : null}
        {!stats && loading ? <p className="empty">正在后台扫描 Codex 会话记录，页面其它操作不受影响。</p> : null}
        {!stats && !loading && !error ? <p className="empty">进入页面后会在后台扫描一次，也可以手动重新扫描。</p> : null}
        {stats?.unpricedEvents ? (
          <div className="warning-box">
            <strong>{stats.unpricedEvents} 条 Token 事件尚未定价</strong>
            <p>
              当前成本只汇总已匹配价格的事件。未定价模型：{unpricedModelText}。可在 Companion 数据目录创建 model-pricing.json 补充价格。
            </p>
          </div>
        ) : null}
        <div className="metric-grid details-top">
          <Metric label="今日 Token" value={formatTokens(todayBucket?.totalTokens ?? 0)} />
          <Metric label="总 Token" value={formatTokens(stats?.totalTokens ?? 0)} />
          <Metric label={todayCostLabel} value={formatUsd(todayBucket?.cost.totalUsd)} />
          <Metric label={totalCostLabel} value={formatUsd(stats?.cost.totalUsd)} />
          <Metric label="新输入" value={formatTokens(stats?.inputTokens ?? 0)} />
          <Metric label="缓存输入" value={formatTokens(stats?.cachedInputTokens ?? 0)} />
          <Metric label="输出" value={formatTokens(stats?.outputTokens ?? 0)} />
        </div>
      </Panel>

      <Panel eyebrow="范围" title="扫描范围">
        <dl className="details-grid">
          <dt>目录</dt>
          <dd>{compactPath(stats?.codexDir ?? codexDir)}</dd>
          <dt>文件</dt>
          <dd>{stats?.filesScanned ?? 0} 个 JSONL</dd>
          <dt>Session</dt>
          <dd>{stats?.sessions ?? 0}</dd>
          <dt>Token 事件</dt>
          <dd>{stats?.events ?? 0}</dd>
          <dt>已定价事件</dt>
          <dd>{stats?.pricedEvents ?? 0}</dd>
          <dt>新输入成本</dt>
          <dd>{formatUsd(stats?.cost.freshInputUsd)}</dd>
          <dt>缓存输入成本</dt>
          <dd>{formatUsd(stats?.cost.cachedInputUsd)}</dd>
          <dt>输出成本</dt>
          <dd>{formatUsd(stats?.cost.outputUsd)}</dd>
          <dt>价格快照</dt>
          <dd>{stats?.pricingAsOf ?? "—"}</dd>
          <dt>价格覆盖</dt>
          <dd>{stats?.pricingOverridePath ? compactPath(stats.pricingOverridePath) : "未启用"}</dd>
        </dl>
      </Panel>

      <Panel eyebrow="模型" title="按模型">
        <BucketList buckets={stats?.byModel ?? []} max={maxModelTokens} />
      </Panel>

      <Panel eyebrow="来源" title="按 Provider">
        <BucketList buckets={stats?.byProvider ?? []} max={maxProviderTokens} />
      </Panel>

      <Panel eyebrow="日期" title="按日期">
        <BucketList buckets={stats?.byDay ?? []} max={maxDayTokens} />
      </Panel>

      <Panel eyebrow="最近" title="最近 token 事件">
        {stats?.recentEvents.length ? (
          <div className="table-list" role="list">
            {stats.recentEvents.slice().reverse().map((event) => (
              <div className="table-row" key={tokenEventKey(event)} role="listitem">
                <div>
                  <strong>{event.model}</strong>
                  <span>{event.providerId ?? "unknown"} · {formatTime(event.timestamp)}</span>
                  <small className="token-event-usage">
                    本次 {formatTokens(event.totalTokens)} · {tokenEventBreakdown(event)} · {formatEventCost(event)}
                  </small>
                </div>
              </div>
            ))}
          </div>
        ) : (
          <p className="empty">还没有从 Codex 会话里扫到 token_count 事件。</p>
        )}
      </Panel>
    </div>
  );
}

function tokenEventBreakdown(event: TokenUsageEvent) {
  return `新输入 ${formatTokens(event.inputTokens)} · 缓存 ${formatTokens(event.cachedInputTokens)} · 输出 ${formatTokens(event.outputTokens)}`;
}

function formatEventCost(event: TokenUsageEvent): string {
  if (!event.cost) {
    return "未定价";
  }
  return `估算 ${formatUsd(event.cost.totalUsd)}`;
}

function tokenEventKey(event: TokenUsageEvent): string {
  return [
    event.sessionId ?? "",
    event.timestamp ?? "",
    event.providerId ?? "",
    event.model,
    event.inputTokens,
    event.cachedInputTokens,
    event.outputTokens,
  ].join("|");
}

function formatUsd(raw?: string | null): string {
  const amount = Number(raw ?? "0");
  if (!Number.isFinite(amount)) {
    return "$—";
  }
  let digits = 2;
  if (amount > 0 && amount < 0.0001) {
    digits = 8;
  } else if (amount > 0 && amount < 0.01) {
    digits = 6;
  }
  return `$${amount.toFixed(digits)}`;
}

function formatBucketCost(bucket: TokenUsageBucket): string {
  if (bucket.pricedEvents === 0 && bucket.unpricedEvents > 0) {
    return "未定价";
  }
  if (bucket.unpricedEvents > 0) {
    return `已定价 ${formatUsd(bucket.cost.totalUsd)}`;
  }
  return formatUsd(bucket.cost.totalUsd);
}

function Metric({ label, value }: { label: string; value: string }) {
  return (
    <div className="metric">
      <div className="metric-icon">
        <BarChart3 aria-hidden="true" size={16} />
      </div>
      <span>{label}</span>
      <strong>{value}</strong>
    </div>
  );
}

function BucketList({ buckets, max }: { buckets: TokenUsageBucket[]; max: number }) {
  if (buckets.length === 0) {
    return <p className="empty">暂无统计数据。</p>;
  }
  return (
    <div className="usage-list" role="list">
      {buckets.slice(0, 12).map((bucket) => (
        <div className="usage-row" key={bucket.key} role="listitem">
          <div className="usage-row-head">
            <strong>{bucket.key}</strong>
            <span>{formatTokens(bucket.totalTokens)} · {formatBucketCost(bucket)}</span>
          </div>
          <div aria-hidden="true" className="usage-bar">
            <span style={{ width: `${Math.max(3, Math.round((bucket.totalTokens / max) * 100))}%` }} />
          </div>
          <small>
            新输入 {formatTokens(bucket.inputTokens)} · 缓存 {formatTokens(bucket.cachedInputTokens)} · 输出 {formatTokens(bucket.outputTokens)} · {bucket.events} 次
            {bucket.unpricedEvents ? ` · ${bucket.unpricedEvents} 次未定价` : ""}
          </small>
        </div>
      ))}
    </div>
  );
}

function maxTokens(buckets?: TokenUsageBucket[]) {
  return Math.max(1, ...(buckets ?? []).map((bucket) => bucket.totalTokens));
}
