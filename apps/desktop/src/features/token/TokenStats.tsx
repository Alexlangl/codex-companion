import { RefreshCw, RotateCcw } from "lucide-react";
import { useCallback, useEffect, useMemo, useRef, useState } from "react";
import type { ChangeEvent } from "react";
import { Button, Field } from "../../components/ui";
import { compactPath, formatTime, formatTokens } from "../../lib/format";
import { userFacingError } from "../../lib/errors";
import { providerAccountTitle } from "../../lib/provider-display";
import { getTokenUsageSyncStatus } from "../../lib/token-usage-api";
import { SettingsDialog } from "../../components/SettingsDialog";
import { PricingEditor } from "./PricingEditor";
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
  onLoad: (
    codexDir?: string,
    query?: TokenUsageQuery,
  ) => Promise<TokenUsageSummary>;
}) {
  const [filters, setFilters] = useState<UsageFilterState>(() =>
    loadUsageFilterState(status.codex.codexDir),
  );
  const [availableProviders, setAvailableProviders] = useState<string[]>([]);
  const [availableModels, setAvailableModels] = useState<string[]>([]);
  const [groupBy, setGroupBy] = useState<"model" | "provider" | "day">("model");
  const [stats, setStats] = useState<TokenUsageSummary | null>(null);
  const [loading, setLoading] = useState(false);
  const [refreshing, setRefreshing] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const [syncStatus, setSyncStatus] = useState<TokenUsageSyncStatus | null>(
    null,
  );
  const inFlightRef = useRef(false);
  const requestedQueryRef = useRef<string | null>(null);
  const {
    codexDir,
    rangePreset,
    customStartTime,
    customEndTime,
    providerId,
    model,
  } = filters;
  const hasStats = stats !== null;
  const dateRange = useMemo(
    () => dateRangeForPreset(rangePreset, customStartTime, customEndTime),
    [customEndTime, customStartTime, rangePreset],
  );
  const dateRangeError = validateDateRange(
    rangePreset,
    customStartTime,
    customEndTime,
  );
  const query = useMemo<TokenUsageQuery>(
    () => ({
      ...dateRange,
      providerId: providerId || undefined,
      model: model || undefined,
    }),
    [dateRange, model, providerId],
  );
  const queryKey = `${codexDir.trim()}|${query.startDate ?? ""}|${query.endDate ?? ""}|${providerId}|${model}`;
  const latestQueryKeyRef = useRef(queryKey);
  const pricingRevisionRef = useRef(0);
  latestQueryKeyRef.current = queryKey;
  const rangeLabel = usageRangeLabel(
    rangePreset,
    customStartTime,
    customEndTime,
  );
  const refreshIntervalSeconds =
    status.config.app.tokenUsageRefreshIntervalSeconds;

  useEffect(() => {
    saveUsageFilterState(filters);
  }, [filters]);

  const load = useCallback(
    async (mode: "manual" | "silent" | "rebuild" = "manual") => {
      if (inFlightRef.current) return;
      if (dateRangeError) {
        requestedQueryRef.current = queryKey;
        return;
      }
      inFlightRef.current = true;
      requestedQueryRef.current = queryKey;
      const requestQueryKey = queryKey;
      const pricingRevision = pricingRevisionRef.current;
      const showFullLoading = !hasStats;
      if (showFullLoading) {
        setLoading(true);
      } else {
        setRefreshing(true);
      }
      setError(null);
      try {
        const nextStats = await onLoad(codexDir, {
          ...query,
          rebuild: mode === "rebuild",
        });
        if (
          latestQueryKeyRef.current !== requestQueryKey ||
          pricingRevisionRef.current !== pricingRevision
        )
          return;
        setAvailableProviders((current) =>
          sameStringArray(current, nextStats.availableProviders)
            ? current
            : nextStats.availableProviders,
        );
        setAvailableModels((current) =>
          sameStringArray(current, nextStats.availableModels)
            ? current
            : nextStats.availableModels,
        );
        setStats((current) =>
          current &&
          tokenUsageSummaryKey(current) === tokenUsageSummaryKey(nextStats)
            ? current
            : nextStats,
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
    },
    [codexDir, dateRangeError, hasStats, onLoad, query, queryKey],
  );

  useEffect(() => {
    if (
      !active ||
      loading ||
      refreshing ||
      requestedQueryRef.current === queryKey
    )
      return;
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
            current &&
            tokenUsageSyncStatusKey(current) ===
              tokenUsageSyncStatusKey(nextStatus)
              ? current
              : nextStatus,
          );
        }
      } catch {
        if (!cancelled)
          setSyncStatus((current) => (current === null ? current : null));
      }
    };
    void poll();
    const timer = window.setInterval(() => void poll(), 750);
    return () => {
      cancelled = true;
      window.clearInterval(timer);
    };
  }, [active, loading, refreshing]);

  const unpricedModelText = stats?.unpricedModels.join("、") ?? "";
  const rangeCostLabel = stats?.unpricedEvents
    ? "范围内已定价成本"
    : "范围内估算成本";
  const providerOptions = includeSelectedOption(availableProviders, providerId);
  const modelOptions = includeSelectedOption(availableModels, model);
  const integrityIssueCount =
    (stats?.deferredFiles ?? 0) + (stats?.suspectedDuplicates ?? 0);

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

  function handleCustomStartDateChange(
    event: ChangeEvent<HTMLInputElement>,
  ): void {
    const nextStartTime = event.target.value;
    setFilters((current) => ({ ...current, customStartTime: nextStartTime }));
    setError(null);
  }

  function handleCustomEndDateChange(
    event: ChangeEvent<HTMLInputElement>,
  ): void {
    const nextEndTime = event.target.value;
    setFilters((current) => ({ ...current, customEndTime: nextEndTime }));
    setError(null);
  }

  function handleProviderChange(event: ChangeEvent<HTMLSelectElement>): void {
    const nextProviderId = event.target.value;
    setFilters((current) => ({
      ...current,
      providerId: nextProviderId,
      model: "",
    }));
    setError(null);
  }

  function handleModelChange(event: ChangeEvent<HTMLSelectElement>): void {
    const nextModel = event.target.value;
    setFilters((current) => ({ ...current, model: nextModel }));
    setError(null);
  }

  return (
    <div className="usage-page">
      <section className="usage-overview" aria-labelledby="usage-title">
        <div className="usage-heading">
          <div>
            <h2 id="usage-title">用量概览</h2>
            <p className="field-hint">{rangeLabel} · Token 与本地成本估算</p>
          </div>
          <div className="actions">
            <SettingsDialog
              title="模型定价"
              description="管理模型价格与账号成本倍率，保存后重新计算本地估算。"
            >
              <PricingEditor
                active={active}
                providers={status.config.providers}
                onSaved={async () => {
                  pricingRevisionRef.current += 1;
                  requestedQueryRef.current = "";
                  await load("silent");
                }}
              />
            </SettingsDialog>
            <Button
              disabled={loading || refreshing || Boolean(dateRangeError)}
              onClick={handleRefresh}
              variant="secondary"
            >
              <RefreshCw aria-hidden="true" size={15} />{" "}
              {loading ? "扫描中" : "刷新统计"}
            </Button>
          </div>
        </div>
        <div className="usage-filter-bar">
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
                  aria-describedby={
                    dateRangeError ? "usage-date-error" : undefined
                  }
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
                  aria-describedby={
                    dateRangeError ? "usage-date-error" : undefined
                  }
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
          {dateRangeError ? (
            <p className="field-error" id="usage-date-error">
              {dateRangeError}
            </p>
          ) : null}
          <div className="usage-dimension-filters">
            <Field label="Provider">
              <select
                disabled={loading}
                onChange={handleProviderChange}
                value={providerId}
              >
                <option value="">全部 Provider</option>
                {providerOptions.map((provider) => (
                  <option key={provider} value={provider}>
                    {usageProviderLabel(status, provider)}
                  </option>
                ))}
              </select>
            </Field>
            <Field label="模型">
              <select
                disabled={loading}
                onChange={handleModelChange}
                value={model}
              >
                <option value="">全部模型</option>
                {modelOptions.map((availableModel) => (
                  <option key={availableModel} value={availableModel}>
                    {availableModel}
                  </option>
                ))}
              </select>
            </Field>
          </div>
        </div>
        {syncStatus?.active ? (
          <div className="usage-sync-progress" aria-live="polite">
            <progress
              max={Math.max(1, syncStatus.totalFiles)}
              value={syncStatus.scannedFiles}
            />
            <span>
              {syncStatus.scannedFiles}/{syncStatus.totalFiles} 个文件
              {syncStatus.deferredFiles
                ? ` · ${syncStatus.deferredFiles} 个等待父会话`
                : ""}
              {syncStatus.suspectedDuplicates
                ? ` · ${syncStatus.suspectedDuplicates} 个疑似重复`
                : ""}
            </span>
          </div>
        ) : null}
        {error ? <div className="error-banner">{error}</div> : null}
        {!stats && loading ? (
          <p className="empty">
            正在后台扫描 Codex 会话记录，页面其它操作不受影响。
          </p>
        ) : null}
        {stats?.unpricedEvents ? (
          <div className="warning-box">
            <strong>{stats.unpricedEvents} 条 Token 事件尚未定价</strong>
            <p>
              当前成本只汇总已匹配价格的事件。未定价模型：{unpricedModelText}
              。可在本页「模型定价」直接添加价格。
            </p>
          </div>
        ) : null}
        {stats?.inferredPricedEvents ? (
          <p className="usage-pricing-note" role="status">
            其中 {stats.inferredPricedEvents}{" "}
            条成本按父任务模型推断，仅用于本地估算，不代表 OpenAI 或上游账单。
          </p>
        ) : null}
        {stats && integrityIssueCount > 0 ? (
          <div
            aria-live="polite"
            className="warning-box usage-integrity-warning"
            role="status"
          >
            <strong>有 {integrityIssueCount} 个文件未纳入统计</strong>
            {stats.deferredFiles > 0 ? (
              <p>
                {stats.deferredFiles}{" "}
                个子任务文件尚未找到可验证的父会话，暂不计入统计。
              </p>
            ) : null}
            {stats.suspectedDuplicates > 0 ? (
              <p>
                {stats.suspectedDuplicates} 个文件存在父记录冲突，暂不计入统计。
              </p>
            ) : null}
          </div>
        ) : null}
        <dl className="usage-metrics">
          <Metric
            label="总 Token"
            value={stats ? formatTokens(stats.totalTokens) : "—"}
          />
          <Metric
            label={rangeCostLabel}
            value={stats ? formatUsd(stats.cost.totalUsd) : "—"}
          />
          <Metric
            label="缓存命中率"
            value={stats ? cacheHitRate(stats) : "—"}
          />
        </dl>
        <div className="usage-token-breakdown">
          <span>
            新输入 <strong>{formatTokens(stats?.inputTokens ?? 0)}</strong>
          </span>
          <span>
            缓存读取{" "}
            <strong>{formatTokens(stats?.cachedInputTokens ?? 0)}</strong>
          </span>
          <span>
            缓存写入{" "}
            <strong>{formatTokens(stats?.cacheWriteInputTokens ?? 0)}</strong>
          </span>
          <span>
            输出 <strong>{formatTokens(stats?.outputTokens ?? 0)}</strong>
          </span>
        </div>
        <p className="field-hint">
          缓存命中率 = 缓存读取 ÷（新输入 + 缓存读取 + 缓存写入），不含输出。
        </p>
      </section>

      <section
        className="usage-breakdown"
        aria-labelledby="usage-breakdown-title"
      >
        <div className="usage-heading">
          <h2 id="usage-breakdown-title">用量明细</h2>
          <fieldset className="usage-range-fieldset usage-grouping">
            <legend className="sr-only">统计分组</legend>
            <div className="usage-range-options">
              {(
                [
                  ["model", "按模型"],
                  ["provider", "按 Provider"],
                  ["day", "按日期"],
                ] as const
              ).map(([value, label]) => (
                <label key={value}>
                  <input
                    type="radio"
                    name="usage-grouping"
                    value={value}
                    checked={groupBy === value}
                    onChange={() => setGroupBy(value)}
                  />
                  <span>{label}</span>
                </label>
              ))}
            </div>
          </fieldset>
        </div>
        {groupBy === "model" ? (
          <BucketTable buckets={stats?.byModel ?? []} label="模型" />
        ) : null}
        {groupBy === "provider" ? (
          <>
            <BucketTable
              buckets={stats?.byProvider ?? []}
              label="Provider"
              labelForKey={(key) => usageProviderLabel(status, key)}
            />
            <p className="field-hint">
              按会话记录中的来源统计。中转记录未保存具体上游时，标为“中转 ·
              上游未记录”，不会将历史用量归到当前账号。
            </p>
          </>
        ) : null}
        {groupBy === "day" ? (
          <BucketTable buckets={stats?.byDay ?? []} label="日期" />
        ) : null}
      </section>

      <details className="usage-recent-events">
        <summary>
          最近 Token 事件 <span>{stats?.recentEvents.length ?? 0} 条</span>
        </summary>
        {stats?.recentEvents.length ? (
          <div className="table-list" role="list">
            {stats.recentEvents
              .slice()
              .reverse()
              .map((event) => (
                <div
                  className="table-row"
                  key={tokenEventKey(event)}
                  role="listitem"
                >
                  <div>
                    <strong>{event.model}</strong>
                    <span>
                      {usageProviderLabel(
                        status,
                        event.providerId ?? "unknown",
                      )}{" "}
                      · {formatTime(event.timestamp)}
                    </span>
                    <small className="token-event-usage">
                      本次 {formatTokens(event.totalTokens)} ·{" "}
                      {tokenEventBreakdown(event)} · 缓存命中{" "}
                      {cacheHitRate(event)} · {formatEventCost(event)}
                    </small>
                  </div>
                </div>
              ))}
          </div>
        ) : (
          <p className="empty">还没有从 Codex 会话里扫到 token_count 事件。</p>
        )}
      </details>

      <details className="usage-scan-details">
        <summary>
          扫描与成本明细{" "}
          <span>
            {stats?.filesScanned ?? 0} 个文件 · {stats?.sessions ?? 0} 个会话 ·{" "}
            {stats?.events ?? 0} 次事件
          </span>
        </summary>
        <div className="usage-scan-settings">
          <Field label="Codex 目录">
            <input onChange={handleCodexDirChange} value={codexDir} />
          </Field>
          <Button
            disabled={loading || refreshing || Boolean(dateRangeError)}
            onClick={handleRebuild}
            variant="secondary"
          >
            <RotateCcw aria-hidden="true" size={15} /> 重建统计
          </Button>
        </div>
        <dl className="details-grid usage-scan-grid">
          <dt>目录</dt>
          <dd>{compactPath(stats?.codexDir ?? codexDir)}</dd>
          <dt>时间</dt>
          <dd>{rangeLabel}</dd>
          <dt>Provider</dt>
          <dd>
            {providerId ? usageProviderLabel(status, providerId) : "全部"}
          </dd>
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
          <dt>父模型推断</dt>
          <dd>{stats?.inferredPricedEvents ?? 0}</dd>
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
          <dd>
            {stats?.pricingOverridePath
              ? compactPath(stats.pricingOverridePath)
              : "未启用"}
          </dd>
        </dl>
      </details>
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
  if (event.pricingSource === "inferredParentModel" && event.pricingModel) {
    return `按父任务模型 ${event.pricingModel} 推断 ${formatUsd(event.cost.totalUsd)}`;
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
  return (
    left.length === right.length &&
    left.every((value, index) => value === right[index])
  );
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
    <div>
      <dt>{label}</dt>
      <dd>{value}</dd>
    </div>
  );
}

