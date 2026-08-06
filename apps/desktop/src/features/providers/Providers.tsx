import * as Dialog from "@radix-ui/react-dialog";
import * as Select from "@radix-ui/react-select";
import * as Tabs from "@radix-ui/react-tabs";
import {
  Check,
  CircleCheck,
  Copy,
  Download,
  Eye,
  EyeOff,
  ExternalLink,
  FileJson,
  FolderInput,
  Globe2,
  KeyRound,
  LayoutGrid,
  List,
  LoaderCircle,
  Plus,
  RefreshCw,
  ShieldCheck,
  Upload,
  X,
} from "lucide-react";
import {
  useEffect,
  useMemo,
  useRef,
  useState,
  type FormEvent,
  type ReactNode,
  type RefObject,
} from "react";
import { Button, Field, Panel } from "../../components/ui";
import {
  cancelCodexOAuth,
  getCodexOAuthStatus,
  getProviderImportProgress,
  getProviderRefreshProgress,
  openCodexOAuth,
  reviewProviderJsonMany,
  startCodexOAuth,
  submitCodexOAuthCallback,
} from "../../lib/api";
import { userFacingError } from "../../lib/errors";
import { providerKindLabel } from "../../lib/format";
import { providerAccountTitle, providerUsesAgentIdentity } from "../../lib/provider-display";
import type {
  ApiKeyProviderUpdate,
  BusyState,
  CodexOAuthStartResponse,
  CodexOAuthStatus,
  CompanionStatus,
  ProviderConfig,
  ProviderExportFormat,
  ProviderExportOutput,
  ProviderLaunchMode,
  ProviderImportBatchReport,
  ProviderImportProgress,
  ProviderImportReviewReport,
  ProviderRefreshProgress,
  ProviderViewMode,
} from "../../types/domain";
import { ProviderCard, ProviderCompactItem } from "./ProviderCards";
import { emptyApiKeyForm, type ApiKeyForm, type ApiKeyKind, type JsonImportFile } from "./provider-types";
import { UsageQueryForm, usageQueryPresetScript } from "./UsageQueryForm";

interface PendingJsonImport {
  sources: JsonImportFile[];
  addToGroupId: string | null;
  activeGroupName: string;
  review: ProviderImportReviewReport;
}

