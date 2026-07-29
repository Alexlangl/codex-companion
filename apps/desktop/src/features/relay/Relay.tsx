import * as Dialog from "@radix-ui/react-dialog";
import {
  ArrowRight,
  CheckCircle2,
  Copy,
  Database,
  KeyRound,
  Plus,
  RadioTower,
  RefreshCw,
  RotateCw,
  Settings2,
  ShieldCheck,
  Trash2,
  X,
  XCircle,
} from "lucide-react";
import { useCallback, useEffect, useMemo, useRef, useState } from "react";
import { Badge, Button, Field, IconButton, Panel } from "../../components/ui";
import type { BadgeTone } from "../../components/ui";
import {
  apiServiceSelfTest,
  clearApiRequestLogs,
  createApiClient,
  deleteApiClient,
  getApiRequestLogs,
  getApiServiceSnapshot,
  getRelayEvents,
  rotateApiClientKey,
  updateApiClient,
  updateRelaySettings,
} from "../../lib/api";
import { formatTime } from "../../lib/format";
import { userFacingError } from "../../lib/errors";
import { apiRequestLogsEqual, relayEventsEqual } from "../../lib/log-snapshot";
import { providerAccountTitle, shortId } from "../../lib/provider-display";
import {
  groupRelayDiagnosticEvents,
  relayEventMessageText,
  type RelayDiagnosticGroup,
  type RelayRequestEventGroup,
} from "../../lib/relay-diagnostics";
import type {
  ApiClient,
  ApiClientSecret,
  ApiRequestAttemptLog,
  ApiRequestLog,
  ApiServiceSelfTest,
  ApiServiceSnapshot,
  CompanionStatus,
  RelayEvent,
  RelaySettingsUpdate,
} from "../../types/domain";

type RelayProps = {
  active: boolean;
  status: CompanionStatus;
};

type ClientEditor = {
  id: string;
  name: string;
  models: string;
  enabled: boolean;
};

const LOG_REFRESH_INTERVAL_MS = 2_000;

