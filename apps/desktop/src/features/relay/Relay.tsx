import { Copy, GitBranch, RadioTower, Route, ShieldCheck } from "lucide-react";
import type { ReactNode } from "react";
import { Badge, Button, Panel } from "../../components/ui";
import { formatTime } from "../../lib/format";
import { providerAccountSubtitle, providerAccountTitle, shortId } from "../../lib/provider-display";
import type { CompanionStatus, RelayEvent } from "../../types/domain";

export function Relay({ status }: { status: CompanionStatus }) {
  return (
    <div className="content-grid">
      <Panel eyebrow="本地转发" title="Companion 转发服务">
        <div className="relay-summary">
          <div className="relay-url-card">
            <RadioTower size={20} />
            <div>
              <span>Codex 使用这个地址时，账号切换不需要重启 Codex</span>
              <strong>{status.relayBaseUrl}</strong>
            </div>
          </div>
          <div className="actions">
            <Button onClick={() => void navigator.clipboard.writeText(status.relayBaseUrl)} variant="secondary">
              <Copy size={15} /> 复制地址
            </Button>
          </div>
        </div>
        <dl className="details-grid details-top">
          <dt>监听地址</dt>
          <dd>{status.config.relay.host}:{status.config.relay.port}</dd>
          <dt>当前分组</dt>
          <dd>{status.config.relay.activeGroupId}</dd>
          <dt>账号数量</dt>
          <dd>{status.activeProviders.length}</dd>
        </dl>
      </Panel>

      <Panel eyebrow="什么时候使用" title="转发模式">
        <div className="relay-mode-list">
          <RelayMode icon={<GitBranch size={16} />} title="启动账号分组" text="Codex 固定指向 Companion，分组内按优先级失败切换。" />
          <RelayMode icon={<ShieldCheck size={16} />} title="启动官方 Codex 账号" text="Companion 负责 OAuth token、账号 ID 和请求头注入。" />
          <RelayMode icon={<Route size={16} />} title="账号选择本地代理启动" text="API Key 账号也可以走这里，由 Companion 注入密钥、记录请求并参与分组路由。" />
        </div>
      </Panel>

      <Panel eyebrow="请求日志" title="本地转发记录">
        <p className="relay-help">
          这里记录 Codex 发到 Companion 本地转发服务的请求、上游错误和自动切换。直连账号不会经过这里。
        </p>
        {status.recentEvents.length === 0 ? (
          <div className="empty-state">
            <RadioTower size={28} />
            <p>启动账号分组、官方账号或本地转发 API Key 后，这里会显示请求记录。</p>
          </div>
        ) : (
          <div className="relay-event-list">
            {status.recentEvents.map((event) => (
              <div className={`relay-event-row relay-event-${event.kind}`} key={`${event.timestamp}-${event.message}`}>
                <div>
                  <strong>{relayEventTitle(status, event)}</strong>
                  {relayEventProviderHint(status, event) ? <small className="relay-event-provider">{relayEventProviderHint(status, event)}</small> : null}
                  <span className="relay-event-message" title={relayEventMessage(event.providerId, event.message)}>
                    {relayEventMessage(event.providerId, event.message)}
                  </span>
                </div>
                <div className="relay-event-meta">
                  <Badge tone={event.kind === "error" ? "danger" : event.kind === "fallback" ? "warn" : "info"}>{eventKindLabel(event.kind)}</Badge>
                  <small>{formatTime(event.timestamp)}</small>
                </div>
              </div>
            ))}
          </div>
        )}
      </Panel>
    </div>
  );
}

function relayEventTitle(status: CompanionStatus, event: RelayEvent) {
  const providerTitle = event.providerId ? providerEventTitle(status, event.providerId) : "Companion";
  switch (event.kind) {
    case "error":
      return `上游错误 · ${providerTitle}`;
    case "fallback":
      return `失败切换 · ${providerTitle}`;
    case "request":
      return event.providerId ? `请求 · ${providerTitle}` : "Companion 请求";
    case "health":
      return `健康检查 · ${providerTitle}`;
    default:
      return providerTitle;
  }
}

function providerEventTitle(status: CompanionStatus, providerId: string) {
  const provider = status.config.providers[providerId];
  return provider ? providerAccountTitle(provider) : shortId(providerId, 34);
}

function relayEventProviderHint(status: CompanionStatus, event: RelayEvent) {
  if (!event.providerId) return null;
  const provider = status.config.providers[event.providerId];
  if (!provider) return `Provider ID: ${shortId(event.providerId, 42)}`;
  const subtitle = providerAccountSubtitle(provider);
  return subtitle && subtitle !== event.providerId
    ? subtitle
    : `Provider ID: ${shortId(event.providerId, 42)}`;
}

function relayEventMessage(providerId: string | null | undefined, message: string) {
  const rawMessage = message || "";
  const companionMessage = explainRelayEventMessage(rawMessage);
  if (!providerId) return companionMessage || rawMessage || "Companion 本地转发事件";
  const returnedPrefix = `${providerId} returned `;
  if (message.startsWith(returnedPrefix)) {
    const detail = message.slice(returnedPrefix.length) || "错误响应";
    return explainRelayEventMessage(detail) || `上游返回 ${detail}`;
  }
  const failedPrefix = `${providerId} failed before stream: `;
  if (message.startsWith(failedPrefix)) {
    const detail = message.slice(failedPrefix.length) || "请求未写回 Codex，可继续 fallback";
    return explainRelayEventMessage(detail) || `stream 开始前请求失败：${detail}`;
  }
  const stripped = message.startsWith(providerId) ? message.slice(providerId.length).trimStart() : message;
  return explainRelayEventMessage(stripped) || stripped || "上游请求失败";
}

function explainRelayEventMessage(message: string) {
  const normalized = message.replace(/\s+/g, " ").trim();
  const lower = normalized.toLowerCase();
  if (!normalized) return null;
  if (normalized === "GET /v1") {
    return "Codex 正在探测 Companion 本地转发根地址。";
  }
  if (lower.includes("client_version") && lower.includes("field required")) {
    return "官方 Codex 请求缺少 client_version。当前版本已自动补齐；这是旧转发请求留下的日志，不是账号或额度问题。";
  }
  if (normalized.includes("404 Not Found") && (normalized.includes('"detail":"Not Found"') || normalized.includes('"detail": "Not Found"'))) {
    return "Codex 探测 /v1 根路径时官方后端返回 404。当前已由 Companion 本地处理；只要 /v1/models 成功，这条不影响使用。";
  }
  return null;
}

function RelayMode({ icon, title, text }: { icon: ReactNode; title: string; text: string }) {
  return (
    <div className="relay-mode-row">
      <div className="relay-mode-icon">{icon}</div>
      <div>
        <strong>{title}</strong>
        <span>{text}</span>
      </div>
    </div>
  );
}

function eventKindLabel(kind: string) {
  switch (kind) {
    case "fallback":
      return "失败切换";
    case "error":
      return "错误";
    case "request":
      return "请求";
    case "health":
      return "健康检查";
    default:
      return kind;
  }
}
