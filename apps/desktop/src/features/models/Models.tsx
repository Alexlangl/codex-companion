import {
  AlertTriangle,
  Check,
  Database,
  Minus,
  RefreshCw,
  Search,
  Server,
  ShieldCheck,
} from "lucide-react";
import { useCallback, useDeferredValue, useEffect, useMemo, useRef, useState } from "react";
import { Badge, IconButton } from "../../components/ui";
import { getModelMatrix } from "../../lib/api";
import { userFacingError } from "../../lib/errors";
import { formatTime } from "../../lib/format";
import type {
  ModelMatrixModel,
  ModelMatrixSnapshot,
  ModelMatrixSource,
  ModelSourceKind,
} from "../../types/domain";

type ModelsProps = {
  active: boolean;
};

type MatrixFilter = "all" | "trusted" | "differences" | "relay-only" | "ultra";

const filterLabels: Record<MatrixFilter, string> = {
  all: "全部",
  trusted: "官方 / 缓存",
  differences: "有差异",
  "relay-only": "上游独有",
  ultra: "Ultra",
};

export function Models({ active }: ModelsProps) {
  const [snapshot, setSnapshot] = useState<ModelMatrixSnapshot | null>(null);
  const [loading, setLoading] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const [query, setQuery] = useState("");
  const [filter, setFilter] = useState<MatrixFilter>("all");
  const requestRef = useRef<Promise<void> | null>(null);
  const deferredQuery = useDeferredValue(query.trim().toLocaleLowerCase());

  const load = useCallback(async () => {
    if (requestRef.current) return requestRef.current;
    const request = (async () => {
      setLoading(true);
      setError(null);
      try {
        setSnapshot(await getModelMatrix());
      } catch (unknownError) {
        setError(userFacingError(unknownError));
      } finally {
        setLoading(false);
      }
    })();
    requestRef.current = request;
    try {
      await request;
    } finally {
      if (requestRef.current === request) requestRef.current = null;
    }
  }, []);

  useEffect(() => {
    if (active && snapshot === null) void load();
  }, [active, load, snapshot]);

  const sourceMap = useMemo(
    () => new Map(snapshot?.sources.map((source) => [source.id, source]) ?? []),
    [snapshot],
  );
  const availableSources = useMemo(
    () => snapshot?.sources.filter((source) => source.status === "available") ?? [],
    [snapshot],
  );
  const modelFlags = useMemo(() => {
    const flags = new Map<string, ReturnType<typeof classifyModel>>();
    for (const model of snapshot?.models ?? []) {
      flags.set(model.id, classifyModel(model, sourceMap, availableSources.length));
    }
    return flags;
  }, [availableSources.length, snapshot, sourceMap]);
  const filterCounts = useMemo(() => {
    const counts: Record<MatrixFilter, number> = {
      all: snapshot?.models.length ?? 0,
      trusted: 0,
      differences: 0,
      "relay-only": 0,
      ultra: 0,
    };
    for (const flags of modelFlags.values()) {
      if (flags.trusted) counts.trusted += 1;
      if (flags.different) counts.differences += 1;
      if (flags.relayOnly) counts["relay-only"] += 1;
      if (flags.ultra) counts.ultra += 1;
    }
    return counts;
  }, [modelFlags, snapshot]);
  const visibleModels = useMemo(() => {
    const models = snapshot?.models ?? [];
    return models.filter((model) => {
      const flags = modelFlags.get(model.id);
      if (!flags || !matchesFilter(flags, filter)) return false;
      if (!deferredQuery) return true;
      return [model.id, model.displayName, ...model.reasoningEfforts]
        .join(" ")
        .toLocaleLowerCase()
        .includes(deferredQuery);
    });
  }, [deferredQuery, filter, modelFlags, snapshot]);

  if (snapshot === null) {
    return (
      <section className="model-matrix-loading" aria-live="polite">
        {error ? (
          <>
            <AlertTriangle aria-hidden="true" size={18} />
            <strong>{error}</strong>
            <IconButton label="重试模型查询" onClick={() => void load()}>
              <RefreshCw size={16} />
            </IconButton>
          </>
        ) : (
          <>
            <RefreshCw aria-hidden="true" className="spin-icon" size={18} />
            <strong>正在读取模型来源</strong>
          </>
        )}
      </section>
    );
  }

  const availableCount = availableSources.length;
  const failedCount = snapshot.sources.filter((source) => source.status === "failed").length;

  return (
    <div className="model-matrix-stack">
      <section className="model-matrix-overview" aria-labelledby="model-matrix-title">
        <div className="model-matrix-heading">
          <div>
            <span className="panel-eyebrow">MODEL DISCOVERY</span>
            <h2 id="model-matrix-title">模型矩阵</h2>
          </div>
          <IconButton label="刷新模型矩阵" disabled={loading} onClick={() => void load()}>
            <RefreshCw aria-hidden="true" className={loading ? "spin-icon" : undefined} size={16} />
          </IconButton>
        </div>
        <dl className="model-matrix-summary">
          <SummaryItem label="模型" value={snapshot.models.length} />
          <SummaryItem label="可用来源" value={`${availableCount}/${snapshot.sources.length}`} />
          <SummaryItem label="Ultra" value={filterCounts.ultra} />
          <SummaryItem label="上游独有" value={filterCounts["relay-only"]} />
        </dl>
        {error ? <div className="model-matrix-error">{error}</div> : null}
        {failedCount > 0 ? (
          <div className="model-matrix-notice">
            <AlertTriangle aria-hidden="true" size={14} />
            <span>{failedCount} 个来源查询失败，失败列不参与差异判定</span>
          </div>
        ) : null}
      </section>

      <section className="model-source-section" aria-labelledby="model-source-title">
        <div className="model-section-heading">
          <h3 id="model-source-title">来源</h3>
          <span>更新于 {formatTime(snapshot.generatedAt)}</span>
        </div>
        <div className="model-source-list">
          {snapshot.sources.map((source) => (
            <SourceItem key={source.id} source={source} />
          ))}
        </div>
      </section>

      <section className="model-table-section" aria-labelledby="model-table-title">
        <div className="model-section-heading model-table-heading">
          <h3 id="model-table-title">比对</h3>
          <span>{visibleModels.length} 个结果</span>
        </div>
        <div className="model-matrix-toolbar">
          <label className="model-search-field">
            <span className="sr-only">搜索模型</span>
            <Search aria-hidden="true" size={15} />
            <input
              type="search"
              value={query}
              onChange={(event) => setQuery(event.currentTarget.value)}
              placeholder="搜索模型"
            />
          </label>
          <div className="model-filter-list" aria-label="模型过滤">
            {(Object.keys(filterLabels) as MatrixFilter[]).map((value) => (
              <button
                key={value}
                type="button"
                className="model-filter-button"
                aria-pressed={filter === value}
                onClick={() => setFilter(value)}
              >
                <span>{filterLabels[value]}</span>
                <small>{filterCounts[value]}</small>
              </button>
            ))}
          </div>
        </div>

        <div className="model-matrix-table-shell">
          <table className="model-matrix-table">
            <thead>
              <tr>
                <th className="model-name-column" scope="col">模型</th>
                <th className="model-capability-column" scope="col">能力</th>
                {snapshot.sources.map((source) => (
                  <th key={source.id} className="model-source-column" scope="col">
                    <SourceHeader source={source} />
                  </th>
                ))}
              </tr>
            </thead>
            <tbody>
              {visibleModels.map((model) => {
                const modelSources = new Set(model.sourceIds);
                const flags = modelFlags.get(model.id);
                return (
                  <tr key={model.id}>
                    <th className="model-name-column" scope="row">
                      <div className="model-name-cell">
                        <strong>{model.displayName}</strong>
                        {model.displayName !== model.id ? <code>{model.id}</code> : null}
                        <div className="model-name-badges">
                          {model.ultraCapable ? <Badge tone="accent">Ultra</Badge> : null}
                          {flags?.relayOnly ? <Badge tone="info">上游独有</Badge> : null}
                          {model.visibility === "hide" ? <Badge>隐藏</Badge> : null}
                        </div>
                      </div>
                    </th>
                    <td className="model-capability-column">
                      <div className="model-capability-cell">
                        <span>{model.reasoningEfforts.length ? model.reasoningEfforts.join(" · ") : "未声明"}</span>
                        {model.multiAgentVersion ? <small>Multi-agent {model.multiAgentVersion}</small> : null}
                      </div>
                    </td>
                    {snapshot.sources.map((source) => (
                      <td key={source.id} className="model-availability-cell">
                        <Availability source={source} present={modelSources.has(source.id)} />
                      </td>
                    ))}
                  </tr>
                );
              })}
            </tbody>
          </table>
          {visibleModels.length === 0 ? <div className="model-matrix-empty">没有匹配的模型</div> : null}
        </div>
      </section>
    </div>
  );
}

