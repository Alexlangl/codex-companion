import { BarChart3, RefreshCw, RotateCcw } from "lucide-react";
import { useCallback, useEffect, useMemo, useRef, useState } from "react";
import type { ChangeEvent } from "react";
import { Button, Field, Panel } from "../../components/ui";
import { compactPath, formatTime, formatTokens } from "../../lib/format";
import { userFacingError } from "../../lib/errors";
import { providerAccountTitle } from "../../lib/provider-display";
import { getTokenUsageSyncStatus } from "../../lib/token-usage-api";
import type {
  CompanionStatus,
  TokenUsageBucket,
  TokenUsageDateRange,
  TokenUsageEvent,
  TokenUsageQuery,
  TokenUsageSummary,
  TokenUsageSyncStatus,
} from "../../types/domain";

type UsageRangePreset = "today" | "7d" | "30d" | "all" | "custom";

type UsageFilterState = {
  codexDir: string;
  rangePreset: UsageRangePreset;
  customStartTime: string;
  customEndTime: string;
  providerId: string;
  model: string;
};

const USAGE_FILTER_STORAGE_KEY = "codex-companion:token-usage-filters:v1";

const USAGE_RANGE_OPTIONS = [
  { value: "today", label: "今天" },
  { value: "7d", label: "7 天" },
  { value: "30d", label: "30 天" },
  { value: "all", label: "全部" },
  { value: "custom", label: "自定义" },
] as const satisfies ReadonlyArray<{ value: UsageRangePreset; label: string }>;