function cacheHitRate(usage: {
  inputTokens: number;
  cachedInputTokens: number;
  cacheWriteInputTokens: number;
}): string {
  const input =
    usage.inputTokens + usage.cachedInputTokens + usage.cacheWriteInputTokens;
  if (input <= 0) return "—";
  return `${((usage.cachedInputTokens / input) * 100).toFixed(1)}%`;
}

function BucketTable({
  buckets,
  label,
  labelForKey = (key) => key,
}: {
  buckets: TokenUsageBucket[];
  label: string;
  labelForKey?: (key: string) => string;
}) {
  if (buckets.length === 0)
    return <p className="empty">当前筛选范围内暂无统计数据。</p>;
  return (
    <div
      className="usage-table-scroll"
      role="region"
      aria-label={`按${label}统计`}
      tabIndex={0}
    >
      <table className="usage-table">
        <caption className="sr-only">
          按{label}统计的 Token、缓存命中率与估算成本
        </caption>
        <thead>
          <tr>
            <th scope="col">{label}</th>
            <th scope="col">总 Token</th>
            <th scope="col">缓存命中率</th>
            <th scope="col">新输入</th>
            <th scope="col">缓存读取</th>
            <th scope="col">缓存写入</th>
            <th scope="col">输出</th>
            <th scope="col">估算成本</th>
          </tr>
        </thead>
        <tbody>
          {buckets.map((bucket) => (
            <tr key={bucket.key}>
              <th scope="row">
                {labelForKey(bucket.key)}
                <small>
                  {bucket.events} 次事件
                  {bucket.inferredPricedEvents
                    ? ` · ${bucket.inferredPricedEvents} 次模型推断`
                    : ""}
                </small>
              </th>
              <td>{formatTokens(bucket.totalTokens)}</td>
              <td>{cacheHitRate(bucket)}</td>
              <td>{formatTokens(bucket.inputTokens)}</td>
              <td>{formatTokens(bucket.cachedInputTokens)}</td>
              <td>{formatTokens(bucket.cacheWriteInputTokens)}</td>
              <td>{formatTokens(bucket.outputTokens)}</td>
              <td>{formatBucketCost(bucket)}</td>
            </tr>
          ))}
        </tbody>
      </table>
    </div>
  );
}

