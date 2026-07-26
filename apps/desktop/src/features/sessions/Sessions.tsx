import { Copy, History, Play, RefreshCw, RotateCcw, Search } from "lucide-react";
import { useCallback, useDeferredValue, useEffect, useRef, useState } from "react";
import { Badge, Button, Field, IconButton, Panel } from "../../components/ui";
import { getSessionPage, launchCli } from "../../lib/api";
import { userFacingError } from "../../lib/errors";
import { compactPath, formatTime } from "../../lib/format";
import type { CompanionStatus, SessionPage, SessionSummary } from "../../types/domain";

type SessionsProps = {
  active: boolean;
  status: CompanionStatus;
};

export function Sessions({ active, status }: SessionsProps) {
  const [codexDir, setCodexDir] = useState(status.codex.codexDir);
  const [query, setQuery] = useState("");
  const [page, setPage] = useState<SessionPage | null>(null);
  const [loading, setLoading] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const [copiedId, setCopiedId] = useState<string | null>(null);
  const [launchingId, setLaunchingId] = useState<string | null>(null);
  const [launchMessage, setLaunchMessage] = useState("");
  const loadRequestRef = useRef(0);
  const deferredQuery = useDeferredValue(query);

  const load = useCallback(async (rebuild = false): Promise<void> => {
    const requestId = loadRequestRef.current + 1;
    loadRequestRef.current = requestId;
    setLoading(true);
    try {
      const nextPage = await getSessionPage(codexDir, {
        query: deferredQuery,
        limit: 50,
        rebuild,
      });
      if (loadRequestRef.current === requestId) {
        setPage(nextPage);
        setError(null);
      }
    } catch (unknownError) {
      if (loadRequestRef.current === requestId) {
        setError(userFacingError(unknownError));
      }
    } finally {
      if (loadRequestRef.current === requestId) {
        setLoading(false);
      }
    }
  }, [codexDir, deferredQuery]);

  useEffect(() => {
    if (!active) return;
    const timer = window.setTimeout(() => void load(), deferredQuery ? 180 : 0);
    return () => window.clearTimeout(timer);
  }, [active, deferredQuery, load]);

  async function handleCopyResume(session: SessionSummary): Promise<void> {
    const command = `codex resume ${shellQuote(session.id)}`;
    await navigator.clipboard.writeText(command);
    setCopiedId(session.id);
    window.setTimeout(() => setCopiedId((current) => current === session.id ? null : current), 1600);
  }

  async function handleLaunchResume(session: SessionSummary): Promise<void> {
    const workingDirectory = status.config.app.recentWorkingDirectories[0] ?? status.codex.codexDir;
    setLaunchingId(session.id);
    setLaunchMessage("");
    try {
      const outcome = await launchCli({
        workingDirectory,
        terminal: status.config.app.preferredTerminal,
        resumeSessionId: session.id,
      });
      setLaunchMessage(outcome.message);
    } catch (unknownError) {
      setLaunchMessage(userFacingError(unknownError));
    } finally {
      setLaunchingId(null);
    }
  }

  const rootIsIsolated = status.dataRoots.companionIsolated || status.dataRoots.codexIsolated;
  const sessions = page?.sessions ?? [];

  return (
    <div className="content-grid sessions-stack">
      <Panel eyebrow="历史" title="Codex 会话">
        <div className="session-toolbar">
          <Field label="搜索会话">
            <div className="session-search-control">
              <Search aria-hidden="true" size={15} />
              <input
                onChange={(event) => setQuery(event.target.value)}
                placeholder="标题、Session ID、模型或 Provider"
                type="search"
                value={query}
              />
            </div>
          </Field>
          <Field label="Codex 数据目录">
            <input onChange={(event) => setCodexDir(event.target.value)} value={codexDir} />
          </Field>
          <div className="session-toolbar-actions">
            <Button disabled={loading} onClick={() => void load()} variant="secondary">
              <RefreshCw className={loading ? "spin-icon" : undefined} size={15} /> 刷新
            </Button>
            <IconButton disabled={loading} label="重建会话索引" onClick={() => void load(true)}>
              <RotateCcw size={15} />
            </IconButton>
          </div>
        </div>
        <div className="session-index-status" aria-live="polite">
          <span>{page?.total ?? 0} 个会话</span>
          <span>{page?.fromCache ? "首屏缓存" : "索引已更新"}</span>
          <span>{rootIsIsolated ? "隔离数据根" : "默认数据根"}</span>
          <code>{compactPath(page?.dataRoot ?? codexDir)}</code>
        </div>
        {error ? <div className="error-banner">{error}</div> : null}
        {launchMessage ? <p className="field-hint" role="status">{launchMessage}</p> : null}
      </Panel>

      <section className="session-list" aria-label="会话列表" aria-busy={loading}>
        {sessions.length === 0 ? (
          <div className="empty session-empty"><History size={20} />{loading ? "正在读取会话" : "没有匹配的会话"}</div>
        ) : sessions.map((session) => (
          <article className="session-row" key={session.path}>
            <div className="session-row-main">
              <div className="session-row-title">
                <strong>{session.title}</strong>
                {session.isRunning ? <Badge tone="ok">运行中</Badge> : null}
                {session.isSubagent ? <Badge tone="info">子任务</Badge> : null}
              </div>
              <span>{session.model}{session.providerId ? ` · ${session.providerId}` : ""}</span>
              {session.parentId ? (
                <span title={session.parentId}>父会话 {compactSessionId(session.parentId)}</span>
              ) : null}
              <code>{session.id}</code>
            </div>
            <div className="session-row-meta">
              <span>{formatTime(session.modifiedAt)}</span>
              <span>{formatBytes(session.bytes)}</span>
              <IconButton
                label={copiedId === session.id ? "恢复命令已复制" : `复制 ${session.title} 的恢复命令`}
                onClick={() => void handleCopyResume(session)}
              >
                <Copy size={14} />
              </IconButton>
              <IconButton
                disabled={launchingId !== null}
                label={`在终端恢复 ${session.title}`}
                onClick={() => void handleLaunchResume(session)}
              >
                <Play size={14} />
              </IconButton>
            </div>
          </article>
        ))}
      </section>
    </div>
  );
}

function formatBytes(bytes: number): string {
  if (bytes < 1024) return `${bytes} B`;
  if (bytes < 1024 * 1024) return `${Math.round(bytes / 1024)} KB`;
  return `${(bytes / (1024 * 1024)).toFixed(1)} MB`;
}

function shellQuote(value: string): string {
  return `'${value.replaceAll("'", `'\\''`)}'`;
}

function compactSessionId(value: string): string {
  return value.length > 16 ? `${value.slice(0, 8)}...${value.slice(-5)}` : value;
}
