import { Field } from "../../components/ui";
import { providerAccountTitle } from "../../lib/provider-display";
import type { AccountPolicy, AccountProtection, ProviderConfig } from "../../types/domain";

export const DEFAULT_ACCOUNT_PROTECTION: AccountProtection = {
  maxAccountConcurrency: 0,
  accountConcurrencyWaitMs: 0,
  excludedModels: [],
  providers: {},
};

export function AccountProtectionFields({ value, providers, onChange }: {
  value: AccountProtection;
  providers: Record<string, ProviderConfig>;
  onChange: (value: AccountProtection) => void;
}) {
  function updateAccount(id: string, policy: AccountPolicy): void {
    onChange({ ...value, providers: { ...value.providers, [id]: policy } });
  }

  return (
    <fieldset className="configuration-fields">
      <legend>账号保护</legend>
      <p className="field-hint">限制单账号并发、保留额度并排除指定模型。规则只影响后续请求。</p>
      <div className="form-grid">
        <Field label="每个账号最大并发（0 = 不限制）">
          <input type="number" required min={0} max={256} step={1} value={numberInputValue(value.maxAccountConcurrency)}
            onChange={(event) => onChange({ ...value, maxAccountConcurrency: event.target.valueAsNumber })} />
        </Field>
        <Field label="并发满时等待（毫秒，0 = 不等待）">
          <input type="number" required min={0} max={120000} step={1} value={numberInputValue(value.accountConcurrencyWaitMs)}
            onChange={(event) => onChange({ ...value, accountConcurrencyWaitMs: event.target.valueAsNumber })} />
        </Field>
      </div>
      <ModelRules label="全局排除模型" value={value.excludedModels}
        onChange={(excludedModels) => onChange({ ...value, excludedModels })} />
      <p className="field-hint">每行一个模型规则，支持 * 通配符，例如 gpt-5.4*。全局规则和账号规则同时生效。</p>
      <details>
        <summary>按账号设置额度与模型规则</summary>
        <div className="configuration-stack">
          {Object.entries(providers).map(([id, provider]) => {
            const policy = value.providers[id] ?? { excludedModels: [] };
            const reserve = policy.quotaReserve;
            return (
              <fieldset className="configuration-fields" key={id}>
                <legend>{providerAccountTitle(provider)}</legend>
                <ModelRules label="此账号排除模型" value={policy.excludedModels}
                  onChange={(excludedModels) => updateAccount(id, { ...policy, excludedModels })} />
                {provider.kind === "official_codex" ? (
                  <>
                    <label className="toggle-row">
                      <input type="checkbox" checked={Boolean(reserve)} onChange={(event) => updateAccount(id, {
                        ...policy,
                        quotaReserve: event.target.checked ? { hourlyThresholdPercent: 10, weeklyThresholdPercent: 10 } : null,
                      })} />
                      <span>启用额度保留</span>
                    </label>
                    {reserve ? (
                      <div className="form-grid">
                        <Field label="5 小时剩余额度阈值（%）">
                          <input type="number" required min={1} max={100} step={1} value={numberInputValue(reserve.hourlyThresholdPercent)}
                            onChange={(event) => updateAccount(id, { ...policy, quotaReserve: { ...reserve, hourlyThresholdPercent: event.target.valueAsNumber } })} />
                        </Field>
                        <Field label="每周剩余额度阈值（%）">
                          <input type="number" required min={1} max={100} step={1} value={numberInputValue(reserve.weeklyThresholdPercent)}
                            onChange={(event) => updateAccount(id, { ...policy, quotaReserve: { ...reserve, weeklyThresholdPercent: event.target.valueAsNumber } })} />
                        </Field>
                      </div>
                    ) : null}
                    <p className="field-hint">剩余额度小于等于阈值时暂停分配；启用后额度未知或过期也会暂停分配，等待刷新。</p>
                  </>
                ) : null}
              </fieldset>
            );
          })}
        </div>
      </details>
      <details>
        <summary>高级：Codex 客户端版本</summary>
        <Field label="版本覆盖（留空使用自动检测）">
          <input maxLength={64} pattern="[a-zA-Z0-9.+\\-]*" value={value.codexClientVersion ?? ""}
            onChange={(event) => onChange({ ...value, codexClientVersion: event.target.value || null })} />
        </Field>
      </details>
    </fieldset>
  );
}

function ModelRules({ label, value, onChange }: { label: string; value: string[]; onChange: (rules: string[]) => void }) {
  // Keep empty lines while editing so Enter can start the next rule.
  return (
    <Field label={label}>
      <textarea rows={3} value={value.join("\n")} onChange={(event) => onChange(event.target.value.split("\n"))} />
    </Field>
  );
}

function numberInputValue(value: number): number | "" {
  return Number.isFinite(value) ? value : "";
}
