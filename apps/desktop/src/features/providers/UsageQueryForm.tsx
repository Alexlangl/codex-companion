import * as Tabs from "@radix-ui/react-tabs";
import { LoaderCircle, Play, RotateCcw, WalletCards } from "lucide-react";
import { useRef, useState } from "react";
import { Button, Field } from "../../components/ui";
import { testUsageQuery } from "../../lib/api";
import { userFacingError } from "../../lib/errors";
import type { ProviderAccountInfo, ProviderUsageQueryTemplate } from "../../types/domain";
import type { ApiKeyForm } from "./provider-types";

const USAGE_QUERY_TEMPLATES = [
  { value: "general", label: "通用" },
  { value: "new_api", label: "NewAPI" },
  { value: "open_router", label: "OpenRouter" },
  { value: "custom", label: "自定义" },
] as const satisfies ReadonlyArray<{
  value: ProviderUsageQueryTemplate;
  label: string;
}>;

const USAGE_QUERY_PRESETS: Record<ProviderUsageQueryTemplate, string> = {
  general: `({
  request: {
    url: "{{baseUrl}}/user/balance",
    method: "GET",
    headers: {
      "Authorization": "Bearer {{apiKey}}",
      "User-Agent": "codex-companion/1.0"
    }
  },
  extractor: function (response) {
    return {
      isValid: response.is_active !== false,
      remaining: response.balance,
      unit: response.unit || "USD"
    };
  }
})`,
  new_api: `({
  request: {
    url: "{{baseUrl}}/api/user/self",
    method: "GET",
    headers: {
      "Content-Type": "application/json",
      "Authorization": "Bearer {{accessToken}}",
      "User-Agent": "codex-companion/1.0",
      "New-Api-User": "{{userId}}"
    }
  },
  extractor: function (response) {
    if (response.success && response.data) {
      return {
        planName: response.data.group || "默认套餐",
        remaining: response.data.quota / 500000,
        used: response.data.used_quota / 500000,
        total: (response.data.quota + response.data.used_quota) / 500000,
        unit: "USD"
      };
    }
    return {
      isValid: false,
      invalidMessage: response.message || "查询失败"
    };
  }
})`,
  open_router: `({
  request: {
    url: "{{baseUrl}}/api/v1/credits",
    method: "GET",
    headers: {
      "Authorization": "Bearer {{apiKey}}",
      "User-Agent": "codex-companion/1.0"
    }
  },
  extractor: function (response) {
    var data = response.data || response;
    var total = data.total_credits;
    var used = data.total_usage || 0;
    return {
      remaining: total == null ? null : Math.max(total - used, 0),
      used: used,
      total: total,
      unit: "USD",
      planName: "OpenRouter"
    };
  }
})`,
  custom: `({
  request: {
    url: "",
    method: "GET",
    headers: {}
  },
  extractor: function (response) {
    return {
      remaining: response.remaining,
      used: response.used,
      total: response.total,
      unit: response.unit || "USD"
    };
  }
})`,
};

type UsageQueryTestState =
  | { status: "idle" }
  | { status: "testing" }
  | { status: "success"; summary: string }
  | { status: "error"; message: string };

type UsageQueryFormProps = {
  disabled: boolean;
  form: ApiKeyForm;
  isEditing: boolean;
  providerId?: string;
  update: (form: ApiKeyForm) => void;
};

export function usageQueryPresetScript(template: ProviderUsageQueryTemplate): string {
  return USAGE_QUERY_PRESETS[template];
}

function isUsageQueryTemplate(value: string): value is ProviderUsageQueryTemplate {
  return USAGE_QUERY_TEMPLATES.some((template) => template.value === value);
}