export function Relay({ active, status }: RelayProps) {
  const [snapshot, setSnapshot] = useState<ApiServiceSnapshot | null>(null);
  const [loading, setLoading] = useState(false);
  const [action, setAction] = useState<string | null>(null);
  const [error, setError] = useState<string | null>(null);
  const [clientName, setClientName] = useState("");
  const [clientModels, setClientModels] = useState("");
  const [revealedSecret, setRevealedSecret] = useState<ApiClientSecret | null>(null);
  const [editor, setEditor] = useState<ClientEditor | null>(null);
  const [selfTest, setSelfTest] = useState<ApiServiceSelfTest | null>(null);
  const [settings, setSettings] = useState<RelaySettingsUpdate>(() => relaySettingsFromStatus(status));
  const [relayEvents, setRelayEvents] = useState<RelayEvent[]>(status.recentEvents);
  const [logsRefreshing, setLogsRefreshing] = useState(false);
  const logRefreshInFlightRef = useRef(false);

  const loadSnapshot = useCallback(async () => {
    setLoading(true);
    try {
      const [nextSnapshot, nextSelfTest] = await Promise.all([
        getApiServiceSnapshot(),
        apiServiceSelfTest(),
      ]);
      setSnapshot(nextSnapshot);
      setSelfTest(nextSelfTest);
      setError(null);
    } catch (unknownError) {
      setError(userFacingError(unknownError));
    } finally {
      setLoading(false);
    }
  }, []);

  const loadLogs = useCallback(async (showLoading: boolean): Promise<void> => {
    if (logRefreshInFlightRef.current) return;
    logRefreshInFlightRef.current = true;
    if (showLoading) setLogsRefreshing(true);
    try {
      const [requests, events] = await Promise.all([getApiRequestLogs(), getRelayEvents()]);
      setSnapshot((current) => {
        if (!current || apiRequestLogsEqual(current.recentRequests, requests)) return current;
        return { ...current, recentRequests: requests };
      });
      setRelayEvents((current) => relayEventsEqual(current, events) ? current : events);
      if (showLoading) setError(null);
    } catch (unknownError) {
      if (showLoading) setError(userFacingError(unknownError));
    } finally {
      logRefreshInFlightRef.current = false;
      if (showLoading) setLogsRefreshing(false);
    }
  }, []);

  useEffect(() => {
    if (!active) return;
    void loadSnapshot();
    void loadLogs(false);
    const timer = window.setInterval(() => {
      void loadLogs(false);
    }, LOG_REFRESH_INTERVAL_MS);
    return () => window.clearInterval(timer);
  }, [active, loadLogs, loadSnapshot]);

  useEffect(() => {
    setSettings(relaySettingsFromStatus(status));
  }, [status]);

  async function runAction(label: string, task: () => Promise<void>) {
    setAction(label);
    setError(null);
    try {
      await task();
      await loadSnapshot();
    } catch (unknownError) {
      setError(userFacingError(unknownError));
    } finally {
      setAction(null);
    }
  }

  function handleCreateClient() {
    const name = clientName.trim();
    if (!name) {
      setError("请输入 API client 名称");
      return;
    }
    void runAction("create-client", async () => {
      const secret = await createApiClient({ name, allowedModels: parseModels(clientModels) });
      setRevealedSecret(secret);
      setClientName("");
      setClientModels("");
    });
  }

  function handleRotateClient(client: ApiClient) {
    if (!window.confirm(`轮换“${client.name}”的密钥？旧密钥会立即失效。`)) return;
    void runAction(`rotate-${client.id}`, async () => {
      setRevealedSecret(await rotateApiClientKey(client.id));
    });
  }

  function handleDeleteClient(client: ApiClient) {
    if (!window.confirm(`删除 API client“${client.name}”？此操作不可撤销。`)) return;
    void runAction(`delete-${client.id}`, async () => {
      await deleteApiClient(client.id);
      if (revealedSecret?.client.id === client.id) setRevealedSecret(null);
    });
  }

  function handleSaveClient() {
    if (!editor) return;
    void runAction(`update-${editor.id}`, async () => {
      await updateApiClient({
        id: editor.id,
        name: editor.name,
        allowedModels: parseModels(editor.models),
        enabled: editor.enabled,
      });
      setEditor(null);
    });
  }

  function handleSaveSettings() {
    void runAction("save-settings", async () => {
      await updateRelaySettings(settings);
    });
  }

  function handleSelfTest() {
    void runAction("self-test", () => Promise.resolve());
  }

  function handleClearLogs() {
    if (!window.confirm("清空本地 API 请求日志？client 和配置不会被删除。")) return;
    void runAction("clear-logs", async () => {
      await clearApiRequestLogs();
    });
  }

  function handleRefreshLogs(): void {
    void loadLogs(true);
  }

  const clients = snapshot?.clients ?? [];
  const requests = snapshot?.recentRequests ?? [];
  const cooldowns = snapshot?.modelCooldowns ?? [];
  const diagnosticGroups = useMemo(() => groupRelayDiagnosticEvents(relayEvents), [relayEvents]);
  const requestEvents = useMemo(() => requestEventMap(diagnosticGroups), [diagnosticGroups]);
  const requestsById = useMemo(
    () => new Map(requests.map((request) => [request.requestId, request])),
    [requests],
  );
  const secretForExample = revealedSecret?.apiKey ?? "YOUR_CODEX_COMPANION_API_KEY";
  const curlExample = [
    `curl ${status.relayBaseUrl}/responses \\`,
    `  -H "Authorization: Bearer ${secretForExample}" \\`,
    '  -H "Content-Type: application/json" \\',
    `  -d '{"model":"gpt-5.6-codex","input":"hello"}'`,
  ].join("\n");

  return (
    <div className="api-service-stack">
      <section className="api-service-overview" aria-labelledby="api-service-title">
        <div className="api-service-overview-main">
          <div className="api-service-icon" aria-hidden="true">
            <RadioTower size={18} />
          </div>
          <div>
            <span className="panel-eyebrow">LOCAL OPENAI-COMPATIBLE API</span>
            <h2 id="api-service-title">把当前账号分组作为本地 API</h2>
            <p>应用只连接一个地址，Companion 负责 OAuth、协议转换、会话亲和、故障切换与审计。</p>
          </div>
        </div>
        <div className="api-service-endpoint">
          <span>Base URL</span>
          <code>{status.relayBaseUrl}</code>
          <IconButton label="复制 API Base URL" onClick={() => void navigator.clipboard.writeText(status.relayBaseUrl)}>
            <Copy size={15} />
          </IconButton>
        </div>
        <div className="api-service-health-row">
          <StatusItem
            label="HTTP 监听"
            state={selfTest === null ? "pending" : selfTest.listenerOk ? "ok" : "error"}
            value={`${status.config.relay.host}:${status.config.relay.port}`}
          />
          <StatusItem
            label="当前分组"
            state={status.activeGroup ? "ok" : "error"}
            value={status.activeGroup?.name ?? status.config.relay.activeGroupId}
          />
          <StatusItem
            label="可用账号"
            state={status.activeProviders.length > 0 ? "ok" : "error"}
            value={`${status.activeProviders.length}`}
          />
          <StatusItem
            label="访问策略"
            state="ok"
            value={settings.requireApiKey ? "强制密钥" : "本机兼容"}
          />
          <StatusItem
            label="账号池"
            state={poolHealthState(snapshot)}
            value={snapshot ? `${snapshot.poolHealth.healthy}/${snapshot.poolHealth.enabled} 健康` : "读取中"}
          />
          <StatusItem
            label="会话亲和"
            state="ok"
            value={snapshot ? `${snapshot.affinityBindings} 个绑定` : "读取中"}
          />
        </div>
        <div className="api-service-toolbar">
          <Button disabled={action !== null} onClick={handleSelfTest} variant="secondary">
            <ShieldCheck size={15} /> {action === "self-test" ? "正在自检" : "运行本地自检"}
          </Button>
          <Button disabled={loading} onClick={() => void loadSnapshot()} variant="ghost">
            <RefreshCw className={loading ? "spin-icon" : undefined} size={15} /> 刷新数据
          </Button>
          {selfTest ? (
            <span className={`api-self-test-result ${selfTest.ok ? "api-self-test-ok" : "api-self-test-failed"}`}>
              {selfTest.ok ? <CheckCircle2 size={14} /> : <XCircle size={14} />}
              {selfTest.message} · {selfTest.latencyMs} ms
            </span>
          ) : null}
        </div>
      </section>

      {error ? <div className="error-banner api-service-error">{error}</div> : null}

      {revealedSecret ? (
        <section className="api-secret-reveal" aria-live="polite">
          <div>
            <KeyRound size={17} />
            <div>
              <strong>{revealedSecret.client.name} 的新密钥</strong>
              <span>只显示这一次。数据库仅保存 SHA-256 哈希，请现在复制到调用方。</span>
            </div>
          </div>
          <code>{revealedSecret.apiKey}</code>
          <div className="actions">
            <Button onClick={() => void navigator.clipboard.writeText(revealedSecret.apiKey)}>
              <Copy size={14} /> 复制密钥
            </Button>
            <Button onClick={() => setRevealedSecret(null)} variant="ghost">我已保存</Button>
          </div>
        </section>
      ) : null}

      <div className="api-service-grid">
        <Panel eyebrow="访问控制" title="API clients">
          <p className="relay-help">为每个调用方创建独立密钥和模型权限。停用、轮换或删除不会影响其他 client。</p>
          <div className="api-client-create">
            <Field label="Client 名称">
              <input
                aria-label="API client 名称"
                onChange={(event) => setClientName(event.target.value)}
                placeholder="例如：本地 CLI / 自动化脚本"
                value={clientName}
              />
            </Field>
            <Field label="允许模型（可选）">
              <input
                aria-describedby="api-model-help"
                onChange={(event) => setClientModels(event.target.value)}
                placeholder="gpt-5.6, gpt-5.6-codex"
                value={clientModels}
              />
            </Field>
            <p className="field-hint" id="api-model-help">逗号或换行分隔；留空表示允许全部模型。</p>
            <Button disabled={action !== null} onClick={handleCreateClient}>
              <Plus size={15} /> 创建并显示密钥
            </Button>
          </div>
          <div className="api-client-list">
            {clients.length === 0 ? (
              <div className="api-compact-empty">尚未创建 client。先创建一个，再开启强制密钥。</div>
            ) : (
              clients.map((client) => (
                <div className="api-client-row" key={client.id}>
                  <div className="api-client-main">
                    <div className="api-client-title">
                      <strong>{client.name}</strong>
                      <Badge tone={client.enabled ? "ok" : "neutral"}>{client.enabled ? "启用" : "停用"}</Badge>
                    </div>
                    <code>{client.keyPrefix}••••••••</code>
                    <span>
                      {client.allowedModels.length === 0 ? "全部模型" : client.allowedModels.join(" · ")}
                      {` · ${client.requestCount} 次请求`}
                      {client.lastUsedAt ? ` · 最近 ${formatTime(client.lastUsedAt)}` : " · 尚未使用"}
                    </span>
                    <span className="api-client-usage-line">
                      <Badge tone={client.health.status === "degraded" ? "danger" : client.health.status === "healthy" ? "ok" : "neutral"}>
                        {client.health.status === "degraded" ? "连接降级" : client.health.status === "healthy" ? "连接正常" : client.health.status === "idle" ? "待使用" : "已停用"}
                      </Badge>
                      {`今日 ${client.usage.today.requests} · 本周 ${client.usage.week.requests} · 本月 ${client.usage.month.requests} · 成功率 ${client.usage.month.successRate}%`}
                    </span>
                  </div>
                  <div className="api-client-actions">
                    <IconButton label={`编辑 ${client.name}`} onClick={() => setEditor(editorFromClient(client))}>
                      <Settings2 size={14} />
                    </IconButton>
                    <IconButton label={`轮换 ${client.name} 密钥`} onClick={() => handleRotateClient(client)}>
                      <RotateCw size={14} />
                    </IconButton>
                    <IconButton label={`删除 ${client.name}`} onClick={() => handleDeleteClient(client)}>
                      <Trash2 size={14} />
                    </IconButton>
                  </div>
                </div>
              ))
            )}
          </div>
        </Panel>

        <Panel eyebrow="运行策略" title="可靠性与保留策略">
          <div className="api-settings-form">
            <label className="toggle-row api-key-policy-toggle">
              <input
                checked={settings.requireApiKey}
                onChange={(event) => setSettings((current) => ({ ...current, requireApiKey: event.target.checked }))}
                type="checkbox"
              />
              <span>所有非浏览器 API 请求强制使用 client 密钥</span>
            </label>
            <p className="field-hint">
              默认“本机兼容”保留当前 Codex 无感连接；浏览器跨域请求始终需要有效密钥。开启严格模式前请先创建 client。
            </p>
            <div className="form-grid">
              <NumberSetting
                label="重试预算"
                max={20}
                min={0}
                onChange={(retryBudget) => setSettings((current) => ({ ...current, retryBudget }))}
                value={settings.retryBudget}
              />
              <NumberSetting
                label="模型冷却（秒）"
                max={86400}
                min={5}
                onChange={(modelCooldownSeconds) => setSettings((current) => ({ ...current, modelCooldownSeconds }))}
                value={settings.modelCooldownSeconds}
              />
              <NumberSetting
                label="会话亲和（秒）"
                max={86400}
                min={60}
                onChange={(sessionAffinityTtlSeconds) => setSettings((current) => ({ ...current, sessionAffinityTtlSeconds }))}
                value={settings.sessionAffinityTtlSeconds}
              />
              <NumberSetting
                label="日志保留（天）"
                max={3650}
                min={1}
                onChange={(requestLogRetentionDays) => setSettings((current) => ({ ...current, requestLogRetentionDays }))}
                value={settings.requestLogRetentionDays}
              />
            </div>
            <div className="api-setting-notes">
              <span>重试预算 0 = 尝试分组内全部账号</span>
              <span>模型 404 / 429 只冷却“账号 + 模型”，不会误伤该账号的其他模型</span>
            </div>
            <Button disabled={action !== null} onClick={handleSaveSettings}>
              保存运行策略
            </Button>
          </div>
          {cooldowns.length > 0 ? (
            <div className="api-cooldown-list">
              <strong>当前模型冷却</strong>
              {cooldowns.map((cooldown) => (
                <div key={`${cooldown.providerId}-${cooldown.model}`}>
                  <span>{cooldown.model}</span>
                  <small>{providerTitle(status, cooldown.providerId)} · 至 {formatTime(cooldown.cooldownUntil)}</small>
                </div>
              ))}
            </div>
          ) : null}
        </Panel>
      </div>

      <Panel eyebrow="接入" title="OpenAI-compatible 调用方式">
        <div className="api-usage-grid">
          <div>
            <span>支持路径</span>
            <strong><code>/v1/responses</code> 与 <code>/v1/models</code></strong>
            <p>上游只有 Chat Completions 时，Companion 会独立完成请求、SSE、工具调用和错误语义转换。</p>
          </div>
          <pre className="api-code-sample"><code>{curlExample}</code></pre>
        </div>
      </Panel>

      <Panel eyebrow="结构化审计" title="API 请求日志">
        <div className="api-log-toolbar">
          <p className="relay-help">只记录路由元数据，不保存提示词、响应正文或完整密钥。日志持久化在本机 SQLite，每 2 秒自动刷新。</p>
          <div className="actions">
            <Button disabled={logsRefreshing} onClick={handleRefreshLogs} variant="ghost">
              <RefreshCw aria-hidden="true" className={logsRefreshing ? "spin-icon" : undefined} size={14} /> 刷新日志
            </Button>
            <Button disabled={requests.length === 0 || action !== null} onClick={handleClearLogs} variant="ghost">
              <Trash2 aria-hidden="true" size={14} /> 清空日志
            </Button>
          </div>
        </div>
        {requests.length === 0 ? (
          <div className="api-compact-empty"><Database size={18} /> 暂无 API 请求</div>
        ) : (
          <div className="api-request-table" role="table" aria-label="API 请求日志">
            <div className="api-request-head" role="row">
              <span>时间 / Client</span><span>请求</span><span>路由</span><span>结果</span>
            </div>
            {requests.map((request) => (
              <RequestRow
                events={requestEvents.get(request.requestId) ?? []}
                key={request.requestId}
                request={request}
                status={status}
              />
            ))}
          </div>
        )}
      </Panel>

      <details className="advanced-details api-diagnostics">
        <summary>查看底层转发诊断（{diagnosticGroups.length} 组 / {relayEvents.length} 条事件）</summary>
        <div className="relay-diagnostic-list">
          {diagnosticGroups.length === 0 ? (
            <div className="api-compact-empty">暂无诊断事件</div>
          ) : (
            diagnosticGroups.map((group) => (
              <RelayDiagnosticGroupRow
                group={group}
                key={diagnosticGroupKey(group)}
                request={group.type === "request" ? requestsById.get(group.requestId) : undefined}
                status={status}
              />
            ))
          )}
        </div>
      </details>

      <Dialog.Root open={Boolean(editor)} onOpenChange={(open) => !open && setEditor(null)}>
        <Dialog.Portal>
          <Dialog.Overlay className="dialog-overlay" />
          <Dialog.Content className="dialog-content api-client-dialog">
            <div className="dialog-header">
              <div>
                <Dialog.Title className="dialog-title">编辑 API client</Dialog.Title>
                <Dialog.Description className="dialog-description">修改名称、启用状态和允许模型；不会改变当前密钥。</Dialog.Description>
              </div>
              <Dialog.Close className="icon-button" aria-label="关闭"><X size={16} /></Dialog.Close>
            </div>
            {editor ? (
              <div className="api-editor-form">
                <Field label="Client 名称">
                  <input onChange={(event) => setEditor({ ...editor, name: event.target.value })} value={editor.name} />
                </Field>
                <Field label="允许模型">
                  <textarea onChange={(event) => setEditor({ ...editor, models: event.target.value })} value={editor.models} />
                </Field>
                <label className="toggle-row">
                  <input checked={editor.enabled} onChange={(event) => setEditor({ ...editor, enabled: event.target.checked })} type="checkbox" />
                  <span>启用此 client</span>
                </label>
                <div className="actions">
                  <Button disabled={action !== null} onClick={handleSaveClient}>保存 client</Button>
                  <Dialog.Close asChild><Button variant="secondary">取消</Button></Dialog.Close>
                </div>
              </div>
            ) : null}
          </Dialog.Content>
        </Dialog.Portal>
      </Dialog.Root>
    </div>
  );
}