export function Providers({
  busy,
  refreshingAllProviders,
  refreshingProviderIds,
  status,
  onImportApiKey,
  onImportCodexOAuth,
  onImportJsonBatch,
  onImportLocal,
  onExport,
  onLaunch,
  onLaunchModeChange,
  onRemove,
  onRefresh,
  onRefreshAll,
  onUpdateApiKey,
  onViewModeChange,
  launchModes,
  viewMode,
}: {
  busy: BusyState;
  refreshingAllProviders: boolean;
  refreshingProviderIds: ReadonlySet<string>;
  status: CompanionStatus;
  launchModes: Record<string, ProviderLaunchMode>;
  onImportApiKey: (input: ApiKeyForm) => Promise<void>;
  onImportCodexOAuth: (loginId: string) => Promise<void>;
  onImportJsonBatch: (
    jsonFiles: JsonImportFile[],
    addToGroupId?: string | null,
  ) => Promise<ProviderImportBatchReport>;
  onImportLocal: () => Promise<void>;
  onExport: (id: string, format?: ProviderExportFormat | null) => Promise<ProviderExportOutput>;
  onLaunch: (id: string, mode?: ProviderLaunchMode) => Promise<void>;
  onLaunchModeChange: (providerId: string, mode: ProviderLaunchMode) => Promise<void>;
  onRemove: (id: string) => Promise<void>;
  onRefresh: (id: string) => Promise<void>;
  onRefreshAll: () => Promise<void>;
  onUpdateApiKey: (input: ApiKeyProviderUpdate) => Promise<void>;
  onViewModeChange: (mode: ProviderViewMode) => Promise<void>;
  viewMode: ProviderViewMode;
}) {
  const [apiKeyForm, setApiKeyForm] = useState<ApiKeyForm>(emptyApiKeyForm);
  const [editProvider, setEditProvider] = useState<ProviderConfig | null>(null);
  const [editForm, setEditForm] = useState<ApiKeyForm>(emptyApiKeyForm);
  const [editError, setEditError] = useState("");
  const [exportProvider, setExportProvider] = useState<ProviderConfig | null>(null);
  const [exportFormat, setExportFormat] = useState<ProviderExportFormat>("codex_companion");
  const [exportOutput, setExportOutput] = useState<ProviderExportOutput | null>(null);
  const [exportLoading, setExportLoading] = useState(false);
  const [exportError, setExportError] = useState("");
  const [exportHidden, setExportHidden] = useState(true);
  const [exportCopied, setExportCopied] = useState(false);
  const [jsonFiles, setJsonFiles] = useState<JsonImportFile[]>([]);
  const [pastedJson, setPastedJson] = useState("");
  const [addOpen, setAddOpen] = useState(false);
  const [apiKeyError, setApiKeyError] = useState("");
  const [refreshProgress, setRefreshProgress] = useState<ProviderRefreshProgress | null>(null);
  const [importProgress, setImportProgress] = useState<ProviderImportProgress | null>(null);
  const [importReport, setImportReport] = useState<ProviderImportBatchReport | null>(null);
  const [importReviewError, setImportReviewError] = useState("");
  const [importReviewing, setImportReviewing] = useState(false);
  const [pendingJsonImport, setPendingJsonImport] = useState<PendingJsonImport | null>(null);
  const [addToCurrentGroup, setAddToCurrentGroup] = useState(false);
  const fileInputRef = useRef<HTMLInputElement>(null);
  const exportRequestRef = useRef(0);
  const importReviewRequestRef = useRef(0);
  const importConfirmingRef = useRef(false);
  const disabled = busy !== "idle";
  const providers = Object.values(status.config.providers);
  const isRefreshingProviders = refreshingAllProviders || refreshingProviderIds.size > 0;
  const exportFormats = exportProvider ? exportFormatOptionsForProvider(exportProvider) : [];
  const maskedExportJson = useMemo(() => (exportOutput ? maskJsonPreviewContent(exportOutput.jsonContent) : ""), [exportOutput]);
  const exportPreviewText = exportOutput ? (exportHidden ? maskedExportJson : exportOutput.jsonContent) : "";
  const jsonImportSources = [
    ...jsonFiles,
    ...(pastedJson.trim() ? [{ name: "粘贴的 JSON", text: pastedJson.trim() }] : []),
  ];

  useEffect(() => {
    if (!isRefreshingProviders) {
      setRefreshProgress(null);
      return;
    }
    let cancelled = false;
    const poll = async (): Promise<void> => {
      try {
        const progress = await getProviderRefreshProgress();
        if (!cancelled) setRefreshProgress(progress);
      } catch {
        if (!cancelled) setRefreshProgress(null);
      }
    };
    void poll();
    const timer = window.setInterval(() => void poll(), 300);
    return () => {
      cancelled = true;
      window.clearInterval(timer);
    };
  }, [isRefreshingProviders]);

  useEffect(() => {
    if (busy !== "saving" || !addOpen) {
      setImportProgress(null);
      return;
    }
    let cancelled = false;
    const poll = async (): Promise<void> => {
      try {
        const progress = await getProviderImportProgress();
        if (!cancelled) setImportProgress(progress);
      } catch {
        if (!cancelled) setImportProgress(null);
      }
    };
    void poll();
    const timer = window.setInterval(() => void poll(), 200);
    return () => {
      cancelled = true;
      window.clearInterval(timer);
    };
  }, [addOpen, busy]);

  async function submitApiKey(event: FormEvent) {
    event.preventDefault();
    const input = {
      ...apiKeyForm,
      providerName: apiKeyForm.providerName.trim(),
      baseUrl: apiKeyForm.baseUrl.trim(),
      websocketUrl: apiKeyForm.websocketUrl.trim(),
      apiKey: apiKeyForm.apiKey.trim(),
      envVar: apiKeyForm.envVar.trim(),
      refreshIntervalSeconds: Number(apiKeyForm.refreshIntervalSeconds) || 60,
      usageQueryScript: apiKeyForm.usageQueryScript.trim(),
      usageQueryTimeoutSeconds: Number(apiKeyForm.usageQueryTimeoutSeconds) || 10,
      usageQueryApiKey: apiKeyForm.usageQueryApiKey.trim(),
      usageQueryBaseUrl: apiKeyForm.usageQueryBaseUrl.trim(),
      usageQueryAccessToken: apiKeyForm.usageQueryAccessToken.trim(),
      usageQueryUserId: apiKeyForm.usageQueryUserId.trim(),
    };
    if (!input.providerName || !input.baseUrl) {
      setApiKeyError("供应商名称和请求地址不能为空。");
      return;
    }
    if (!input.apiKey && !input.envVar) {
      setApiKeyError("至少填写 API Key 或 API Key 环境变量名。直连的密钥写入方式受“保留官方 Codex 登录”设置控制；本地代理由 Companion 注入密钥且不写 auth.json。");
      return;
    }
    const usageQueryError = validateUsageQueryForm(input, false);
    if (usageQueryError) {
      setApiKeyError(usageQueryError);
      return;
    }
    try {
      await onImportApiKey(input);
      setApiKeyForm(emptyApiKeyForm);
      setApiKeyError("");
      setAddOpen(false);
    } catch (unknownError) {
      setApiKeyError(userFacingError(unknownError));
    }
  }

  async function submitJsonBatch(event: FormEvent) {
    event.preventDefault();
    const requestId = importReviewRequestRef.current + 1;
    importReviewRequestRef.current = requestId;
    const sources = jsonImportSources.map((source) => ({ ...source }));
    const addToGroupId = addToCurrentGroup ? status.config.relay.activeGroupId : null;
    setImportReviewing(true);
    setImportReviewError("");
    setImportReport(null);
    try {
      const review = await reviewJsonImportSources(sources);
      if (importReviewRequestRef.current !== requestId) return;
      setPendingJsonImport({
        sources,
        addToGroupId,
        activeGroupName: status.activeGroup?.name ?? status.config.relay.activeGroupId,
        review,
      });
    } catch (unknownError) {
      if (importReviewRequestRef.current === requestId) {
        setImportReviewError(userFacingError(unknownError));
      }
    } finally {
      if (importReviewRequestRef.current === requestId) {
        setImportReviewing(false);
      }
    }
  }

  async function confirmJsonBatch() {
    if (!pendingJsonImport || pendingJsonImport.review.ready.length === 0 || importConfirmingRef.current) return;
    importConfirmingRef.current = true;
    try {
      const report = await onImportJsonBatch(
        pendingJsonImport.sources,
        pendingJsonImport.addToGroupId,
      );
      setImportReport(report);
      setPendingJsonImport(null);
      if (report.failed.length === 0) {
        setJsonFiles([]);
        setPastedJson("");
        if (fileInputRef.current) {
          fileInputRef.current.value = "";
        }
        setAddOpen(false);
      }
    } finally {
      importConfirmingRef.current = false;
    }
  }

  async function importLocalAccount() {
    try {
      await onImportLocal();
      setAddOpen(false);
    } catch {
      // The controller owns the global error message; keep the dialog open for retry.
    }
  }

  async function importCodexOAuthAccount(loginId: string) {
    await onImportCodexOAuth(loginId);
    setAddOpen(false);
  }

  function openEdit(provider: ProviderConfig) {
    setEditProvider(provider);
    setEditForm(apiKeyFormFromProvider(provider));
    setEditError("");
  }

  async function submitEdit(event: FormEvent) {
    event.preventDefault();
    if (!editProvider || !isApiKeyProvider(editProvider)) return;
    const input = {
      ...editForm,
      providerDisplayName: editForm.providerDisplayName.trim(),
      providerName: editForm.providerName.trim(),
      baseUrl: editForm.baseUrl.trim(),
      websocketUrl: editForm.websocketUrl.trim(),
      apiKey: editForm.apiKey.trim(),
      envVar: editForm.envVar.trim(),
      refreshIntervalSeconds: Number(editForm.refreshIntervalSeconds) || 60,
      usageQueryScript: editForm.usageQueryScript.trim(),
      usageQueryTimeoutSeconds: Number(editForm.usageQueryTimeoutSeconds) || 10,
      usageQueryApiKey: editForm.usageQueryApiKey.trim(),
      usageQueryBaseUrl: editForm.usageQueryBaseUrl.trim(),
      usageQueryAccessToken: editForm.usageQueryAccessToken.trim(),
      usageQueryUserId: editForm.usageQueryUserId.trim(),
    };
    if (!input.providerDisplayName || !input.providerName || !input.baseUrl) {
      setEditError("Provider Name、供应商名称和请求地址不能为空。");
      return;
    }
    const canReuseNewApiCredentials = editProvider.account?.usageQuery?.template === "new_api";
    const usageQueryError = validateUsageQueryForm(input, canReuseNewApiCredentials);
    if (usageQueryError) {
      setEditError(usageQueryError);
      return;
    }
    try {
      await onUpdateApiKey({
        id: editProvider.id,
        providerDisplayName: input.providerDisplayName,
        providerName: input.providerName,
        kind: input.kind,
        baseUrl: input.baseUrl,
        websocketUrl: input.websocketUrl || null,
        apiKey: input.apiKey || null,
        envVar: input.envVar || null,
        refreshIntervalSeconds: input.refreshIntervalSeconds,
        usageQuery: {
          enabled: input.usageQueryEnabled,
          template: input.usageQueryTemplate,
          baseUrl: input.usageQueryBaseUrl || null,
          script: input.usageQueryScript || null,
          timeoutSeconds: input.usageQueryTimeoutSeconds,
          apiKey: input.usageQueryApiKey || null,
          accessToken: input.usageQueryAccessToken || null,
          userId: input.usageQueryUserId || null,
        },
      });
      setEditProvider(null);
      setEditError("");
    } catch (unknownError) {
      setEditError(userFacingError(unknownError));
    }
  }

  async function loadExportPreview(provider: ProviderConfig, format: ProviderExportFormat) {
    const requestId = exportRequestRef.current + 1;
    exportRequestRef.current = requestId;
    setExportLoading(true);
    setExportError("");
    setExportOutput(null);
    try {
      const output = await onExport(provider.id, format);
      if (exportRequestRef.current === requestId) {
        setExportOutput(output);
      }
    } catch (unknownError) {
      if (exportRequestRef.current === requestId) {
        setExportError(userFacingError(unknownError));
      }
    } finally {
      if (exportRequestRef.current === requestId) {
        setExportLoading(false);
      }
    }
  }

  function startExport(provider: ProviderConfig) {
    const nextFormat: ProviderExportFormat = "codex_companion";
    setExportProvider(provider);
    setExportFormat(nextFormat);
    setExportOutput(null);
    setExportError("");
    setExportHidden(true);
    setExportCopied(false);
    void loadExportPreview(provider, nextFormat);
  }

  function closeExportDialog() {
    exportRequestRef.current += 1;
    setExportProvider(null);
    setExportFormat("codex_companion");
    setExportOutput(null);
    setExportLoading(false);
    setExportError("");
    setExportHidden(true);
    setExportCopied(false);
  }

  function selectExportFormat(format: ProviderExportFormat) {
    if (!exportProvider || format === exportFormat) return;
    setExportFormat(format);
    setExportHidden(true);
    setExportCopied(false);
    void loadExportPreview(exportProvider, format);
  }

  async function copyExportJson() {
    if (!exportOutput) return;
    await copyText(exportOutput.jsonContent);
    setExportCopied(true);
    window.setTimeout(() => setExportCopied(false), 1200);
  }

  function downloadExportJson() {
    if (!exportOutput) return;
    downloadJson(`${exportOutput.fileNameBase}.json`, exportOutput.jsonContent);
  }

  async function loadJsonFiles(fileList: FileList | null) {
    const files = Array.from(fileList ?? []);
    const loaded = await Promise.all(
      files.map(async (file) => ({
        name: file.name,
        text: await file.text(),
      })),
    );
    setJsonFiles(loaded);
  }

  return (
    <div className="content-stack">
      <Panel eyebrow="账号" title="账号列表">
        <div className="panel-toolbar provider-toolbar">
          <div className="toolbar-left">
            <Button disabled={disabled} onClick={() => setAddOpen(true)}>
              <Plus size={15} /> 添加账号
            </Button>
            <Button disabled={disabled || providers.length === 0 || isRefreshingProviders} onClick={() => void onRefreshAll()} variant="secondary">
              <RefreshCw aria-hidden="true" className={refreshingAllProviders ? "spin-icon" : undefined} size={15} /> 刷新全部
            </Button>
          </div>
          <div className="segmented-control" aria-label="Provider 展示方式">
            <button aria-pressed={viewMode === "compact"} onClick={() => void onViewModeChange("compact")} type="button">
              <List size={15} /> 紧凑
            </button>
            <button aria-pressed={viewMode === "cards"} onClick={() => void onViewModeChange("cards")} type="button">
              <LayoutGrid size={15} /> 卡片
            </button>
          </div>
        </div>
        {refreshProgress?.active ? (
          <div className="provider-refresh-progress" aria-live="polite">
            <progress max={Math.max(1, refreshProgress.total)} value={refreshProgress.completed} />
            <span>
              {refreshProgress.completed}/{refreshProgress.total}
              {refreshProgress.currentProviderId ? ` · ${refreshProgress.currentProviderId}` : ""}
            </span>
          </div>
        ) : null}

        {providers.length === 0 ? (
          <p className="empty">添加账号后，可以直接启动单个账号，也可以把多个账号编排成分组。</p>
        ) : viewMode === "compact" ? (
          <div className="provider-compact-list">
            {providers.map((provider) => (
              <ProviderCompactItem
                disabled={disabled}
                key={provider.id}
                provider={provider}
                refreshing={refreshingAllProviders || refreshingProviderIds.has(provider.id)}
                status={status}
                launchMode={launchModes[provider.id]}
                onLaunch={onLaunch}
                onLaunchModeChange={onLaunchModeChange}
                onEdit={openEdit}
                onExport={startExport}
                onRemove={onRemove}
                onRefresh={onRefresh}
              />
            ))}
          </div>
        ) : (
          <div className="provider-card-grid">
            {providers.map((provider) => (
              <ProviderCard
                disabled={disabled}
                key={provider.id}
                provider={provider}
                refreshing={refreshingAllProviders || refreshingProviderIds.has(provider.id)}
                status={status}
                launchMode={launchModes[provider.id]}
                onLaunch={onLaunch}
                onLaunchModeChange={onLaunchModeChange}
                onEdit={openEdit}
                onExport={startExport}
                onRefresh={onRefresh}
                onRemove={onRemove}
              />
            ))}
          </div>
        )}
      </Panel>

      <Dialog.Root open={addOpen} onOpenChange={(open) => {
        if (!open && disabled) return;
        setAddOpen(open);
        if (!open) {
          importReviewRequestRef.current += 1;
          setImportReviewing(false);
          setImportReviewError("");
          setImportReport(null);
          setPendingJsonImport(null);
        }
      }}>
        <Dialog.Portal>
          <Dialog.Overlay className="dialog-overlay" />
          <Dialog.Content className="dialog-content add-provider-dialog">
            <div className="dialog-header">
              <div>
                <Dialog.Title className="dialog-title">添加账号</Dialog.Title>
                <Dialog.Description className="dialog-description">
                  选择 OAuth、API Key、Token / JSON，或导入本机已有 Codex 账号。
                </Dialog.Description>
              </div>
              <Dialog.Close className="icon-button" aria-label="关闭" disabled={disabled}>
                <X size={16} />
              </Dialog.Close>
            </div>
            <ProviderAddTabs
              apiKeyForm={apiKeyForm}
              disabled={disabled}
              fileInputRef={fileInputRef}
              jsonImportSources={jsonImportSources}
              importProgress={importProgress}
              importReport={importReport}
              importReviewError={importReviewError}
              importReviewing={importReviewing}
              loadJsonFiles={loadJsonFiles}
              onImportLocal={importLocalAccount}
              onImportCodexOAuth={importCodexOAuthAccount}
              apiKeyError={apiKeyError}
              pastedJson={pastedJson}
              addToCurrentGroup={addToCurrentGroup}
              activeGroupName={status.activeGroup?.name ?? status.config.relay.activeGroupId}
              setApiKeyForm={setApiKeyForm}
              setApiKeyError={setApiKeyError}
              setPastedJson={setPastedJson}
              setAddToCurrentGroup={setAddToCurrentGroup}
              submitApiKey={submitApiKey}
              submitJsonBatch={submitJsonBatch}
            />
          </Dialog.Content>
        </Dialog.Portal>
      </Dialog.Root>

      <Dialog.Root
        open={Boolean(pendingJsonImport)}
        onOpenChange={(open) => {
          if (!open && !disabled) setPendingJsonImport(null);
        }}
      >
        <Dialog.Portal>
          <Dialog.Overlay className="dialog-overlay" />
          <Dialog.Content className="dialog-content provider-import-review-dialog">
            <div className="dialog-header">
              <div>
                <Dialog.Title className="dialog-title">确认导入目标</Dialog.Title>
                <Dialog.Description className="dialog-description">
                  请核对目标地址、凭据类型和覆盖行为。敏感值不会显示在预览中。
                </Dialog.Description>
              </div>
              <Dialog.Close className="icon-button" aria-label="关闭导入确认" disabled={disabled}>
                <X size={16} />
              </Dialog.Close>
            </div>
            {pendingJsonImport ? (
              <ProviderImportReview
                disabled={disabled}
                pendingImport={pendingJsonImport}
                onCancel={() => setPendingJsonImport(null)}
                onConfirm={() => void confirmJsonBatch()}
              />
            ) : null}
          </Dialog.Content>
        </Dialog.Portal>
      </Dialog.Root>

      <Dialog.Root
        open={Boolean(editProvider)}
        onOpenChange={(open) => {
          if (!open && !disabled) setEditProvider(null);
        }}
      >
        <Dialog.Portal>
          <Dialog.Overlay className="dialog-overlay" />
          <Dialog.Content className="dialog-content api-key-edit-dialog">
            <div className="dialog-header">
              <div>
                <Dialog.Title className="dialog-title">编辑 API Key Provider</Dialog.Title>
                <Dialog.Description className="dialog-description">
                  API Key 留空会保留原密钥；只改名称或请求地址时不需要重新填写密钥。
                </Dialog.Description>
              </div>
              <Dialog.Close className="icon-button" aria-label="关闭" disabled={disabled}>
                <X size={16} />
              </Dialog.Close>
            </div>
            <ApiKeyEditForm
              disabled={disabled}
              editError={editError}
              form={editForm}
              providerId={editProvider?.id}
              setEditError={setEditError}
              setForm={setEditForm}
              submit={submitEdit}
            />
          </Dialog.Content>
        </Dialog.Portal>
      </Dialog.Root>

      <Dialog.Root open={Boolean(exportProvider)} onOpenChange={(open) => !open && closeExportDialog()}>
        <Dialog.Portal>
          <Dialog.Overlay className="dialog-overlay" />
          <Dialog.Content className="dialog-content provider-export-dialog">
            <div className="dialog-header">
              <div>
                <Dialog.Title className="dialog-title">导出 JSON</Dialog.Title>
                <Dialog.Description className="dialog-description">
                  {exportProvider ? providerAccountTitle(exportProvider) : "Provider"} · 默认隐藏关键信息，复制和下载使用完整 JSON。
                </Dialog.Description>
              </div>
              <Dialog.Close className="icon-button" aria-label="关闭">
                <X size={16} />
              </Dialog.Close>
            </div>
            <div className="export-preview-toolbar">
              <div className="export-format-options" aria-label="导出格式">
                <span className="export-format-label">导出格式</span>
                {exportFormats.map((format) => (
                  <button
                    aria-pressed={exportFormat === format}
                    className="export-format-option"
                    disabled={disabled || exportLoading}
                    key={format}
                    onClick={() => selectExportFormat(format)}
                    type="button"
                  >
                    {EXPORT_FORMAT_LABELS[format]}
                  </button>
                ))}
              </div>
              <div className="export-preview-actions">
                <Button disabled={!exportOutput || exportLoading} onClick={() => setExportHidden((current) => !current)} type="button" variant="secondary">
                  {exportHidden ? <Eye size={15} /> : <EyeOff size={15} />}
                  {exportHidden ? "显示" : "隐藏"}
                </Button>
                <Button disabled={!exportOutput || exportLoading} onClick={() => void copyExportJson()} type="button" variant="secondary">
                  {exportCopied ? <Check size={15} /> : <Copy size={15} />}
                  {exportCopied ? "已复制" : "复制"}
                </Button>
                <Button disabled={!exportOutput || exportLoading} onClick={downloadExportJson} type="button">
                  <Download size={15} /> 下载
                </Button>
              </div>
            </div>
            {exportError ? <p className="field-error">{exportError}</p> : null}
            <textarea
              className="export-json-textarea"
              readOnly
              value={exportLoading ? "正在生成 JSON..." : exportOutput ? exportPreviewText : "暂无 JSON"}
            />
          </Dialog.Content>
        </Dialog.Portal>
      </Dialog.Root>
    </div>
  );
}

