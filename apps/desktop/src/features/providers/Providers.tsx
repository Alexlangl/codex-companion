import * as Dialog from "@radix-ui/react-dialog";
import * as Select from "@radix-ui/react-select";
import * as Tabs from "@radix-ui/react-tabs";
import {
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
import { useRef, useState, type FormEvent, type RefObject } from "react";
import { Button, Field, Panel } from "../../components/ui";
import { providerKindLabel } from "../../lib/format";
import type { BusyState, CompanionStatus, ProviderLaunchMode, ProviderViewMode } from "../../types/domain";
import { ProviderCard, ProviderCompactItem } from "./ProviderCards";
import { emptyApiKeyForm, type ApiKeyForm, type ApiKeyKind, type JsonImportFile } from "./provider-types";

export function Providers({
  busy,
  status,
  onImportApiKey,
  onImportJsonBatch,
  onImportLocal,
  onLaunch,
  onLaunchModeChange,
  onRemove,
  onRefresh,
  onRefreshAll,
  onViewModeChange,
  launchModes,
  viewMode,
}: {
  busy: BusyState;
  status: CompanionStatus;
  launchModes: Record<string, ProviderLaunchMode>;
  onImportApiKey: (input: ApiKeyForm) => Promise<void>;
  onImportJsonBatch: (jsonFiles: JsonImportFile[]) => Promise<void>;
  onImportLocal: () => Promise<void>;
  onLaunch: (id: string, mode?: ProviderLaunchMode) => Promise<void>;
  onLaunchModeChange: (providerId: string, mode: ProviderLaunchMode) => Promise<void>;
  onRemove: (id: string) => Promise<void>;
  onRefresh: (id: string) => Promise<void>;
  onRefreshAll: () => Promise<void>;
  onViewModeChange: (mode: ProviderViewMode) => Promise<void>;
  viewMode: ProviderViewMode;
}) {
  const [apiKeyForm, setApiKeyForm] = useState<ApiKeyForm>(emptyApiKeyForm);
  const [jsonFiles, setJsonFiles] = useState<JsonImportFile[]>([]);
  const [pastedJson, setPastedJson] = useState("");
  const [addOpen, setAddOpen] = useState(false);
  const [apiKeyError, setApiKeyError] = useState("");
  const fileInputRef = useRef<HTMLInputElement>(null);
  const disabled = busy !== "idle";
  const providers = Object.values(status.config.providers);
  const jsonImportSources = [
    ...jsonFiles,
    ...(pastedJson.trim() ? [{ name: "粘贴的 JSON", text: pastedJson.trim() }] : []),
  ];

  async function submitApiKey(event: FormEvent) {
    event.preventDefault();
    const input = {
      ...apiKeyForm,
      providerName: apiKeyForm.providerName.trim(),
      baseUrl: apiKeyForm.baseUrl.trim(),
      apiKey: apiKeyForm.apiKey.trim(),
      envVar: apiKeyForm.envVar.trim(),
      refreshIntervalSeconds: Number(apiKeyForm.refreshIntervalSeconds) || 60,
    };
    if (!input.apiKey && !input.envVar) {
      setApiKeyError("至少填写 API Key 或 API Key 环境变量名。直连中转站需要环境变量名；本地代理可以使用 API Key。");
      return;
    }
    await onImportApiKey(input);
    setApiKeyForm(emptyApiKeyForm);
    setApiKeyError("");
    setAddOpen(false);
  }

  async function submitJsonBatch(event: FormEvent) {
    event.preventDefault();
    await onImportJsonBatch(jsonImportSources);
    setJsonFiles([]);
    setPastedJson("");
    if (fileInputRef.current) {
      fileInputRef.current.value = "";
    }
    setAddOpen(false);
  }

  async function importLocalAccount() {
    await onImportLocal();
    setAddOpen(false);
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
                onRefresh={onRefresh}
                onRemove={onRemove}
              />
            ))}
          </div>
        )}
      </Panel>

      <Dialog.Root open={addOpen} onOpenChange={setAddOpen}>
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
              loadJsonFiles={loadJsonFiles}
              onImportLocal={importLocalAccount}
              apiKeyError={apiKeyError}
              pastedJson={pastedJson}
              setApiKeyForm={setApiKeyForm}
              setApiKeyError={setApiKeyError}
              setPastedJson={setPastedJson}
              submitApiKey={submitApiKey}
              submitJsonBatch={submitJsonBatch}
            />
          </Dialog.Content>
        </Dialog.Portal>
      </Dialog.Root>
    </div>
  );
}

function ProviderAddTabs({
  apiKeyForm,
  disabled,
  fileInputRef,
  jsonImportSources,
  loadJsonFiles,
  onImportLocal,
  apiKeyError,
  pastedJson,
  setApiKeyForm,
  setApiKeyError,
  setPastedJson,
  submitApiKey,
  submitJsonBatch,
}: {
  apiKeyForm: ApiKeyForm;
  disabled: boolean;
  fileInputRef: RefObject<HTMLInputElement | null>;
  jsonImportSources: JsonImportFile[];
  loadJsonFiles: (fileList: FileList | null) => Promise<void>;
  onImportLocal: () => Promise<void>;
  apiKeyError: string;
  pastedJson: string;
  setApiKeyForm: (form: ApiKeyForm) => void;
  setApiKeyError: (value: string) => void;
  setPastedJson: (value: string) => void;
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
            <Field label="显示名称">
              <input value={apiKeyForm.providerName} onChange={(event) => updateApiKeyForm({ ...apiKeyForm, providerName: event.target.value })} placeholder="OpenRouter" required />
            </Field>
            <Field label="类型">
              <Select.Root value={apiKeyForm.kind} onValueChange={(kind) => updateApiKeyForm({ ...apiKeyForm, kind: kind as ApiKeyKind })}>
                <Select.Trigger className="select-trigger">
                  <Select.Value />
                </Select.Trigger>
                <Select.Portal>
                  <Select.Content className="select-content">
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
          <Field label="Base URL">
            <input value={apiKeyForm.baseUrl} onChange={(event) => updateApiKeyForm({ ...apiKeyForm, baseUrl: event.target.value })} placeholder="https://api.example.com/v1" required />
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
            创建时只保存账号材料；启动时在账号卡片上选择直连中转站或本地代理。直连由 Codex 读取环境变量，本地代理由 Companion 注入 API Key。
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