function dateRangeForPreset(
  preset: UsageRangePreset,
  customStartTime: string,
  customEndTime: string,
): TokenUsageDateRange {
  if (preset === "all") return {};
  if (preset === "custom") {
    return {
      startDate: customStartTime || undefined,
      endDate: customEndTime || undefined,
    };
  }
  const daysByPreset: Record<
    Exclude<UsageRangePreset, "all" | "custom">,
    number
  > = {
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
  const option = USAGE_RANGE_OPTIONS.find(
    (candidate) => candidate.value === preset,
  );
  if (preset !== "custom") return option?.label ?? "全部";
  if (!customStartTime || !customEndTime) return "自定义";
  return `${formatLocalDateTime(customStartTime)} 至 ${formatLocalDateTime(customEndTime)}`;
}

function localDateTimeWithOffset(
  dayOffset: number,
  boundary: "start" | "end",
): string {
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
    const stored: unknown = JSON.parse(
      window.localStorage.getItem(USAGE_FILTER_STORAGE_KEY) ?? "null",
    );
    if (!isRecord(stored)) return fallback;
    return {
      codexDir: stringPreference(stored.codexDir, fallback.codexDir),
      rangePreset: isUsageRangePreset(stored.rangePreset)
        ? stored.rangePreset
        : fallback.rangePreset,
      customStartTime: dateTimePreference(
        stored.customStartTime,
        fallback.customStartTime,
      ),
      customEndTime: dateTimePreference(
        stored.customEndTime,
        fallback.customEndTime,
      ),
      providerId: stringPreference(stored.providerId, fallback.providerId),
      model: stringPreference(stored.model, fallback.model),
    };
  } catch {
    return fallback;
  }
}