async function reviewJsonImportSources(sources: JsonImportFile[]): Promise<ProviderImportReviewReport> {
  const combined: ProviderImportReviewReport = {
    total: 0,
    ready: [],
    failed: [],
  };
  const reviewedProviderIds = new Set<string>();
  for (const source of sources) {
    const offset = combined.total;
    try {
      const report = await reviewProviderJsonMany(source.text);
      combined.total += report.total;
      for (const item of report.ready) {
        const willOverwrite = item.willOverwrite || reviewedProviderIds.has(item.providerId);
        reviewedProviderIds.add(item.providerId);
        combined.ready.push({
          ...item,
          index: item.index + offset,
          label: `${source.name} · ${item.label}`,
          willOverwrite,
        });
      }
      combined.failed.push(
        ...report.failed.map((failure) => ({
          ...failure,
          index: failure.index + offset,
          label: `${source.name} · ${failure.label}`,
        })),
      );
    } catch (unknownError) {
      combined.total += 1;
      combined.failed.push({
        index: offset,
        label: source.name,
        message: userFacingError(unknownError),
      });
    }
  }
  return combined;
}

function ProviderImportReview({
  disabled,
  pendingImport,
  onCancel,
  onConfirm,
}: {
  disabled: boolean;
  pendingImport: PendingJsonImport;
  onCancel: () => void;
  onConfirm: () => void;
}) {
  const { review } = pendingImport;
  const overwriteCount = review.ready.filter((item) => item.willOverwrite).length;
  const groupDescription = pendingImport.addToGroupId
    ? `成功导入后加入当前分组“${pendingImport.activeGroupName}”`
    : "成功导入后不会自动加入分组";

  return (
    <div className="provider-import-review">
      <div className="provider-import-review-summary" aria-live="polite">
        <strong>{review.ready.length} 项可导入</strong>
        <span>{review.failed.length} 项无法导入</span>
        {overwriteCount > 0 ? <span className="review-overwrite-summary">{overwriteCount} 项将覆盖现有 Provider</span> : null}
      </div>

      {review.ready.length > 0 ? (
        <div className="provider-import-review-list" aria-label="可导入项目">
          {review.ready.map((item) => (
            <article className="provider-import-review-item" key={`${item.index}-${item.providerId}`}>
              <div className="provider-import-review-item-header">
                <div>
                  <strong>{item.providerName}</strong>
                  <span>{item.label}</span>
                </div>
                <div className="provider-import-review-badges">
                  <span>{item.credentialKind}</span>
                  {item.willOverwrite ? <span className="review-overwrite-badge">覆盖现有</span> : <span>新建</span>}
                </div>
              </div>
              <dl className="provider-import-review-details">
                <div>
                  <dt>Provider ID</dt>
                  <dd><code>{item.providerId}</code></dd>
                </div>
                <div>
                  <dt>类型</dt>
                  <dd>{providerKindLabel(item.providerKind)}</dd>
                </div>
                <div className="review-detail-wide">
                  <dt>请求地址</dt>
                  <dd><code>{item.baseUrl}</code></dd>
                </div>
                {item.websocketUrl ? (
                  <div className="review-detail-wide">
                    <dt>WebSocket</dt>
                    <dd><code>{item.websocketUrl}</code></dd>
                  </div>
                ) : null}
                {item.model ? (
                  <div>
                    <dt>模型</dt>
                    <dd>{item.model}</dd>
                  </div>
                ) : null}
              </dl>
            </article>
          ))}
        </div>
      ) : null}

      {review.failed.length > 0 ? (
        <div className="warning-box provider-import-review-failures" role="status">
          <strong>{review.failed.length} 项不会导入</strong>
          {review.failed.map((failure) => (
            <p key={`${failure.index}-${failure.label}`}>{failure.label}：{failure.message}</p>
          ))}
        </div>
      ) : null}

      <div className="provider-import-review-safety">
        <p>{groupDescription}</p>
        <p>OAuth token、API Key 与 Agent Identity 私钥均已隐藏；确认后才会写入本机私密文件。</p>
      </div>
      <div className="actions provider-import-review-actions">
        <Button disabled={disabled} onClick={onCancel} variant="secondary">返回修改</Button>
        <Button disabled={disabled || review.ready.length === 0} onClick={onConfirm}>
          <Upload size={15} /> 确认导入 {review.ready.length} 项
        </Button>
      </div>
    </div>
  );
}