export function UsageQueryForm({
  disabled,
  form,
  isEditing,
  providerId,
  update,
}: UsageQueryFormProps) {
  const [testState, setTestState] = useState<UsageQueryTestState>({ status: "idle" });
  const testRequestRef = useRef(0);
  const queryDisabled = disabled || !form.usageQueryEnabled;
  const queryBaseUrlPlaceholder = form.baseUrl || "https://api.example.com";

  function updateQuery(patch: Partial<ApiKeyForm>): void {
    testRequestRef.current += 1;
    setTestState({ status: "idle" });
    update({ ...form, ...patch });
  }

  function handleEnabledChange(): void {
    const enabled = !form.usageQueryEnabled;
    const script = form.usageQueryScript.trim()
      ? form.usageQueryScript
      : usageQueryPresetScript(form.usageQueryTemplate);
    updateQuery({ usageQueryEnabled: enabled, usageQueryScript: script });
  }

  function handleTemplateChange(value: string): void {
    if (!isUsageQueryTemplate(value)) return;
    updateQuery({
      usageQueryTemplate: value,
      usageQueryScript: usageQueryPresetScript(value),
    });
  }

  function handleResetScript(): void {
    updateQuery({ usageQueryScript: usageQueryPresetScript(form.usageQueryTemplate) });
  }

  async function handleTest(): Promise<void> {
    if (!form.usageQueryScript.trim()) {
      setTestState({ status: "error", message: "查询脚本不能为空。" });
      return;
    }
    if (
      !isEditing &&
      form.usageQueryTemplate === "new_api" &&
      (!form.usageQueryAccessToken.trim() || !form.usageQueryUserId.trim())
    ) {
      setTestState({ status: "error", message: "NewAPI 查询需要个人访问令牌和用户 ID。" });
      return;
    }

    const requestId = testRequestRef.current + 1;
    testRequestRef.current = requestId;
    setTestState({ status: "testing" });
    try {
      const account = await testUsageQuery({
        providerId: providerId ?? null,
        providerBaseUrl: form.baseUrl.trim(),
        providerApiKey: form.apiKey.trim() || null,
        usageQuery: {
          enabled: true,
          template: form.usageQueryTemplate,
          baseUrl: form.usageQueryBaseUrl.trim() || null,
          script: form.usageQueryScript,
          timeoutSeconds: form.usageQueryTimeoutSeconds,
          apiKey: form.usageQueryApiKey.trim() || null,
          accessToken: form.usageQueryAccessToken.trim() || null,
          userId: form.usageQueryUserId.trim() || null,
        },
      });
      if (testRequestRef.current === requestId) {
        setTestState({ status: "success", summary: usageQueryResultSummary(account) });
      }
    } catch (unknownError) {
      if (testRequestRef.current === requestId) {
        setTestState({ status: "error", message: userFacingError(unknownError) });
      }
    }
  }

  const credentials = usageQueryCredentials({
    form,
    isEditing,
    queryBaseUrlPlaceholder,
    updateQuery,
  });

  return (
    <section className="usage-query-panel" aria-labelledby="usage-query-heading">
      <div className="usage-query-header">
        <div>
          <h3 id="usage-query-heading">
            <WalletCards aria-hidden="true" size={16} /> 余额查询
          </h3>
          <p>默认关闭；开启后按这里的独立接口刷新账号额度。</p>
        </div>
        <label className="toggle-row usage-query-toggle">
          <input
            checked={form.usageQueryEnabled}
            disabled={disabled}
            onChange={handleEnabledChange}
            type="checkbox"
          />
          <span>{form.usageQueryEnabled ? "已启用" : "未启用"}</span>
        </label>
      </div>

      <fieldset className="usage-query-config" disabled={queryDisabled}>
        <legend className="sr-only">余额查询配置</legend>
        <Tabs.Root onValueChange={handleTemplateChange} value={form.usageQueryTemplate}>
          <Tabs.List className="usage-template-list" aria-label="余额查询模板">
            {USAGE_QUERY_TEMPLATES.map((template) => (
              <Tabs.Trigger
                className="usage-template-trigger"
                key={template.value}
                value={template.value}
              >
                {template.label}
              </Tabs.Trigger>
            ))}
          </Tabs.List>
          <Tabs.Content className="usage-template-content" value={form.usageQueryTemplate}>
            {credentials}
          </Tabs.Content>
        </Tabs.Root>

        <div className="usage-query-code-header">
          <div>
            <strong>请求与提取脚本</strong>
            <span>可使用 {"{{baseUrl}}"}、{"{{apiKey}}"}、{"{{accessToken}}"}、{"{{userId}}"}</span>
          </div>
          <Button onClick={handleResetScript} variant="secondary">
            <RotateCcw aria-hidden="true" size={14} /> 恢复预置
          </Button>
        </div>
        <Field label="JavaScript">
          <textarea
            className="usage-query-code"
            onChange={(event) => updateQuery({ usageQueryScript: event.currentTarget.value })}
            spellCheck={false}
            value={form.usageQueryScript}
          />
        </Field>

        <div className="usage-query-actions">
          <Field label="请求超时（秒）">
            <input
              max={30}
              min={2}
              onChange={(event) =>
                updateQuery({ usageQueryTimeoutSeconds: Number(event.currentTarget.value) })
              }
              type="number"
              value={form.usageQueryTimeoutSeconds}
            />
          </Field>
          <Button
            disabled={testState.status === "testing"}
            onClick={handleTest}
            variant="secondary"
          >
            {testState.status === "testing" ? (
              <LoaderCircle aria-hidden="true" className="spin-icon" size={14} />
            ) : (
              <Play aria-hidden="true" size={14} />
            )}
            {testState.status === "testing" ? "正在测试" : "测试查询"}
          </Button>
        </div>
      </fieldset>

      {testState.status === "success" ? (
        <p className="usage-query-test-result usage-query-test-success" role="status">
          查询成功 · {testState.summary}
        </p>
      ) : null}
      {testState.status === "error" ? (
        <p className="usage-query-test-result field-error" role="alert">
          {testState.message}
        </p>
      ) : null}
    </section>
  );
}