function SummaryItem({ label, value }: { label: string; value: string | number }) {
  return (
    <div>
      <dt>{label}</dt>
      <dd>{value}</dd>
    </div>
  );
}

function SourceItem({ source }: { source: ModelMatrixSource }) {
  const status = sourceStatus(source);
  return (
    <article className={`model-source-item model-source-${source.status}`} title={source.error ?? undefined}>
      <div className="model-source-icon">{sourceIcon(source.kind)}</div>
      <div className="model-source-copy">
        <strong>{source.name}</strong>
        <span>{sourceKindLabel(source.kind)}</span>
        {source.error ? <small>{source.error}</small> : source.fetchedAt ? <small>{formatTime(source.fetchedAt)}</small> : null}
      </div>
      <div className="model-source-state">
        {source.activeGroup ? <Badge tone="info">当前分组</Badge> : null}
        <Badge tone={status.tone}>{status.label}</Badge>
      </div>
    </article>
  );
}

function SourceHeader({ source }: { source: ModelMatrixSource }) {
  return (
    <div className="model-source-header" title={source.error ?? source.name}>
      {sourceIcon(source.kind)}
      <strong>{source.name}</strong>
      <small>{sourceKindLabel(source.kind)}</small>
    </div>
  );
}

function Availability({ source, present }: { source: ModelMatrixSource; present: boolean }) {
  if (source.status === "failed") {
    return (
      <span className="model-availability model-availability-error" aria-label={`${source.name} 查询失败`} title={source.error ?? "查询失败"}>
        <AlertTriangle aria-hidden="true" size={15} />
      </span>
    );
  }
  if (source.status === "skipped") {
    return (
      <span className="model-availability model-availability-skipped" aria-label={`${source.name} 未查询`} title="未查询">
        <Minus aria-hidden="true" size={15} />
      </span>
    );
  }
  return present ? (
    <span className="model-availability model-availability-present" aria-label={`${source.name} 支持`} title="接口已返回">
      <Check aria-hidden="true" size={16} />
    </span>
  ) : (
    <span className="model-availability model-availability-absent" aria-label={`${source.name} 未返回`} title="接口未返回">
      <Minus aria-hidden="true" size={15} />
    </span>
  );
}