function ApiKeyEditForm({
  disabled,
  editError,
  form,
  providerId,
  setEditError,
  setForm,
  submit,
}: {
  disabled: boolean;
  editError: string;
  form: ApiKeyForm;
  providerId?: string;
  setEditError: (value: string) => void;
  setForm: (form: ApiKeyForm) => void;
  submit: (event: FormEvent) => Promise<void>;
}) {
  function update(form: ApiKeyForm) {
    setEditError("");
    setForm(form);
  }

  return (
    <form onSubmit={submit}>
      <ApiKeyFormTabs
        disabled={disabled}
        form={form}
        isEditing
        providerId={providerId}
        update={update}
      >
        <Field label="Provider Name">
          <input value={form.providerDisplayName} onChange={(event) => update({ ...form, providerDisplayName: event.target.value })} required />
        </Field>
        <Field label="供应商名称">
          <input value={form.providerName} onChange={(event) => update({ ...form, providerName: event.target.value })} required />
        </Field>
        <Field label="请求地址">
          <input
            value={form.baseUrl}
            onChange={(event) => update({ ...form, baseUrl: event.target.value })}
            placeholder="https://api.example.com/v1 或完整 endpoint"
            required
          />
        </Field>
        <Field label="Responses WebSocket（可选）">
          <input
            value={form.websocketUrl}
            onChange={(event) => update({ ...form, websocketUrl: event.target.value })}
            placeholder="wss://api.example.com/v1/responses"
          />
        </Field>
        <p className="field-hint">
          这里不决定直连或代理；启动方式以账号卡片上的选择为准。本地代理下填 /v1 会按 Codex 请求路径拼接，填完整 endpoint 则按原地址发送。
        </p>
        <Field label="API Key（留空保留）">
          <input value={form.apiKey} onChange={(event) => update({ ...form, apiKey: event.target.value })} placeholder="sk-..." type="password" />
        </Field>
        <details className="advanced-details" open={Boolean(form.envVar)}>
          <summary>高级选项</summary>
          <Field label="环境变量名">
            <input value={form.envVar} onChange={(event) => update({ ...form, envVar: event.target.value })} placeholder="例如 OPENROUTER_API_KEY" />
          </Field>
          <p className="field-hint">
            只有你已经把 API Key 放进系统环境变量时才填写。普通导入的 provider 保持为空即可。
          </p>
        </details>
        <Field label="状态刷新间隔（秒）">
          <input min={15} type="number" value={form.refreshIntervalSeconds} onChange={(event) => update({ ...form, refreshIntervalSeconds: Number(event.target.value) })} />
        </Field>
      </ApiKeyFormTabs>
      {editError ? <p className="field-error">{editError}</p> : null}
      <div className="actions">
        <Button disabled={disabled} type="submit">
          <KeyRound size={15} /> 保存
        </Button>
      </div>
    </form>
  );
}