function usageQueryCredentials({
  form,
  isEditing,
  queryBaseUrlPlaceholder,
  updateQuery,
}: {
  form: ApiKeyForm;
  isEditing: boolean;
  queryBaseUrlPlaceholder: string;
  updateQuery: (patch: Partial<ApiKeyForm>) => void;
}) {
  const apiKeyLabel = isEditing ? "查询 API Key（留空保留）" : "查询 API Key（可选）";
  const usesApiKey = form.usageQueryTemplate !== "new_api";
  const usesTokenIdentity = form.usageQueryTemplate === "new_api" || form.usageQueryTemplate === "custom";

  return (
    <div className="usage-query-credentials">
      <Field label="查询地址（可选）">
        <input
          onChange={(event) => updateQuery({ usageQueryBaseUrl: event.currentTarget.value })}
          placeholder={queryBaseUrlPlaceholder}
          value={form.usageQueryBaseUrl}
        />
      </Field>
      {usesApiKey ? (
        <Field label={apiKeyLabel}>
          <input
            autoComplete="off"
            onChange={(event) => updateQuery({ usageQueryApiKey: event.currentTarget.value })}
            placeholder="留空时使用 Provider API Key"
            type="password"
            value={form.usageQueryApiKey}
          />
        </Field>
      ) : null}
      {usesTokenIdentity ? (
        <>
          <Field label={isEditing ? "个人访问令牌（留空保留）" : "个人访问令牌"}>
            <input
              autoComplete="off"
              onChange={(event) => updateQuery({ usageQueryAccessToken: event.currentTarget.value })}
              placeholder={form.usageQueryTemplate === "new_api" ? "从个人安全设置获取" : "可选"}
              type="password"
              value={form.usageQueryAccessToken}
            />
          </Field>
          <Field label={isEditing ? "用户 ID（留空保留）" : "用户 ID"}>
            <input
              autoComplete="off"
              onChange={(event) => updateQuery({ usageQueryUserId: event.currentTarget.value })}
              placeholder={form.usageQueryTemplate === "new_api" ? "例如 1" : "可选"}
              value={form.usageQueryUserId}
            />
          </Field>
        </>
      ) : null}
    </div>
  );
}

function usageQueryResultSummary(account: ProviderAccountInfo): string {
  const unit = account.quotaLabel?.replace(/\s*余额$/, "") || "额度";
  const summary = [account.subscriptionType?.trim()].filter(Boolean) as string[];
  if (typeof account.usageAvailable === "number") {
    summary.push(`剩余 ${formatUsageAmount(account.usageAvailable)} ${unit}`);
  }
  if (typeof account.usageUsed === "number") {
    summary.push(`已用 ${formatUsageAmount(account.usageUsed)} ${unit}`);
  }
  if (typeof account.usageTotal === "number") {
    summary.push(`总额 ${formatUsageAmount(account.usageTotal)} ${unit}`);
  }
  return summary.join(" · ") || "接口已返回有效额度";
}

function formatUsageAmount(value: number): string {
  return new Intl.NumberFormat("zh-CN", { maximumFractionDigits: 2 }).format(value);
}