function StatusItem({
  label,
  state,
  value,
}: {
  label: string;
  state: "error" | "ok" | "pending";
  value: string;
}) {
  return (
    <div className="api-status-item">
      <span>{label}</span>
      <strong>
        {state === "pending" ? (
          <RefreshCw className="spin-icon" size={13} />
        ) : state === "ok" ? (
          <CheckCircle2 size={13} />
        ) : (
          <XCircle size={13} />
        )}
        {value}
      </strong>
    </div>
  );
}

function poolHealthState(snapshot: ApiServiceSnapshot | null): "error" | "ok" | "pending" {
  if (!snapshot) return "pending";
  if (snapshot.poolHealth.degraded > 0) return "error";
  if (snapshot.poolHealth.healthy < snapshot.poolHealth.enabled) return "pending";
  return "ok";
}

function NumberSetting(props: {
  label: string;
  max: number;
  min: number;
  onChange: (value: number) => void;
  value: number;
}) {
  return (
    <Field label={props.label}>
      <input
        max={props.max}
        min={props.min}
        onChange={(event) => props.onChange(Number(event.target.value))}
        type="number"
        value={props.value}
      />
    </Field>
  );
}

function RequestRow(props: {
  events: RelayEvent[];
  request: ApiRequestLog;
  status: CompanionStatus;
}) {
  const { events, request, status } = props;
  const tone = requestOutcomeTone(request.outcome);
  const provider = request.providerId ? providerTitle(status, request.providerId) : "本地处理";
  const attemptViews = requestAttemptViews(request, events);
  const hasSwitch = request.attempts > 1 || attemptViews.some((attempt) => attempt.routeReason === "fallback");
  const attemptSummary = requestAttemptSummary(request.attempts, hasSwitch);
  const showTrace = shouldShowRequestTrace(request, attemptViews);

  return (
    <div className="api-request-row" role="row" title={request.error ?? undefined}>
      <div role="cell"><strong>{formatTime(request.startedAt)}</strong><span>{request.clientName ?? "本机兼容调用"}</span></div>
      <div role="cell"><strong>{request.method} {request.path}</strong><span>{request.model ?? "未指定模型"}</span></div>
      <div role="cell"><strong>{provider}</strong><span>{attemptSummary}</span></div>
      <div role="cell"><Badge tone={tone}>{request.statusCode ?? "—"} · {outcomeLabel(request.outcome)}</Badge><span>{request.latencyMs ?? 0} ms</span></div>
      {showTrace ? (
        <RequestAuditTrace attempts={attemptViews} request={request} status={status} />
      ) : null}
    </div>
  );
}

