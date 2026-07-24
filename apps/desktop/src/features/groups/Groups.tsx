import * as Dialog from "@radix-ui/react-dialog";
import * as Select from "@radix-ui/react-select";
import { ArrowDown, ArrowUp, Play, Plus, Save, Settings2, X } from "lucide-react";
import { useCallback, useEffect, useMemo, useRef, useState, type FormEvent } from "react";
import { Badge, Button, Field, IconButton, Panel } from "../../components/ui";
import { currentApplication, userVisibleGroups } from "../../lib/current-application";
import { getApiRequestLogs } from "../../lib/api";
import { formatTime } from "../../lib/format";
import { apiRequestLogsEqual } from "../../lib/log-snapshot";
import { providerAccountTitle, providerHealthLabel, providerHealthTone, quotaInfo } from "../../lib/provider-display";
import type { ApiRequestLog, BusyState, CompanionStatus, GroupPolicy, GroupUpsert, ProviderConfig } from "../../types/domain";

const GROUP_ROUTE_REFRESH_INTERVAL_MS = 2_000;

export function Groups({
  active,
  busy,
  status,
  onSave,
  onLaunch,
  onUse,
}: {
  active: boolean;
  busy: BusyState;
  status: CompanionStatus;
  onSave: (group: GroupUpsert) => Promise<void>;
  onLaunch: (id: string) => Promise<void>;
  onUse: (id: string) => Promise<void>;
}) {
  const providers = useMemo(() => Object.values(status.config.providers), [status]);
  const groups = userVisibleGroups(status);
  const application = currentApplication(status);
  const [form, setForm] = useState<GroupUpsert>(newGroupDraft());
  const [open, setOpen] = useState(false);
  const [recentRequests, setRecentRequests] = useState<ApiRequestLog[]>([]);
  const routeRefreshInFlightRef = useRef(false);
  const disabled = busy !== "idle";
  const lastRoutedRequest = recentRequests.find(isSuccessfulProviderRequest) ?? null;
  const lastRoutedProviderId = lastRoutedRequest?.providerId ?? null;

  const loadRecentRequests = useCallback(async (): Promise<void> => {
    if (routeRefreshInFlightRef.current) return;
    routeRefreshInFlightRef.current = true;
    try {
      const nextRequests = await getApiRequestLogs();
      setRecentRequests((current) => apiRequestLogsEqual(current, nextRequests) ? current : nextRequests);
    } catch {
      // Keep the last known route marker when the local log database is briefly unavailable.
    } finally {
      routeRefreshInFlightRef.current = false;
    }
  }, []);

  useEffect(() => {
    if (!active) return;
    void loadRecentRequests();
    const timer = window.setInterval(() => {
      void loadRecentRequests();
    }, GROUP_ROUTE_REFRESH_INTERVAL_MS);
    return () => window.clearInterval(timer);
  }, [active, loadRecentRequests]);

  function openNewGroup() {
    setForm(newGroupDraft());
    setOpen(true);
  }

  function openEditor(group: GroupUpsert) {
    setForm({
      ...group,
      providerOrder: existingProviderIds(group.providerOrder, providers),
      providerWeights: group.providerWeights ?? {},
    });
    setOpen(true);
  }

  async function submit(event: FormEvent) {
    event.preventDefault();
    await onSave({
      ...form,
      id: form.id.trim(),
      name: form.name.trim(),
      providerOrder: existingProviderIds(form.providerOrder, providers),
      providerWeights: Object.fromEntries(
        form.providerOrder.map((providerId) => [providerId, form.providerWeights[providerId] ?? 1]),
      ),
    });
    setOpen(false);
  }

  function toggleProvider(provider: ProviderConfig, checked: boolean) {
    const providerOrder = checked
      ? [...form.providerOrder, provider.id].filter(unique)
      : form.providerOrder.filter((id) => id !== provider.id);
    const providerWeights = { ...form.providerWeights };
    if (checked) {
      providerWeights[provider.id] = providerWeights[provider.id] ?? 1;
    } else {
      delete providerWeights[provider.id];
    }
    setForm({ ...form, providerOrder, providerWeights });
  }

  function moveProvider(providerId: string, direction: -1 | 1) {
    const index = form.providerOrder.indexOf(providerId);
    const nextIndex = index + direction;
    if (index < 0 || nextIndex < 0 || nextIndex >= form.providerOrder.length) return;
    const providerOrder = [...form.providerOrder];
    [providerOrder[index], providerOrder[nextIndex]] = [providerOrder[nextIndex], providerOrder[index]];
    setForm({ ...form, providerOrder });
  }

  return (
    <div className="content-stack">
      <Panel eyebrow="分组" title="账号分组">
        <div className="panel-toolbar">
          <Button disabled={disabled} onClick={openNewGroup}>
            <Plus size={15} /> 新建分组
          </Button>
        </div>
        <div className="group-card-grid">
          {groups.map((group) => {
            const providerIds = existingProviderIds(group.providerOrder, providers);
            const isActiveGroup = application.kind === "group" && application.id === group.id;
            const recentlyUsedProviderId = isActiveGroup && lastRoutedProviderId && providerIds.includes(lastRoutedProviderId)
              ? lastRoutedProviderId
              : null;
            const recentlyUsedProvider = recentlyUsedProviderId
              ? status.config.providers[recentlyUsedProviderId]
              : null;
            const routeStatusLabel = activeGroupRouteLabel(
              isActiveGroup,
              recentlyUsedProvider,
              lastRoutedRequest?.startedAt ?? null,
            );
            return (
              <div className="group-card" key={group.id}>
                <div className="group-card-head">
                  <div>
                    <strong>{group.name}</strong>
                    <span>
                      {providerIds.length} 个账号 · {policyLabel(group.policy)}
                      {routeStatusLabel ? ` · ${routeStatusLabel}` : ""}
                    </span>
                  </div>
                  <div className="badge-row">
                    {isActiveGroup ? <Badge tone="ok">当前分组</Badge> : null}
                    <Badge tone={group.fallbackEnabled ? "accent" : "neutral"}>{group.fallbackEnabled ? "自动切换" : "固定首个"}</Badge>
                  </div>
                </div>
                {providerIds.length === 0 ? (
                  <p className="empty group-empty">这个分组还没有账号。</p>
                ) : (
                  <div className="group-provider-list">
                    {providerIds.map((id, index) => {
                          const provider = status.config.providers[id];
                          const health = status.config.health[id];
                          const quota = provider ? quotaInfo(provider.account) : null;
                          return (
                            <div className="group-provider-row" key={id}>
                              <span>{index + 1}</span>
                              <div className="group-provider-main">
                                <strong>{provider ? providerAccountTitle(provider) : id}</strong>
                                <small>{provider ? groupProviderMeta(provider, quota?.percentLabel) : "账号不存在"}</small>
                              </div>
                              <div className="group-provider-badges">
                                {recentlyUsedProviderId === id ? <Badge tone="accent">最近使用</Badge> : null}
                                <Badge tone={providerHealthTone(health?.status)}>{providerHealthLabel(health?.status)}</Badge>
                                {quota ? <Badge tone={quota.tone}>{quota.percentLabel}</Badge> : null}
                              </div>
                            </div>
                          );
                        })}
                  </div>
                )}
                <div className="row-actions">
                  <Button disabled={disabled} onClick={() => openEditor(group)} variant="secondary">
                    <Settings2 size={15} /> 编排分组
                  </Button>
                  <IconButton disabled={disabled} label={`启动分组：${group.name}`} onClick={() => void onLaunch(group.id)}>
                    <Play size={16} />
                  </IconButton>
                  <Button disabled={disabled || isActiveGroup} onClick={() => void onUse(group.id)} variant="secondary">
                    设为当前
                  </Button>
                </div>
              </div>
            );
          })}
          {groups.length === 0 ? <p className="empty">还没有用户分组。单账号启动会作为当前应用显示，不会占用这里的分组列表。</p> : null}
        </div>
      </Panel>

      <Dialog.Root open={open} onOpenChange={setOpen}>
        <Dialog.Portal>
          <Dialog.Overlay className="dialog-overlay" />
          <Dialog.Content className="dialog-content group-editor-dialog">
            <div className="dialog-header">
              <div>
                <Dialog.Title className="dialog-title">{status.config.groups[form.id] ? "编排分组" : "新建分组"}</Dialog.Title>
                <Dialog.Description className="dialog-description">
                  按顺序选择账号。启动分组后，自动切换会按这里的优先级执行。
                </Dialog.Description>
              </div>
              <Dialog.Close className="icon-button" aria-label="关闭">
                <X size={16} />
              </Dialog.Close>
            </div>

            <form onSubmit={submit}>
              <div className="form-grid">
                <Field label="分组名称">
                  <input value={form.name} onChange={(event) => setForm({ ...form, name: event.target.value, id: form.id || slugify(event.target.value) })} placeholder="工作账号" required />
                </Field>
                <Field label="分组 ID">
                  <input value={form.id} onChange={(event) => setForm({ ...form, id: event.target.value })} placeholder="work" required />
                </Field>
              </div>
              <Field label="切换方式">
                <Select.Root value={form.policy} onValueChange={(policy) => {
                  const nextPolicy = policy as GroupPolicy;
                  setForm({
                    ...form,
                    policy: nextPolicy,
                    fallbackEnabled: nextPolicy !== "manual",
                  });
                }}>
                  <Select.Trigger className="select-trigger">
                    <Select.Value />
                  </Select.Trigger>
                  <Select.Portal>
                    <Select.Content className="select-content" position="popper" sideOffset={4}>
                      <Select.Item className="select-item" value="priority_fallback">
                        <Select.ItemText>按优先级自动切换</Select.ItemText>
                      </Select.Item>
                      <Select.Item className="select-item" value="round_robin">
                        <Select.ItemText>轮询分配</Select.ItemText>
                      </Select.Item>
                      <Select.Item className="select-item" value="random">
                        <Select.ItemText>随机分配</Select.ItemText>
                      </Select.Item>
                      <Select.Item className="select-item" value="weighted">
                        <Select.ItemText>按权重分配</Select.ItemText>
                      </Select.Item>
                      <Select.Item className="select-item" value="least_loaded">
                        <Select.ItemText>优先最低负载</Select.ItemText>
                      </Select.Item>
                      <Select.Item className="select-item" value="manual">
                        <Select.ItemText>只使用第一个账号</Select.ItemText>
                      </Select.Item>
                    </Select.Content>
                  </Select.Portal>
                </Select.Root>
                <p className="field-hint">
                  会话亲和始终优先；新会话按所选策略决定首选账号，失败后可继续使用后备账号。
                </p>
              </Field>

              <div className="field">
                <span>加入分组的账号</span>
                {providers.length === 0 ? (
                  <p className="field-hint">先在账号页面添加账号。</p>
                ) : (
                  <div className="provider-picker">
                    {providers.map((provider) => {
                      const checked = form.providerOrder.includes(provider.id);
                      const health = status.config.health[provider.id];
                      return (
                        <label className="provider-option" key={provider.id}>
                          <input checked={checked} onChange={(event) => toggleProvider(provider, event.target.checked)} type="checkbox" />
                          <span>
                            <strong>{providerAccountTitle(provider)}</strong>
                            <small>{provider.id} · {providerHealthLabel(health?.status)}</small>
                          </span>
                        </label>
                      );
                    })}
                  </div>
                )}
              </div>

              <div className="selected-summary">
                <span>账号优先级</span>
                {form.providerOrder.length === 0 ? (
                  <Badge>空</Badge>
                ) : (
                  <div className="order-list">
                    {form.providerOrder.map((id, index) => {
                      const provider = status.config.providers[id];
                      return (
                        <div className="order-row" key={id}>
                          <div>
                            <strong>{index + 1}. {provider ? providerAccountTitle(provider) : id}</strong>
                            <small>{id}</small>
                          </div>
                          <div className="order-actions">
                            {form.policy === "weighted" ? (
                              <label className="weight-control">
                                <span>权重</span>
                                <input
                                  aria-label={`${provider ? providerAccountTitle(provider) : id} 的权重`}
                                  min={1}
                                  onChange={(event) => setForm({
                                    ...form,
                                    providerWeights: {
                                      ...form.providerWeights,
                                      [id]: Math.max(1, Number(event.target.value) || 1),
                                    },
                                  })}
                                  type="number"
                                  value={form.providerWeights[id] ?? 1}
                                />
                              </label>
                            ) : null}
                            <Button disabled={disabled || index === 0} onClick={() => moveProvider(id, -1)} type="button" variant="secondary">
                              <ArrowUp size={14} /> 上移
                            </Button>
                            <Button disabled={disabled || index === form.providerOrder.length - 1} onClick={() => moveProvider(id, 1)} type="button" variant="secondary">
                              <ArrowDown size={14} /> 下移
                            </Button>
                          </div>
                        </div>
                      );
                    })}
                  </div>
                )}
              </div>

              <label className="check-row">
                <input checked={form.fallbackEnabled} disabled={form.policy === "manual"} onChange={(event) => setForm({ ...form, fallbackEnabled: event.target.checked })} type="checkbox" />
                <span>请求失败时自动切换下一个账号</span>
              </label>
              <div className="actions">
                <Button disabled={disabled} type="submit">
                  <Save size={15} /> 保存分组
                </Button>
              </div>
            </form>
          </Dialog.Content>
        </Dialog.Portal>
      </Dialog.Root>
    </div>
  );
}