function isApiKeyProvider(provider: ProviderConfig): provider is ProviderConfig & { kind: ApiKeyKind } {
  return provider.kind === "openai_compatible" || provider.kind === "relay_provider";
}

const EXPORT_FORMAT_LABELS: Record<ProviderExportFormat, string> = {
  codex_companion: "Codex Companion",
  sub2api: "Sub2API",
  cpa: "CPA",
};

function exportFormatOptionsForProvider(provider: ProviderConfig): ProviderExportFormat[] {
  if (isApiKeyProvider(provider)) return ["codex_companion"];
  if (providerUsesAgentIdentity(provider)) return ["codex_companion", "sub2api"];
  return ["codex_companion", "sub2api", "cpa"];
}

function apiKeyFormFromProvider(provider: ProviderConfig): ApiKeyForm {
  const authRef = provider.directAuthRef?.trim() || provider.authRef?.trim() || "";
  const usageQuery = provider.account?.usageQuery;
  const usageQueryTemplate = usageQuery?.template ?? "general";
  return {
    providerDisplayName: provider.account?.email || providerAccountTitle(provider),
    providerName: provider.name,
    kind: isApiKeyProvider(provider) ? provider.kind : "openai_compatible",
    baseUrl: provider.baseUrl,
    websocketUrl: provider.websocketUrl ?? "",
    apiKey: "",
    envVar: authRef.startsWith("env:") ? authRef.slice("env:".length) : "",
    usageQueryEnabled: Boolean(usageQuery),
    usageQueryTemplate,
    usageQueryScript: usageQuery
      ? usageQuery.script.trim() || usageQueryPresetScript(usageQueryTemplate)
      : "",
    usageQueryTimeoutSeconds: usageQuery?.timeoutSeconds ?? 10,
    usageQueryApiKey: "",
    usageQueryBaseUrl: usageQuery?.baseUrl ?? "",
    usageQueryAccessToken: "",
    usageQueryUserId: "",
    refreshIntervalSeconds: provider.refreshIntervalSeconds || 60,
  };
}

