import * as Dialog from "@radix-ui/react-dialog";
import * as Select from "@radix-ui/react-select";
import * as Tabs from "@radix-ui/react-tabs";
import {
  Check,
  Copy,
  Download,
  Eye,
  EyeOff,
  FileJson,
  FolderInput,
  KeyRound,
  LayoutGrid,
  List,
  Plus,
  RefreshCw,
  Upload,
  X,
} from "lucide-react";
import { useEffect, useMemo, useRef, useState, type FormEvent, type RefObject } from "react";
import { Button, Field, Panel } from "../../components/ui";
import { getProviderImportProgress, getProviderRefreshProgress } from "../../lib/api";
import { providerKindLabel } from "../../lib/format";
import { providerAccountTitle, providerUsesAgentIdentity } from "../../lib/provider-display";
import type {
  ApiKeyProviderUpdate,
  BusyState,
  CompanionStatus,
  ProviderConfig,
  ProviderExportFormat,
  ProviderExportOutput,
  ProviderLaunchMode,
  ProviderImportBatchReport,
  ProviderImportProgress,
  ProviderRefreshProgress,
  ProviderViewMode,
} from "../../types/domain";
import { ProviderCard, ProviderCompactItem } from "./ProviderCards";
import { emptyApiKeyForm, type ApiKeyForm, type ApiKeyKind, type JsonImportFile } from "./provider-types";

export function Providers({
  busy,
  status,
  onImportApiKey,
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
  status: CompanionStatus;
  launchModes: Record<string, ProviderLaunchMode>;
  onImportApiKey: (input: ApiKeyForm) => Promise<void>;
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
  const [addToCurrentGroup, setAddToCurrentGroup] = useState(false);
  const fileInputRef = useRef<HTMLInputElement>(null);
  const exportRequestRef = useRef(0);
  const disabled = busy !== "idle";
  const providers = Object.values(status.config.providers);
  const exportFormats = exportProvider ? exportFormatOptionsForProvider(exportProvider) : [];
  const maskedExportJson = useMemo(() => (exportOutput ? maskJsonPreviewContent(exportOutput.jsonContent) : ""), [exportOutput]);
  const exportPreviewText = exportOutput ? (exportHidden ? maskedExportJson : exportOutput.jsonContent) : "";
  const jsonImportSources = [
    ...jsonFiles,
    ...(pastedJson.trim() ? [{ name: "粘贴的 JSON", text: pastedJson.trim() }] : []),
  ];

  useEffect(() => {
    if (busy !== "testing") {
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
  }, [busy]);

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
    };
    if (!input.apiKey && !input.envVar) {
      setApiKeyError("至少填写 API Key 或 API Key 环境变量名。直连的密钥写入方式受“保留官方 Codex 登录”设置控制；本地代理由 Companion 注入密钥且不写 auth.json。");
      return;
    }
    await onImportApiKey(input);
    setApiKeyForm(emptyApiKeyForm);
    setApiKeyError("");
    setAddOpen(false);
  }

  async function submitJsonBatch(event: FormEvent) {
    event.preventDefault();
    const report = await onImportJsonBatch(
      jsonImportSources,
      addToCurrentGroup ? status.config.relay.activeGroupId : null,
    );
    setImportReport(report);
    if (report.failed.length === 0) {
      setJsonFiles([]);
      setPastedJson("");
      if (fileInputRef.current) {
        fileInputRef.current.value = "";
      }
      setAddOpen(false);
    }
  }

  async function importLocalAccount() {
    await onImportLocal();
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
    };
    if (!input.providerDisplayName || !input.providerName || !input.baseUrl) {
      setEditError("Provider Name、供应商名称和请求地址不能为空。");
      return;
    }
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
    });
    setEditProvider(null);
    setEditError("");
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
        setExportError(String(unknownError));
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
            <Button disabled={disabled || providers.length === 0} onClick={() => void onRefreshAll()} variant="secondary">
              <RefreshCw size={15} /> 刷新全部
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
        setAddOpen(open);
        if (!open) setImportReport(null);
      }}>
        <Dialog.Portal>
          <Dialog.Overlay className="dialog-overlay" />
          <Dialog.Content className="dialog-content add-provider-dialog">
            <div className="dialog-header">
              <div>
                <Dialog.Title className="dialog-title">添加账号</Dialog.Title>
                <Dialog.Description className="dialog-description">
                  选择 API Key、Token / JSON，或导入本机已有 Codex 账号。
                </Dialog.Description>
              </div>
              <Dialog.Close className="icon-button" aria-label="关闭">
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
              loadJsonFiles={loadJsonFiles}
              onImportLocal={importLocalAccount}
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

      <Dialog.Root open={Boolean(editProvider)} onOpenChange={(open) => !open && setEditProvider(null)}>
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
              <Dialog.Close className="icon-button" aria-label="关闭">
                <X size={16} />
              </Dialog.Close>
            </div>
            <ApiKeyEditForm
              disabled={disabled}
              editError={editError}
              form={editForm}
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

function ApiKeyEditForm({
  disabled,
  editError,
  form,
  setEditError,
  setForm,
  submit,
}: {
  disabled: boolean;
  editError: string;
  form: ApiKeyForm;
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
  return {
    providerDisplayName: provider.account?.email || providerAccountTitle(provider),
    providerName: provider.name,
    kind: isApiKeyProvider(provider) ? provider.kind : "openai_compatible",
    baseUrl: provider.baseUrl,
    websocketUrl: provider.websocketUrl ?? "",
    apiKey: "",
    envVar: authRef.startsWith("env:") ? authRef.slice("env:".length) : "",
    refreshIntervalSeconds: provider.refreshIntervalSeconds || 60,
  };
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

function ProviderAddTabs({
  apiKeyForm,
  disabled,
  fileInputRef,
  jsonImportSources,
  importProgress,
  importReport,
  loadJsonFiles,
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
  loadJsonFiles: (fileList: FileList | null) => Promise<void>;
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
    <Tabs.Root className="add-tabs" defaultValue="api-key">
      <Tabs.List className="add-tabs-list" aria-label="添加账号方式">
        <Tabs.Trigger className="add-tabs-trigger" value="api-key">
          <KeyRound size={15} /> API Key
        </Tabs.Trigger>
        <Tabs.Trigger className="add-tabs-trigger" value="json">
          <Upload size={15} /> Token / JSON
        </Tabs.Trigger>
      </Tabs.List>

      <Tabs.Content className="add-tabs-content" value="api-key">
        <form onSubmit={submitApiKey}>
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
              placeholder="粘贴 session JSON、auth.json、Sub2API JSON、accessToken 或 refresh_token"
              value={pastedJson}
            />
          </Field>
          <button className="button button-default import-submit" disabled={disabled || jsonImportSources.length === 0} type="submit">
            <Upload size={15} /> 导入
          </button>
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