function RequestAuditTrace(props: {
  attempts: ApiRequestAttemptLog[];
  request: ApiRequestLog;
  status: CompanionStatus;
}) {
  const { attempts, request, status } = props;
  const traceStatus = requestTraceStatus(request, attempts);
  const failedAttempts = attempts.filter((attempt) => attempt.outcome === "failed");
  const detailUnavailable = request.attempts > 1 && attempts.length === 0;

  return (
    <div className="api-request-trace" role="cell">
      <Badge tone={traceStatus.tone}>{traceStatus.label}</Badge>
      <div className="api-request-trace-content">
        {attempts.length > 0 ? (
          <AttemptChain attempts={attempts} status={status} />
        ) : (
          <strong>检测到 {request.attempts} 次上游尝试</strong>
        )}
        {failedAttempts.length > 0 ? (
          <ul className="api-request-failure-list">
            {failedAttempts.map((attempt) => (
              <li key={attempt.attempt}>
                <span>{providerTitle(status, attempt.providerId)}</span>
                <span>{attempt.error ?? attemptStatusSummary(attempt)}</span>
              </li>
            ))}
          </ul>
        ) : null}
        {detailUnavailable ? (
          <span className="api-request-trace-note">这是升级前的历史记录，逐次失败明细未持久化。</span>
        ) : null}
      </div>
    </div>
  );
}