function ApiKeyFormTabs({
  children,
  disabled,
  form,
  isEditing,
  providerId,
  update,
}: {
  children: ReactNode;
  disabled: boolean;
  form: ApiKeyForm;
  isEditing: boolean;
  providerId?: string;
  update: (form: ApiKeyForm) => void;
}) {
  return (
    <Tabs.Root className="api-key-form-tabs" defaultValue="connection">
      <Tabs.List className="api-key-form-tabs-list" aria-label="API Key Provider 配置">
        <Tabs.Trigger className="api-key-form-tabs-trigger" value="connection">
          基础信息
        </Tabs.Trigger>
        <Tabs.Trigger className="api-key-form-tabs-trigger" value="usage-query">
          余额查询
        </Tabs.Trigger>
      </Tabs.List>
      <Tabs.Content className="api-key-form-tabs-content" value="connection">
        {children}
      </Tabs.Content>
      <Tabs.Content className="api-key-form-tabs-content" value="usage-query">
        <UsageQueryForm
          disabled={disabled}
          form={form}
          isEditing={isEditing}
          key={providerId ?? "new-provider"}
          providerId={providerId}
          update={update}
        />
      </Tabs.Content>
    </Tabs.Root>
  );
}

function validateUsageQueryForm(
  form: ApiKeyForm,
  canReuseNewApiCredentials: boolean,
): string | null {
  if (!form.usageQueryEnabled) return null;
  if (!form.usageQueryScript.trim()) return "余额查询脚本不能为空。";
  if (
    form.usageQueryTemplate === "new_api" &&
    !canReuseNewApiCredentials &&
    (!form.usageQueryAccessToken || !form.usageQueryUserId)
  ) {
    return "NewAPI 余额查询需要个人访问令牌和用户 ID。";
  }
  if (
    !Number.isFinite(form.usageQueryTimeoutSeconds) ||
    form.usageQueryTimeoutSeconds < 2 ||
    form.usageQueryTimeoutSeconds > 30
  ) {
    return "余额查询超时必须在 2 到 30 秒之间。";
  }
  return null;
}

function downloadJson(fileName: string, jsonContent: string) {
  const blob = new Blob([`${jsonContent.trimEnd()}\n`], { type: "application/json;charset=utf-8" });
  const url = URL.createObjectURL(blob);
  const anchor = document.createElement("a");
  anchor.href = url;
  anchor.download = sanitizeDownloadName(fileName);
  document.body.append(anchor);
  anchor.click();
  anchor.remove();
  URL.revokeObjectURL(url);
}