function newGroupDraft(): GroupUpsert {
  return {
    id: "",
    name: "",
    policy: "priority_fallback",
    providerOrder: [],
    providerWeights: {},
    fallbackEnabled: true,
  };
}

function policyLabel(policy: GroupPolicy) {
  const labels: Record<GroupPolicy, string> = {
    priority_fallback: "按优先级自动切换",
    round_robin: "轮询分配",
    random: "随机分配",
    weighted: "按权重分配",
    least_loaded: "优先最低负载",
    manual: "只使用第一个账号",
  };
  return labels[policy];
}

function unique(value: string, index: number, array: string[]) {
  return array.indexOf(value) === index;
}

function existingProviderIds(providerOrder: string[], providers: ProviderConfig[]) {
  const ids = new Set(providers.map((provider) => provider.id));
  return providerOrder.filter((id) => ids.has(id));
}

function slugify(value: string) {
  return value
    .trim()
    .toLowerCase()
    .replace(/[^a-z0-9_-]+/g, "-")
    .replace(/^-+|-+$/g, "");
}

function groupProviderMeta(provider: ProviderConfig, quotaLabel?: string) {
  const accountTitle = providerAccountTitle(provider);
  const providerName = provider.name !== accountTitle ? `${provider.name} · ` : "";
  if (provider.kind === "official_codex") {
    const plan = provider.account?.subscriptionType ?? "Codex 官方账号";
    const quota = quotaLabel && quotaLabel !== "待刷新" ? ` · 剩余额度 ${quotaLabel}` : "";
    return `${providerName}${plan}${quota}`;
  }
  const status = provider.account?.subscriptionStatus ?? "连接待检查";
  const quota = quotaLabel && quotaLabel !== "待刷新" ? ` · 余量 ${quotaLabel}` : "";
  return `${providerName}${status}${quota}`;
}

function isSuccessfulProviderRequest(request: ApiRequestLog): boolean {
  return request.outcome === "succeeded" && Boolean(request.providerId);
}

function activeGroupRouteLabel(
  isActiveGroup: boolean,
  provider: ProviderConfig | null,
  startedAt: string | null,
): string {
  if (!isActiveGroup) return "";
  if (!provider) return "等待新请求";
  const routeTime = startedAt ? ` · ${formatTime(startedAt)}` : "";
  return `最近使用 ${providerAccountTitle(provider)}${routeTime}`;
}