function AttemptChain(props: {
  attempts: ApiRequestAttemptLog[];
  status: CompanionStatus;
}) {
  return (
    <ol className="api-attempt-chain" aria-label="上游尝试链路">
      {props.attempts.map((attempt, index) => (
        <li className={`api-attempt api-attempt-${attempt.outcome}`} key={attempt.attempt}>
          <div>
            <strong>{providerTitle(props.status, attempt.providerId)}</strong>
            <span>{attemptOutcomeLabel(attempt.outcome)} · {attemptRouteReasonLabel(attempt.routeReason)}</span>
          </div>
          {index < props.attempts.length - 1 ? <ArrowRight aria-hidden="true" size={14} /> : null}
        </li>
      ))}
    </ol>
  );
}

function RelayDiagnosticGroupRow(props: {
  group: RelayDiagnosticGroup;
  request?: ApiRequestLog;
  status: CompanionStatus;
}) {
  const { group, request, status } = props;
  if (group.type === "standalone") {
    return <StandaloneDiagnosticEvent event={group.event} status={status} />;
  }

  const groupStatus = diagnosticGroupStatus(group.events);
  const requestEvent = group.events.find((event) => event.kind === "request");
  const requestLabel = diagnosticRequestLabel(request, requestEvent);

  return (
    <article className={`relay-diagnostic-group relay-diagnostic-${groupStatus.tone}`}>
      <header className="relay-diagnostic-header">
        <div>
          <strong>{requestLabel}</strong>
          <code>{group.requestId}</code>
        </div>
        <div className="relay-diagnostic-meta">
          <Badge tone={groupStatus.tone}>{groupStatus.label}</Badge>
          <time dateTime={group.latestAt}>{formatDiagnosticTime(group.latestAt)}</time>
        </div>
      </header>
      <ol className="relay-diagnostic-timeline" aria-label={`${requestLabel} 转发时间线`}>
        {group.events.map((event) => (
          <DiagnosticTimelineEvent event={event} key={`${event.timestamp}-${event.kind}-${event.message}`} status={status} />
        ))}
      </ol>
    </article>
  );
}