function sanitizeDownloadName(fileName: string) {
  return fileName.replace(/[<>:"/\\|?*\x00-\x1F]/g, "_").replace(/_+/g, "_") || "provider.json";
}

async function copyText(text: string) {
  if (navigator.clipboard?.writeText) {
    await navigator.clipboard.writeText(text);
    return;
  }
  const textarea = document.createElement("textarea");
  textarea.value = text;
  textarea.style.position = "fixed";
  textarea.style.opacity = "0";
  document.body.append(textarea);
  textarea.select();
  document.execCommand("copy");
  textarea.remove();
}

function maskJsonPreviewContent(jsonContent: string) {
  const trimmed = jsonContent.trim();
  if (!trimmed) return "";
  try {
    return JSON.stringify(maskJsonValue(JSON.parse(trimmed)), null, 2);
  } catch {
    return maskSecretString(trimmed);
  }
}

function maskJsonValue(value: unknown): unknown {
  if (typeof value === "string") return maskSecretString(value);
  if (Array.isArray(value)) return value.map(maskJsonValue);
  if (value && typeof value === "object") {
    return Object.fromEntries(Object.entries(value as Record<string, unknown>).map(([key, item]) => [key, maskJsonValue(item)]));
  }
  return value;
}

function maskSecretString(value: string) {
  if (!value) return value;
  if (value.length <= 4) return "*".repeat(value.length);
  if (value.length <= 8) return `${value.slice(0, 1)}***${value.slice(-1)}`;
  return `${value.slice(0, 2)}***${value.slice(-2)}`;
}

type OAuthFlowState =
  | { phase: "idle" }
  | { phase: "starting" }
  | { phase: "waiting"; session: CodexOAuthStartResponse }
  | { phase: "ready"; session: CodexOAuthStartResponse }
  | { phase: "completing"; session: CodexOAuthStartResponse };

function oauthSessionFromStatus(
  status: CodexOAuthStatus,
): CodexOAuthStartResponse {
  return {
    loginId: status.loginId,
    authUrl: status.authUrl,
    callbackUrl: status.callbackUrl,
    expiresAt: status.expiresAt,
    callbackServerReady: status.callbackServerReady,
  };
}

function CodexOAuthPanel({
  disabled,
  onComplete,
}: {
  disabled: boolean;
  onComplete: (loginId: string) => Promise<void>;
}) {
  const [flow, setFlow] = useState<OAuthFlowState>({ phase: "idle" });
  const [callbackUrl, setCallbackUrl] = useState("");
  const [error, setError] = useState("");
  const [copied, setCopied] = useState(false);
  const statusRevisionRef = useRef(0);
  const session = "session" in flow ? flow.session : null;
  const isWorking = flow.phase === "starting" || flow.phase === "completing";
  const callbackReceived = flow.phase === "ready" || flow.phase === "completing";

  useEffect(() => {
    let disposed = false;
    let requestActive = false;

    async function pollStatus(): Promise<void> {
      if (requestActive) return;
      requestActive = true;
      const revision = statusRevisionRef.current;
      try {
        const status = await getCodexOAuthStatus();
        if (disposed || revision !== statusRevisionRef.current) return;
        if (status?.error) {
          setError(status.error);
        }
        setFlow((current) => {
          if (current.phase === "starting" || current.phase === "completing") {
            return current;
          }
          if (!status) {
            return current.phase === "idle" ? current : { phase: "idle" };
          }
          const nextSession = oauthSessionFromStatus(status);
          return status.callbackReceived
            ? { phase: "ready", session: nextSession }
            : { phase: "waiting", session: nextSession };
        });
      } catch (unknownError) {
        if (!disposed) {
          setError(userFacingError(unknownError));
        }
      } finally {
        requestActive = false;
      }
    }

    void pollStatus();
    const timer = window.setInterval(() => void pollStatus(), 500);
    return () => {
      disposed = true;
      window.clearInterval(timer);
    };
  }, []);

  async function handleStart(): Promise<void> {
    statusRevisionRef.current += 1;
    setError("");
    setCopied(false);
    setFlow({ phase: "starting" });
    try {
      const nextSession = await startCodexOAuth();
      setFlow({ phase: "waiting", session: nextSession });
    } catch (unknownError) {
      setFlow({ phase: "idle" });
      setError(userFacingError(unknownError));
    } finally {
      statusRevisionRef.current += 1;
    }
  }

  async function handleOpenBrowser(): Promise<void> {
    if (!session) return;
    setError("");
    try {
      await openCodexOAuth(session.loginId);
    } catch (unknownError) {
      setError(userFacingError(unknownError));
    }
  }

  async function handleCopyUrl(): Promise<void> {
    if (!session) return;
    setError("");
    try {
      await copyText(session.authUrl);
      setCopied(true);
      window.setTimeout(() => setCopied(false), 1200);
    } catch (unknownError) {
      setCopied(false);
      setError(userFacingError(unknownError));
    }
  }

  async function handleSubmitCallback(): Promise<void> {
    if (!session || !callbackUrl.trim()) return;
    statusRevisionRef.current += 1;
    setError("");
    try {
      await submitCodexOAuthCallback(session.loginId, callbackUrl);
      setFlow({ phase: "ready", session });
      setCallbackUrl("");
    } catch (unknownError) {
      setError(userFacingError(unknownError));
    } finally {
      statusRevisionRef.current += 1;
    }
  }

  async function handleComplete(): Promise<void> {
    if (!session || !callbackReceived) return;
    statusRevisionRef.current += 1;
    setError("");
    setFlow({ phase: "completing", session });
    try {
      await onComplete(session.loginId);
    } catch (unknownError) {
      setFlow({ phase: "ready", session });
      setError(userFacingError(unknownError));
    } finally {
      statusRevisionRef.current += 1;
    }
  }

  async function handleCancel(): Promise<void> {
    statusRevisionRef.current += 1;
    setError("");
    try {
      await cancelCodexOAuth(session?.loginId);
      setFlow({ phase: "idle" });
      setCallbackUrl("");
      setCopied(false);
    } catch (unknownError) {
      setError(userFacingError(unknownError));
    } finally {
      statusRevisionRef.current += 1;
    }
  }

  if (!session) {
    return (
      <div className="oauth-flow">
        <div className="oauth-security-note">
          <ShieldCheck aria-hidden="true" size={19} />
          <div>
            <strong>OpenAI 官方 OAuth</strong>
            <span>使用 PKCE 授权，凭据只写入本机私密文件。</span>
          </div>
        </div>
        <p className="field-hint oauth-account-hint">
          授权后会从 OpenAI 返回的账号信息中读取邮箱和账号 ID；需要添加另一个账号时，重新发起一次授权即可。
        </p>
        {error ? <p className="field-error" role="alert">{error}</p> : null}
        <div className="actions oauth-primary-action">
          <Button disabled={disabled || isWorking} onClick={handleStart}>
            {flow.phase === "starting"
              ? <LoaderCircle aria-hidden="true" className="spin-icon" size={15} />
              : <Globe2 aria-hidden="true" size={15} />}
            {flow.phase === "starting" ? "正在准备..." : "开始 OAuth 授权"}
          </Button>
        </div>
      </div>
    );
  }

  return (
    <div className="oauth-flow">
      <div className={callbackReceived ? "oauth-status oauth-status-ready" : "oauth-status"} aria-live="polite">
        {callbackReceived
          ? <CircleCheck aria-hidden="true" size={19} />
          : <LoaderCircle aria-hidden="true" className="spin-icon" size={19} />}
        <div>
          <strong>{callbackReceived ? "授权回调已收到" : "等待浏览器授权"}</strong>
          <span>{callbackReceived ? "可以保存这个账号。" : "授权会话将在 5 分钟后失效。"}</span>
        </div>
      </div>

      <Field label="授权链接">
        <div className="oauth-url-row">
          <input aria-label="OAuth 授权链接" readOnly value={session.authUrl} />
          <button
            aria-label="复制 OAuth 授权链接"
            className="icon-button"
            disabled={disabled || isWorking}
            onClick={handleCopyUrl}
            title={copied ? "已复制" : "复制授权链接"}
            type="button"
          >
            {copied ? <Check aria-hidden="true" size={16} /> : <Copy aria-hidden="true" size={16} />}
          </button>
        </div>
      </Field>

      <Button disabled={disabled || isWorking} onClick={handleOpenBrowser} variant="secondary">
        <ExternalLink aria-hidden="true" size={15} /> 在浏览器中打开
      </Button>

      {!session.callbackServerReady ? (
        <p className="field-error" role="alert">本地回调端口不可用，请使用下方手动回调。</p>
      ) : null}

      <div className="oauth-manual-callback">
        <Field label="手动输入回调地址">
          <div className="oauth-callback-row">
            <input
              aria-label="OAuth 回调地址"
              disabled={disabled || isWorking}
              onChange={(event) => setCallbackUrl(event.currentTarget.value)}
              placeholder={`${session.callbackUrl}?code=...&state=...`}
              value={callbackUrl}
            />
            <Button
              disabled={disabled || isWorking || !callbackUrl.trim()}
              onClick={handleSubmitCallback}
              variant="secondary"
            >
              <Check aria-hidden="true" size={15} /> 我已授权，继续
            </Button>
          </div>
        </Field>
      </div>

      {error ? <p className="field-error" role="alert">{error}</p> : null}

      <div className="actions oauth-actions">
        <Button disabled={disabled || isWorking || !callbackReceived} onClick={handleComplete}>
          {flow.phase === "completing"
            ? <LoaderCircle aria-hidden="true" className="spin-icon" size={15} />
            : <ShieldCheck aria-hidden="true" size={15} />}
          {flow.phase === "completing" ? "正在保存..." : "完成并添加账号"}
        </Button>
        <Button disabled={disabled || isWorking} onClick={handleCancel} variant="secondary">
          取消授权
        </Button>
      </div>
    </div>
  );
}

function ProviderAddTabs({
  apiKeyForm,
  disabled,
  fileInputRef,
  jsonImportSources,
  importProgress,
  importReport,
  importReviewError,
  importReviewing,
  loadJsonFiles,
  onImportCodexOAuth,
  onImportLocal,
  apiKeyError,
  pastedJson,
  addToCurrentGroup,
  activeGroupName,
  setApiKeyForm,
  setApiKeyError,
  setPastedJson,
  setAddToCurrentGroup,
  submitApiKey,
  submitJsonBatch,
}: {
  apiKeyForm: ApiKeyForm;
  disabled: boolean;
  fileInputRef: RefObject<HTMLInputElement | null>;
  jsonImportSources: JsonImportFile[];
  importProgress: ProviderImportProgress | null;
  importReport: ProviderImportBatchReport | null;
  importReviewError: string;
  importReviewing: boolean;
  loadJsonFiles: (fileList: FileList | null) => Promise<void>;
  onImportCodexOAuth: (loginId: string) => Promise<void>;
  onImportLocal: () => Promise<void>;
  apiKeyError: string;
  pastedJson: string;
  addToCurrentGroup: boolean;
  activeGroupName: string;
  setApiKeyForm: (form: ApiKeyForm) => void;
  setApiKeyError: (value: string) => void;
  setPastedJson: (value: string) => void;
  setAddToCurrentGroup: (value: boolean) => void;
  submitApiKey: (event: FormEvent) => Promise<void>;
  submitJsonBatch: (event: FormEvent) => Promise<void>;
}) {
  function updateApiKeyForm(form: ApiKeyForm) {
    setApiKeyError("");
    setApiKeyForm(form);
  }

  return (
    <Tabs.Root className="add-tabs" defaultValue="oauth">
      <Tabs.List className="add-tabs-list" aria-label="添加账号方式">
        <Tabs.Trigger className="add-tabs-trigger" value="oauth">
          <Globe2 aria-hidden="true" size={15} /> OAuth
        </Tabs.Trigger>
        <Tabs.Trigger className="add-tabs-trigger" value="api-key">
          <KeyRound size={15} /> API Key
        </Tabs.Trigger>
        <Tabs.Trigger className="add-tabs-trigger" value="json">
          <Upload size={15} /> Token / JSON
        </Tabs.Trigger>
      </Tabs.List>

      <Tabs.Content className="add-tabs-content" value="oauth">
        <CodexOAuthPanel disabled={disabled} onComplete={onImportCodexOAuth} />
      </Tabs.Content>

      <Tabs.Content className="add-tabs-content" value="api-key">
        <form onSubmit={submitApiKey}>
          <ApiKeyFormTabs
            disabled={disabled}
            form={apiKeyForm}
            isEditing={false}
            update={updateApiKeyForm}
          >
            <div className="form-grid">
              <Field label="供应商名称">
                <input value={apiKeyForm.providerName} onChange={(event) => updateApiKeyForm({ ...apiKeyForm, providerName: event.target.value })} placeholder="OpenRouter" required />
              </Field>
              <Field label="类型">
                <Select.Root value={apiKeyForm.kind} onValueChange={(kind) => updateApiKeyForm({ ...apiKeyForm, kind: kind as ApiKeyKind })}>
                  <Select.Trigger className="select-trigger">
                    <Select.Value />
                  </Select.Trigger>
                  <Select.Portal>
                    <Select.Content className="select-content" position="popper" sideOffset={4}>
                      {(["openai_compatible", "relay_provider"] as ApiKeyKind[]).map((kind) => (
                        <Select.Item className="select-item" key={kind} value={kind}>
                          <Select.ItemText>{providerKindLabel(kind)}</Select.ItemText>
                        </Select.Item>
                      ))}
                    </Select.Content>
                  </Select.Portal>
                </Select.Root>
              </Field>
            </div>
            <Field label="请求地址">
              <input
                value={apiKeyForm.baseUrl}
                onChange={(event) => updateApiKeyForm({ ...apiKeyForm, baseUrl: event.target.value })}
                placeholder="https://api.example.com/v1 或 https://api.example.com/v1/responses"
                required
              />
            </Field>
            <Field label="Responses WebSocket（可选）">
              <input
                value={apiKeyForm.websocketUrl}
                onChange={(event) => updateApiKeyForm({ ...apiKeyForm, websocketUrl: event.target.value })}
                placeholder="wss://api.example.com/v1/responses"
              />
            </Field>
            <Field label="API Key（可选）">
              <input value={apiKeyForm.apiKey} onChange={(event) => updateApiKeyForm({ ...apiKeyForm, apiKey: event.target.value })} placeholder="sk-..." type="password" />
            </Field>
            <Field label="API Key 环境变量名（可选）">
              <input value={apiKeyForm.envVar} onChange={(event) => updateApiKeyForm({ ...apiKeyForm, envVar: event.target.value })} placeholder="OPENROUTER_API_KEY" />
            </Field>
            <Field label="状态刷新间隔（秒）">
              <input min={15} type="number" value={apiKeyForm.refreshIntervalSeconds} onChange={(event) => updateApiKeyForm({ ...apiKeyForm, refreshIntervalSeconds: Number(event.target.value) })} />
            </Field>
          </ApiKeyFormTabs>
          {apiKeyError ? <p className="field-error">{apiKeyError}</p> : null}
          <p className="field-hint">
            创建时只保存账号材料，不会自动加入分组；请求地址不决定直连或代理，启动方式以账号卡片上的选择为准。直连需要重启 ChatGPT / Codex 以读取账号/API Key，本地代理切换账号无需重启。
          </p>
          <div className="actions">
            <Button disabled={disabled} type="submit">
              <KeyRound size={15} /> 添加账号
            </Button>
          </div>
        </form>
      </Tabs.Content>

      <Tabs.Content className="add-tabs-content" value="json">
        <form onSubmit={submitJsonBatch}>
          <p className="json-import-note">粘贴 Token / JSON，或选择多个 JSON 文件批量导入。</p>
          <Field label="Token / JSON">
            <textarea
              className="json-import-textarea"
              onChange={(event) => setPastedJson(event.target.value)}
              placeholder="粘贴 session JSON、auth.json、Sub2API / New API 连接 JSON、accessToken 或 refresh_token"
              value={pastedJson}
            />
          </Field>
          <button className="button button-default import-submit" disabled={disabled || importReviewing || jsonImportSources.length === 0} type="submit">
            <Upload size={15} /> {importReviewing ? "正在检查..." : "预览导入"}
          </button>
          {importReviewError ? <p className="field-error" role="alert">{importReviewError}</p> : null}
          <label className="check-row import-group-option">
            <input
              checked={addToCurrentGroup}
              disabled={disabled}
              onChange={(event) => setAddToCurrentGroup(event.currentTarget.checked)}
              type="checkbox"
            />
            <span>导入成功后加入当前分组“{activeGroupName}”</span>
          </label>
          {importProgress?.active ? (
            <div className="provider-refresh-progress" aria-live="polite">
              <progress max={Math.max(1, importProgress.total)} value={importProgress.completed} />
              <span>
                {importProgress.completed}/{importProgress.total}
                {importProgress.currentLabel ? ` · ${importProgress.currentLabel}` : ""}
              </span>
            </div>
          ) : null}
          {importReport ? (
            <div className={importReport.failed.length ? "warning-box import-report" : "success-box import-report"}>
              <strong>{importReport.succeeded.length} 成功 · {importReport.failed.length} 失败</strong>
              {importReport.failed.map((failure) => (
                <p key={`${failure.index}-${failure.label}`}>{failure.label}：{failure.message}</p>
              ))}
            </div>
          ) : null}
          <div className="file-import-panel">
            <span>文件批量导入</span>
            <input
              accept=".json,application/json"
              className="hidden-file-input"
              multiple
              onChange={(event) => void loadJsonFiles(event.currentTarget.files)}
              ref={fileInputRef}
              type="file"
            />
            <button className="button button-secondary file-trigger" onClick={() => fileInputRef.current?.click()} type="button">
              <FileJson size={15} /> 选择 JSON 文件
            </button>
            <div className="selected-files">
              {jsonImportSources.length === 0 ? (
                <span className="file-empty">未选择文件，也未粘贴 JSON</span>
              ) : (
                jsonImportSources.map((file, index) => <span className="file-pill" key={`${file.name}-${index}`}>{file.name}</span>)
              )}
            </div>
          </div>
          <div className="actions">
            <Button disabled={disabled} onClick={() => void onImportLocal()} type="button" variant="secondary">
              <FolderInput size={15} /> 导入本机 Codex 账号
            </Button>
          </div>
        </form>
      </Tabs.Content>
    </Tabs.Root>
  );
}
