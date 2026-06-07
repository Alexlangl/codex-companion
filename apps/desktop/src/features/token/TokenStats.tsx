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

  return (
    <div className="content-grid">
      <Panel eyebrow="用量" title="Token 统计">
        <Field label="Codex 目录">
          <input value={codexDir} onChange={(event) => setCodexDir(event.target.value)} />
        </Field>
        <div className="actions">
          <Button disabled={loading} onClick={() => void load()} variant="secondary">
            <RefreshCw size={15} /> {loading ? "扫描中" : "重新扫描"}
          </Button>
          {refreshing ? <span className="inline-muted">后台更新中</span> : null}
        </div>
        {error ? <div className="error-banner">{error}</div> : null}
        {!stats && loading ? <p className="empty">正在后台扫描 Codex 会话记录，页面其它操作不受影响。</p> : null}
        {!stats && !loading && !error ? <p className="empty">进入页面后会在后台扫描一次，也可以手动重新扫描。</p> : null}
        <div className="metric-grid details-top">
          <Metric label="今日 Token" value={formatTokens(todayBucket?.totalTokens ?? 0)} />
          <Metric label="总 Token" value={formatTokens(stats?.totalTokens ?? 0)} />
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
          <div className="table-list">
            {stats.recentEvents.slice().reverse().map((event, index) => (
              <div className="table-row" key={`${event.timestamp}-${event.sessionId}-${index}`}>
                <div>
                  <strong>{event.model}</strong>
                  <span>{event.providerId ?? "unknown"} · {formatTime(event.timestamp)}</span>
                  <small className="token-event-usage">
                    本次 {formatTokens(event.totalTokens)} · {tokenEventBreakdown(event)}
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

function Metric({ label, value }: { label: string; value: string }) {
  return (
    <div className="metric">
      <div className="metric-icon">
        <BarChart3 size={16} />
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
    <div className="usage-list">
      {buckets.slice(0, 12).map((bucket) => (
        <div className="usage-row" key={bucket.key}>
          <div className="usage-row-head">
            <strong>{bucket.key}</strong>
            <span>{formatTokens(bucket.totalTokens)}</span>
          </div>
          <div className="usage-bar">
            <span style={{ width: `${Math.max(3, Math.round((bucket.totalTokens / max) * 100))}%` }} />
          </div>
          <small>
            新输入 {formatTokens(bucket.inputTokens)} · 缓存 {formatTokens(bucket.cachedInputTokens)} · 输出 {formatTokens(bucket.outputTokens)} · {bucket.events} 次
          </small>
        </div>
      ))}
    </div>
  );
}

function maxTokens(buckets?: TokenUsageBucket[]) {
  return Math.max(1, ...(buckets ?? []).map((bucket) => bucket.totalTokens));
}