function DiagnosticTimelineEvent(props: { event: RelayEvent; status: CompanionStatus }) {
  const { event, status } = props;
  const provider = event.providerId ? providerTitle(status, event.providerId) : "Companion";
  const tone = diagnosticEventTone(event.kind);
  return (
    <li className={`relay-diagnostic-step relay-diagnostic-step-${tone}`}>
      <span aria-hidden="true" className="relay-diagnostic-dot" />
      <div>
        <strong>{eventKindLabel(event)} · {provider}</strong>
        <span>{relayEventMessageText(event)}</span>
      </div>
      <time dateTime={event.timestamp}>{formatDiagnosticTime(event.timestamp)}</time>
    </li>
  );
}

function StandaloneDiagnosticEvent(props: { event: RelayEvent; status: CompanionStatus }) {
  const { event, status } = props;
  const tone = diagnosticEventTone(event.kind);
  const provider = event.providerId ? providerTitle(status, event.providerId) : "Companion";
  return (
    <article className={`relay-diagnostic-group relay-diagnostic-${tone}`}>
      <header className="relay-diagnostic-header">
        <div>
          <strong>{eventKindLabel(event)} · {provider}</strong>
          <span>{relayEventMessageText(event)}</span>
        </div>
        <div className="relay-diagnostic-meta">
          <Badge tone={tone}>{eventKindLabel(event)}</Badge>
          <time dateTime={event.timestamp}>{formatDiagnosticTime(event.timestamp)}</time>
        </div>
      </header>
    </article>
  );
}