export function TokenStats({
  active,
  status,
  onLoad,
}: {
  active: boolean;
  status: CompanionStatus;
  onLoad: (codexDir?: string, query?: TokenUsageQuery) => Promise<TokenUsageSummary>;
}) {
  const [filters, setFilters] = useState<UsageFilterState>(() => loadUsageFilterState(status.codex.codexDir));
  const [availableProviders, setAvailableProviders] = useState<string[]>([]);
  const [availableModels, setAvailableModels] = useState<string[]>([]);
  const [stats, setStats] = useState<TokenUsageSummary | null>(null);
  const [loading, setLoading] = useState(false);
  const [refreshing, setRefreshing] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const [syncStatus, setSyncStatus] = useState<TokenUsageSyncStatus | null>(null);
  const inFlightRef = useRef(false);
  const requestedQueryRef = useRef<string | null>(null);
  const { codexDir, rangePreset, customStartTime, customEndTime, providerId, model } = filters;
  const hasStats = stats !== null;
  const dateRange = useMemo(
    () => dateRangeForPreset(rangePreset, customStartTime, customEndTime),
    [customEndTime, customStartTime, rangePreset],
  );
  const dateRangeError = validateDateRange(rangePreset, customStartTime, customEndTime);
  const query = useMemo<TokenUsageQuery>(() => ({
    ...dateRange,
    providerId: providerId || undefined,
    model: model || undefined,
  }), [dateRange, model, providerId]);
  const queryKey = `${codexDir.trim()}|${query.startDate ?? ""}|${query.endDate ?? ""}|${providerId}|${model}`;
  const latestQueryKeyRef = useRef(queryKey);
  latestQueryKeyRef.current = queryKey;
  const rangeLabel = usageRangeLabel(rangePreset, customStartTime, customEndTime);
  const refreshIntervalSeconds = status.config.app.tokenUsageRefreshIntervalSeconds;

  useEffect(() => {
    saveUsageFilterState(filters);
  }, [filters]);

  const load = useCallback(async (mode: "manual" | "silent" | "rebuild" = "manual") => {
    if (inFlightRef.current) return;
    if (dateRangeError) {
      requestedQueryRef.current = queryKey;
      return;
    }
    inFlightRef.current = true;
    requestedQueryRef.current = queryKey;
    const requestQueryKey = queryKey;
    const showFullLoading = !hasStats;
    if (showFullLoading) {
      setLoading(true);
    } else {
      setRefreshing(true);
    }
    setError(null);
    try {
      const nextStats = await onLoad(codexDir, { ...query, rebuild: mode === "rebuild" });
      if (latestQueryKeyRef.current !== requestQueryKey) return;
      setAvailableProviders((current) =>
        sameStringArray(current, nextStats.availableProviders) ? current : nextStats.availableProviders,
      );
      setAvailableModels((current) =>
        sameStringArray(current, nextStats.availableModels) ? current : nextStats.availableModels,
      );
      setStats((current) =>
        current && tokenUsageSummaryKey(current) === tokenUsageSummaryKey(nextStats) ? current : nextStats,
      );
    } catch (unknownError) {
      if (latestQueryKeyRef.current === requestQueryKey) {
        setError(userFacingError(unknownError));
      }
    } finally {
      inFlightRef.current = false;
      setLoading(false);
      setRefreshing(false);
      setSyncStatus(null);
    }
  }, [codexDir, dateRangeError, hasStats, onLoad, query, queryKey]);

  useEffect(() => {
    if (!active || loading || refreshing || requestedQueryRef.current === queryKey) return;
    const timer = window.setTimeout(() => {
      void load("silent");
    }, 300);
    return () => window.clearTimeout(timer);
  }, [active, load, loading, queryKey, refreshing]);

  useEffect(() => {
    if (!active || refreshIntervalSeconds <= 0) return;
    const timer = window.setInterval(() => {
      void load("silent");
    }, refreshIntervalSeconds * 1000);
    return () => window.clearInterval(timer);
  }, [active, load, refreshIntervalSeconds]);

  useEffect(() => {
    if (!active || (!loading && !refreshing)) return;
    let cancelled = false;
    const poll = async (): Promise<void> => {
      try {
        const nextStatus = await getTokenUsageSyncStatus();
        if (!cancelled) {
          setSyncStatus((current) =>
            current && tokenUsageSyncStatusKey(current) === tokenUsageSyncStatusKey(nextStatus)
              ? current
              : nextStatus,
          );
        }
      } catch {
        if (!cancelled) setSyncStatus((current) => (current === null ? current : null));
      }
    };
    void poll();
    const timer = window.setInterval(() => void poll(), 750);
    return () => {
      cancelled = true;
      window.clearInterval(timer);
    };
  }, [active, loading, refreshing]);

  const maxModelTokens = useMemo(() => maxTokens(stats?.byModel), [stats]);
  const maxProviderTokens = useMemo(() => maxTokens(stats?.byProvider), [stats]);
  const maxDayTokens = useMemo(() => maxTokens(stats?.byDay), [stats]);
  const unpricedModelText = stats?.unpricedModels.join("、") ?? "";
  const rangeCostLabel = stats?.unpricedEvents ? "范围内已定价成本" : "范围内估算成本";
  const providerOptions = includeSelectedOption(availableProviders, providerId);
  const modelOptions = includeSelectedOption(availableModels, model);
  const integrityIssueCount = (stats?.deferredFiles ?? 0) + (stats?.suspectedDuplicates ?? 0);

  function handleRefresh(): void {
    void load();
  }

  function handleRebuild(): void {
    void load("rebuild");
  }

  function handleRangeChange(event: ChangeEvent<HTMLInputElement>): void {
    const nextRange = event.target.value as UsageRangePreset;
    setFilters((current) => ({ ...current, rangePreset: nextRange }));
    setError(null);
  }

  function handleCodexDirChange(event: ChangeEvent<HTMLInputElement>): void {
    const nextCodexDir = event.target.value;
    setFilters((current) => ({ ...current, codexDir: nextCodexDir }));
    setStats(null);
    setError(null);
  }

  function handleCustomStartDateChange(event: ChangeEvent<HTMLInputElement>): void {
    const nextStartTime = event.target.value;
    setFilters((current) => ({ ...current, customStartTime: nextStartTime }));
    setError(null);
  }

  function handleCustomEndDateChange(event: ChangeEvent<HTMLInputElement>): void {
    const nextEndTime = event.target.value;
    setFilters((current) => ({ ...current, customEndTime: nextEndTime }));
    setError(null);
  }

  function handleProviderChange(event: ChangeEvent<HTMLSelectElement>): void {
    const nextProviderId = event.target.value;
    setFilters((current) => ({ ...current, providerId: nextProviderId, model: "" }));
    setError(null);
  }

  function handleModelChange(event: ChangeEvent<HTMLSelectElement>): void {
    const nextModel = event.target.value;
    setFilters((current) => ({ ...current, model: nextModel }));
    setError(null);
  }

  return (
    <div className="content-grid">
      <Panel eyebrow="用量" title="Token 统计">
        <Field label="Codex 目录">
          <input onChange={handleCodexDirChange} value={codexDir} />
        </Field>
        <fieldset className="usage-range-fieldset">
          <legend>统计时间</legend>
          <div className="usage-range-options">
            {USAGE_RANGE_OPTIONS.map((option) => (
              <label key={option.value}>
                <input
                  checked={rangePreset === option.value}
                  disabled={loading}
                  name="usage-range"
                  onChange={handleRangeChange}
                  type="radio"
                  value={option.value}
                />
                <span>{option.label}</span>
              </label>
            ))}
          </div>
        </fieldset>
        {rangePreset === "custom" ? (
          <div className="usage-custom-dates">
            <Field label="开始时间">
              <input
                aria-describedby={dateRangeError ? "usage-date-error" : undefined}
                aria-invalid={Boolean(dateRangeError)}
                disabled={loading}
                max={customEndTime || undefined}
                onChange={handleCustomStartDateChange}
                step={1}
                type="datetime-local"
                value={customStartTime}
              />
            </Field>
            <Field label="结束时间">
              <input
                aria-describedby={dateRangeError ? "usage-date-error" : undefined}
                aria-invalid={Boolean(dateRangeError)}
                disabled={loading}
                min={customStartTime || undefined}
                onChange={handleCustomEndDateChange}
                step={1}
                type="datetime-local"
                value={customEndTime}
              />
            </Field>
          </div>
        ) : null}
        {dateRangeError ? <p className="field-error" id="usage-date-error">{dateRangeError}</p> : null}
        <div className="usage-dimension-filters">
          <Field label="Provider">
            <select disabled={loading} onChange={handleProviderChange} value={providerId}>
              <option value="">全部 Provider</option>
              {providerOptions.map((provider) => (
                <option key={provider} value={provider}>{usageProviderLabel(status, provider)}</option>
              ))}
            </select>
          </Field>
          <Field label="模型">
            <select disabled={loading} onChange={handleModelChange} value={model}>
              <option value="">全部模型</option>
              {modelOptions.map((availableModel) => (
                <option key={availableModel} value={availableModel}>{availableModel}</option>
              ))}
            </select>
          </Field>
        </div>
        <div className="actions">
          <Button disabled={loading || refreshing || Boolean(dateRangeError)} onClick={handleRefresh} variant="secondary">
            <RefreshCw aria-hidden="true" size={15} /> {loading ? "扫描中" : "重新扫描"}
          </Button>
          <Button disabled={loading || refreshing || Boolean(dateRangeError)} onClick={handleRebuild} variant="secondary">
            <RotateCcw aria-hidden="true" size={15} /> 重建统计
          </Button>
          {refreshing ? <span className="inline-muted">后台更新中</span> : null}
        </div>
        {syncStatus?.active ? (
          <div className="usage-sync-progress" aria-live="polite">
            <progress max={Math.max(1, syncStatus.totalFiles)} value={syncStatus.scannedFiles} />
            <span>
              {syncStatus.scannedFiles}/{syncStatus.totalFiles} 个文件
              {syncStatus.deferredFiles ? ` · ${syncStatus.deferredFiles} 个等待父会话` : ""}
              {syncStatus.suspectedDuplicates ? ` · ${syncStatus.suspectedDuplicates} 个疑似重复` : ""}
            </span>
          </div>
        ) : null}
        {error ? <div className="error-banner">{error}</div> : null}
        {!stats && loading ? <p className="empty">正在后台扫描 Codex 会话记录，页面其它操作不受影响。</p> : null}
        {stats?.unpricedEvents ? (
          <div className="warning-box">
            <strong>{stats.unpricedEvents} 条 Token 事件尚未定价</strong>
            <p>
              当前成本只汇总已匹配价格的事件。未定价模型：{unpricedModelText}。可在 Companion 数据目录创建 model-pricing.json 补充价格。
            </p>
          </div>
        ) : null}
        {stats && integrityIssueCount > 0 ? (
          <div aria-live="polite" className="warning-box usage-integrity-warning" role="status">
            <strong>有 {integrityIssueCount} 个文件未纳入统计</strong>
            {stats.deferredFiles > 0 ? <p>{stats.deferredFiles} 个子任务文件尚未找到可验证的父会话，暂不计入统计。</p> : null}
            {stats.suspectedDuplicates > 0 ? <p>{stats.suspectedDuplicates} 个文件存在父记录冲突，暂不计入统计。</p> : null}
          </div>
        ) : null}
        <div className="metric-grid details-top">
          <Metric label="范围内 Token" value={formatTokens(stats?.totalTokens ?? 0)} />
          <Metric label={rangeCostLabel} value={formatUsd(stats?.cost.totalUsd)} />
          <Metric label="新输入" value={formatTokens(stats?.inputTokens ?? 0)} />
          <Metric label="缓存输入" value={formatTokens(stats?.cachedInputTokens ?? 0)} />
          <Metric label="缓存写入" value={formatTokens(stats?.cacheWriteInputTokens ?? 0)} />
          <Metric label="输出" value={formatTokens(stats?.outputTokens ?? 0)} />
        </div>
      </Panel>

      <Panel eyebrow="范围" title="扫描范围">
        <dl className="details-grid">
          <dt>目录</dt>
          <dd>{compactPath(stats?.codexDir ?? codexDir)}</dd>
          <dt>时间</dt>
          <dd>{rangeLabel}</dd>
          <dt>Provider</dt>
          <dd>{providerId ? usageProviderLabel(status, providerId) : "全部"}</dd>
          <dt>模型</dt>
          <dd>{model || "全部"}</dd>
          <dt>文件</dt>
          <dd>{stats?.filesScanned ?? 0} 个 JSONL</dd>
          <dt>等待父会话</dt>
          <dd>{stats?.deferredFiles ?? 0}</dd>
          <dt>疑似重复</dt>
          <dd>{stats?.suspectedDuplicates ?? 0}</dd>
          <dt>缓存版本</dt>
          <dd>{stats?.cacheVersion ?? "—"}</dd>
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
          <dt>缓存写入成本</dt>
          <dd>{formatUsd(stats?.cost.cacheWriteInputUsd)}</dd>
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

function tokenEventBreakdown(event: TokenUsageEvent): string {
  return `新输入 ${formatTokens(event.inputTokens)} · 缓存读 ${formatTokens(event.cachedInputTokens)} · 缓存写 ${formatTokens(event.cacheWriteInputTokens)} · 输出 ${formatTokens(event.outputTokens)}`;
}

function formatEventCost(event: TokenUsageEvent): string {
  if (!event.cost) {
    return "未定价";
  }
  return `估算 ${formatUsd(event.cost.totalUsd)}`;
}

function tokenEventKey(event: TokenUsageEvent): string {
  if (event.eventId) return event.eventId;
  return [
    event.sessionId ?? "",
    event.timestamp ?? "",
    event.providerId ?? "",
    event.model,
    event.inputTokens,
    event.cachedInputTokens,
    event.cacheWriteInputTokens,
    event.outputTokens,
  ].join("|");
}

function sameStringArray(left: string[], right: string[]): boolean {
  return left.length === right.length && left.every((value, index) => value === right[index]);
}

function tokenUsageSummaryKey(summary: TokenUsageSummary): string {
  return JSON.stringify(summary);
}

function tokenUsageSyncStatusKey(status: TokenUsageSyncStatus): string {
  return [
    status.active,
    status.scannedFiles,
    status.totalFiles,
    status.deferredFiles,
    status.suspectedDuplicates,
    status.phase,
    status.startedAt ?? "",
    status.finishedAt ?? "",
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
            新输入 {formatTokens(bucket.inputTokens)} · 缓存读 {formatTokens(bucket.cachedInputTokens)} · 缓存写 {formatTokens(bucket.cacheWriteInputTokens)} · 输出 {formatTokens(bucket.outputTokens)} · {bucket.events} 次
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

function dateRangeForPreset(
  preset: UsageRangePreset,
  customStartTime: string,
  customEndTime: string,
): TokenUsageDateRange {
  if (preset === "all") return {};
  if (preset === "custom") {
    return { startDate: customStartTime || undefined, endDate: customEndTime || undefined };
  }
  const daysByPreset: Record<Exclude<UsageRangePreset, "all" | "custom">, number> = {
    today: 1,
    "7d": 7,
    "30d": 30,
  };
  const days = daysByPreset[preset];
  return {
    startDate: localDateTimeWithOffset(-(days - 1), "start"),
    endDate: localDateTimeWithOffset(0, "end"),
  };
}

function validateDateRange(
  preset: UsageRangePreset,
  customStartTime: string,
  customEndTime: string,
): string | null {
  if (preset !== "custom") return null;
  if (!customStartTime || !customEndTime) return "请选择开始时间和结束时间。";
  if (customStartTime > customEndTime) return "开始时间不能晚于结束时间。";
  return null;
}

function usageRangeLabel(
  preset: UsageRangePreset,
  customStartTime: string,
  customEndTime: string,
): string {
  const option = USAGE_RANGE_OPTIONS.find((candidate) => candidate.value === preset);
  if (preset !== "custom") return option?.label ?? "全部";
  if (!customStartTime || !customEndTime) return "自定义";
  return `${formatLocalDateTime(customStartTime)} 至 ${formatLocalDateTime(customEndTime)}`;
}

function localDateTimeWithOffset(dayOffset: number, boundary: "start" | "end"): string {
  const date = new Date();
  date.setDate(date.getDate() + dayOffset);
  if (boundary === "start") {
    date.setHours(0, 0, 0, 0);
  } else {
    date.setHours(23, 59, 59, 999);
  }
  const year = date.getFullYear();
  const month = String(date.getMonth() + 1).padStart(2, "0");
  const day = String(date.getDate()).padStart(2, "0");
  const hour = String(date.getHours()).padStart(2, "0");
  const minute = String(date.getMinutes()).padStart(2, "0");
  const second = String(date.getSeconds()).padStart(2, "0");
  return `${year}-${month}-${day}T${hour}:${minute}:${second}`;
}

function loadUsageFilterState(defaultCodexDir: string): UsageFilterState {
  const fallback: UsageFilterState = {
    codexDir: defaultCodexDir,
    rangePreset: "all",
    customStartTime: localDateTimeWithOffset(-6, "start"),
    customEndTime: localDateTimeWithOffset(0, "end"),
    providerId: "",
    model: "",
  };
  try {
    const stored: unknown = JSON.parse(window.localStorage.getItem(USAGE_FILTER_STORAGE_KEY) ?? "null");
    if (!isRecord(stored)) return fallback;
    return {
      codexDir: stringPreference(stored.codexDir, fallback.codexDir),
      rangePreset: isUsageRangePreset(stored.rangePreset) ? stored.rangePreset : fallback.rangePreset,
      customStartTime: dateTimePreference(stored.customStartTime, fallback.customStartTime),
      customEndTime: dateTimePreference(stored.customEndTime, fallback.customEndTime),
      providerId: stringPreference(stored.providerId, fallback.providerId),
      model: stringPreference(stored.model, fallback.model),
    };
  } catch {
    return fallback;
  }
}

function saveUsageFilterState(filters: UsageFilterState): void {
  try {
    window.localStorage.setItem(USAGE_FILTER_STORAGE_KEY, JSON.stringify(filters));
  } catch {
    // A read-only or full WebView storage should not block usage queries.
  }
}

function isRecord(value: unknown): value is Record<string, unknown> {
  return typeof value === "object" && value !== null;
}

function isUsageRangePreset(value: unknown): value is UsageRangePreset {
  return typeof value === "string" && USAGE_RANGE_OPTIONS.some((option) => option.value === value);
}

function stringPreference(value: unknown, fallback: string): string {
  return typeof value === "string" ? value : fallback;
}

function dateTimePreference(value: unknown, fallback: string): string {
  if (typeof value !== "string") return fallback;
  return /^\d{4}-\d{2}-\d{2}T\d{2}:\d{2}(?::\d{2})?$/.test(value) ? value : fallback;
}

function includeSelectedOption(options: string[], selected: string): string[] {
  if (!selected || options.includes(selected)) return options;
  return [selected, ...options];
}

function formatLocalDateTime(value: string): string {
  return value.replace("T", " ");
}

function usageProviderLabel(status: CompanionStatus, providerId: string): string {
  const provider = status.config.providers[providerId];
  if (!provider) return providerId;
  return providerAccountTitle(provider);
}