function classifyModel(
  model: ModelMatrixModel,
  sourceMap: Map<string, ModelMatrixSource>,
  availableSourceCount: number,
) {
  let trusted = false;
  let relay = false;
  let availablePresence = 0;
  for (const sourceId of model.sourceIds) {
    const source = sourceMap.get(sourceId);
    if (!source || source.status !== "available") continue;
    availablePresence += 1;
    if (source.kind === "relay") relay = true;
    else trusted = true;
  }
  return {
    trusted,
    relayOnly: relay && !trusted,
    different: availableSourceCount > 1 && availablePresence < availableSourceCount,
    ultra: model.ultraCapable,
  };
}

function matchesFilter(flags: ReturnType<typeof classifyModel>, filter: MatrixFilter) {
  switch (filter) {
    case "all":
      return true;
    case "trusted":
      return flags.trusted;
    case "differences":
      return flags.different;
    case "relay-only":
      return flags.relayOnly;
    case "ultra":
      return flags.ultra;
  }
}

function sourceStatus(source: ModelMatrixSource): { label: string; tone: "neutral" | "ok" | "danger" } {
  if (source.status === "available") return { label: `${source.modelCount} 个`, tone: "ok" };
  if (source.status === "failed") return { label: "失败", tone: "danger" };
  return { label: source.kind === "local_cache" ? "未生成" : "已停用", tone: "neutral" };
}

function sourceKindLabel(kind: ModelSourceKind) {
  switch (kind) {
    case "local_cache":
      return "models_cache.json";
    case "official_oauth":
      return "官方 OAuth";
    case "relay":
      return "/v1/models";
  }
}

function sourceIcon(kind: ModelSourceKind) {
  switch (kind) {
    case "local_cache":
      return <Database aria-hidden="true" size={15} />;
    case "official_oauth":
      return <ShieldCheck aria-hidden="true" size={15} />;
    case "relay":
      return <Server aria-hidden="true" size={15} />;
  }
}