function relaySettingsFromStatus(status: CompanionStatus): RelaySettingsUpdate {
  const relay = status.config.relay;
  return {
    requireApiKey: relay.requireApiKey,
    retryBudget: relay.retryBudget,
    modelCooldownSeconds: relay.modelCooldownSeconds,
    sessionAffinityTtlSeconds: relay.sessionAffinityTtlSeconds,
    requestLogRetentionDays: relay.requestLogRetentionDays,
  };
}

function parseModels(value: string) {
  return [...new Set(value.split(/[\n,]/).map((model) => model.trim()).filter(Boolean))];
}

function editorFromClient(client: ApiClient): ClientEditor {
  return {
    id: client.id,
    name: client.name,
    models: client.allowedModels.join("\n"),
    enabled: client.enabled,
  };
}

function providerTitle(status: CompanionStatus, providerId: string) {
  const provider = status.config.providers[providerId];
  return provider ? providerAccountTitle(provider) : shortId(providerId, 28);
}

function outcomeLabel(outcome: string) {
  switch (outcome) {
    case "succeeded": return "成功";
    case "local": return "本地";
    case "failed": return "失败";
    case "rejected": return "拒绝";
    case "processing": return "处理中";
    default: return outcome;
  }
}

function requestOutcomeTone(outcome: string): BadgeTone {
  if (outcome === "succeeded" || outcome === "local") return "ok";
  if (outcome === "processing") return "info";
  return "danger";
}

function requestAttemptSummary(attempts: number, hasSwitch: boolean): string {
  if (attempts === 0) return "未访问上游";
  if (hasSwitch) return `${attempts} 次尝试 · 发生失败切换`;
  return `${attempts} 次尝试`;
}

function requestAttemptViews(
  request: ApiRequestLog,
  events: RelayEvent[],
): ApiRequestAttemptLog[] {
  if (request.attemptLog.length > 0) return request.attemptLog;

  const fallbackEvents = events.filter((event) => event.kind === "fallback" && event.providerId);
  const probeEvent = events.find((event) => event.kind === "failback" && event.providerId);
  if (fallbackEvents.length === 0 && !probeEvent) return [];

  const attempts: ApiRequestAttemptLog[] = fallbackEvents.map((event, index) => ({
    attempt: index + 1,
    providerId: event.providerId ?? "unknown",
    routeReason: legacyRouteReason(index, event.providerId, probeEvent),
    startedAt: event.timestamp,
    finishedAt: event.timestamp,
    statusCode: null,
    outcome: "failed",
    latencyMs: null,
    error: relayEventMessageText(event),
  }));

  const terminalEvent = [...events]
    .reverse()
    .find((event) => event.kind === "stream" || (event.kind === "error" && event.providerId));
  const terminalProviderId = request.providerId ?? terminalEvent?.providerId ?? probeEvent?.providerId;
  if (!terminalProviderId) return attempts;

  const terminalOutcome = requestTerminalOutcome(request.outcome);
  const terminalError = requestTerminalError(request, terminalEvent, probeEvent);
  attempts.push({
    attempt: attempts.length + 1,
    providerId: terminalProviderId,
    routeReason: attempts.length > 0 ? "fallback" : legacyProbeReason(probeEvent),
    startedAt: terminalEvent?.timestamp ?? request.startedAt,
    finishedAt: terminalEvent?.timestamp ?? null,
    statusCode: request.statusCode,
    outcome: terminalOutcome,
    latencyMs: request.latencyMs,
    error: terminalError,
  });
  return attempts;
}

function requestTerminalOutcome(outcome: string): string {
  if (outcome === "processing") return "processing";
  if (outcome === "succeeded") return "succeeded";
  return "failed";
}

function requestTerminalError(
  request: ApiRequestLog,
  terminalEvent: RelayEvent | undefined,
  probeEvent: RelayEvent | undefined,
): string | null {
  if (request.outcome === "succeeded" || request.outcome === "processing") return null;
  if (request.error) return request.error;
  if (terminalEvent) return relayEventMessageText(terminalEvent);
  if (probeEvent) return relayEventMessageText(probeEvent);
  return "上游请求失败";
}

function legacyRouteReason(
  index: number,
  providerId: string | null | undefined,
  probeEvent: RelayEvent | undefined,
): string {
  if (index > 0) return "fallback";
  if (probeEvent?.providerId === providerId) return legacyProbeReason(probeEvent);
  return "policy";
}

function legacyProbeReason(probeEvent: RelayEvent | undefined): string {
  if (!probeEvent) return "policy";
  return probeEvent.message.includes("自动") ? "automatic_failback" : "manual_failback";
}

function shouldShowRequestTrace(
  request: ApiRequestLog,
  attempts: ApiRequestAttemptLog[],
): boolean {
  if (request.attempts > 1) return true;
  if (request.outcome === "failed" || request.outcome === "rejected") return true;
  return attempts.some((attempt) => (
    attempt.outcome !== "succeeded"
      || attempt.routeReason === "manual_failback"
      || attempt.routeReason === "automatic_failback"
  ));
}