function saveUsageFilterState(filters: UsageFilterState): void {
  try {
    window.localStorage.setItem(
      USAGE_FILTER_STORAGE_KEY,
      JSON.stringify(filters),
    );
  } catch {
    // A read-only or full WebView storage should not block usage queries.
  }
}

function isRecord(value: unknown): value is Record<string, unknown> {
  return typeof value === "object" && value !== null;
}

function isUsageRangePreset(value: unknown): value is UsageRangePreset {
  return (
    typeof value === "string" &&
    USAGE_RANGE_OPTIONS.some((option) => option.value === value)
  );
}

function stringPreference(value: unknown, fallback: string): string {
  return typeof value === "string" ? value : fallback;
}

function dateTimePreference(value: unknown, fallback: string): string {
  if (typeof value !== "string") return fallback;
  return /^\d{4}-\d{2}-\d{2}T\d{2}:\d{2}(?::\d{2})?$/.test(value)
    ? value
    : fallback;
}

function includeSelectedOption(options: string[], selected: string): string[] {
  if (!selected || options.includes(selected)) return options;
  return [selected, ...options];
}

function formatLocalDateTime(value: string): string {
  return value.replace("T", " ");
}

function usageProviderLabel(
  status: CompanionStatus,
  providerId: string,
): string {
  const provider = status.config.providers[providerId];
  if (providerId === "codex-companion") return "中转 · 上游未记录";
  if (providerId === "openai") return "OpenAI · 账号未记录";
  if (providerId === "unknown") return "来源未记录";
  if (!provider) return providerId;
  return providerAccountTitle(provider);
}