function requestTraceStatus(
  request: ApiRequestLog,
  attempts: ApiRequestAttemptLog[],
): { label: string; tone: BadgeTone } {
  if (request.outcome === "failed" || request.outcome === "rejected") {
    return { label: "请求失败", tone: "danger" };
  }
  if (request.outcome === "processing" && (
    attempts.some((attempt) => attempt.outcome === "failed") || request.attempts > 1
  )) {
    return { label: "失败后已切换，处理中", tone: "warn" };
  }
  if (attempts.some((attempt) => attempt.outcome === "failed") || request.attempts > 1) {
    return { label: "失败切换后成功", tone: "warn" };
  }
  if (attempts.some((attempt) => attempt.routeReason === "manual_failback")) {
    return { label: "手动向上探测", tone: "info" };
  }
  if (attempts.some((attempt) => attempt.routeReason === "automatic_failback")) {
    return { label: "自动向上探测", tone: "info" };
  }
  return { label: "尝试明细", tone: "info" };
}

function attemptOutcomeLabel(outcome: string): string {
  if (outcome === "succeeded") return "成功";
  if (outcome === "failed") return "失败";
  if (outcome === "processing") return "处理中";
  return outcome;
}

function attemptRouteReasonLabel(reason: string): string {
  if (reason === "policy") return "策略首选";
  if (reason === "affinity") return "会话亲和";
  if (reason === "fallback") return "失败后切换";
  if (reason === "manual_failback") return "手动向上探测";
  if (reason === "automatic_failback") return "自动向上探测";
  return reason;
}

function attemptStatusSummary(attempt: ApiRequestAttemptLog): string {
  const status = attempt.statusCode ? `HTTP ${attempt.statusCode}` : "未收到 HTTP 状态";
  if (attempt.latencyMs === null || attempt.latencyMs === undefined) return status;
  return `${status} · ${attempt.latencyMs} ms`;
}

function requestEventMap(groups: RelayDiagnosticGroup[]): Map<string, RelayEvent[]> {
  const entries = groups
    .filter((group): group is RelayRequestEventGroup => group.type === "request")
    .map((group) => [group.requestId, group.events] as const);
  return new Map(entries);
}

function diagnosticGroupKey(group: RelayDiagnosticGroup): string {
  if (group.type === "request") return `request-${group.requestId}`;
  return `event-${group.event.timestamp}-${group.event.kind}-${group.event.message}`;
}

function diagnosticRequestLabel(
  request: ApiRequestLog | undefined,
  requestEvent: RelayEvent | undefined,
): string {
  if (request) return `${request.method} ${request.path}`;
  if (requestEvent) return relayEventMessageText(requestEvent);
  return "上游请求";
}

function diagnosticGroupStatus(events: RelayEvent[]): { label: string; tone: BadgeTone } {
  const hasFallback = events.some((event) => event.kind === "fallback");
  const hasFailback = events.some((event) => event.kind === "failback");
  const lastTerminalEvent = [...events]
    .reverse()
    .find((event) => event.kind === "stream" || event.kind === "error");

  if (lastTerminalEvent?.kind === "error") return { label: "请求失败", tone: "danger" };
  if (hasFallback && lastTerminalEvent?.kind === "stream") {
    return { label: "失败切换后成功", tone: "warn" };
  }
  if (hasFallback) return { label: "失败后已切换，处理中", tone: "warn" };
  if (hasFailback) return { label: "向上探测", tone: "info" };
  if (lastTerminalEvent?.kind === "stream") return { label: "请求成功", tone: "ok" };
  return { label: "处理中", tone: "info" };
}

function diagnosticEventTone(kind: string): BadgeTone {
  if (kind === "fallback") return "warn";
  if (kind === "error") return "danger";
  if (kind === "stream") return "ok";
  if (kind === "failback") return "info";
  return "neutral";
}

function eventKindLabel(event: RelayEvent): string {
  if (event.kind === "fallback") return "上游失败，切换下一账号";
  if (event.kind === "error") return "请求失败";
  if (event.kind === "request") return "请求开始";
  if (event.kind === "stream") return "上游成功";
  if (event.kind === "health") return "健康检查";
  if (event.kind === "failback") {
    return event.message.includes("自动") ? "自动向上探测" : "手动向上探测";
  }
  return event.kind;
}

function formatDiagnosticTime(value: string): string {
  const date = new Date(value);
  if (Number.isNaN(date.getTime())) return value;
  return new Intl.DateTimeFormat("zh-CN", {
    hour: "2-digit",
    minute: "2-digit",
    second: "2-digit",
  }).format(date);
}
