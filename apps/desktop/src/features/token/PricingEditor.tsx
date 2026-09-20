import { useCallback, useEffect, useRef, useState } from "react";
import { Badge, Button, Field, Panel } from "../../components/ui";
import { getPricingSettings, pricingPreview, savePricingSettings } from "../../lib/pricing-api";
import { userFacingError } from "../../lib/errors";
import { providerAccountTitle } from "../../lib/provider-display";
import type { ModelPriceSettings, PricingSettings, PricingSettingsSnapshot, ProviderConfig } from "../../types/domain";

const PRICE_FIELDS = [
  ["inputPerMillion", "新输入"],
  ["cachedInputPerMillion", "缓存输入"],
  ["cacheWriteInputPerMillion", "缓存写入"],
  ["outputPerMillion", "输出"],
] as const;

type PriceDraft = ModelPriceSettings & { originalModel: string | null; aliasesText: string };

export function PricingEditor({ active, providers, onSaved }: {
  active: boolean;
  providers: Record<string, ProviderConfig>;
  onSaved: () => Promise<void>;
}) {
  const [snapshot, setSnapshot] = useState<PricingSettingsSnapshot | null>(null);
  const [draft, setDraft] = useState<PriceDraft | null>(null);
  const [multipliers, setMultipliers] = useState<Record<string, string>>({});
  const [error, setError] = useState<string | null>(null);
  const [message, setMessage] = useState("");
  const [busy, setBusy] = useState(false);
  const addButtonRef = useRef<HTMLDivElement>(null);

  const reload = useCallback(async (): Promise<void> => {
    setBusy(true);
    setError(null);
    try {
      const result = await getPricingSettings();
      setSnapshot(result);
      setMultipliers(result.overrides.providerMultipliers);
    } catch (unknownError) {
      setError(userFacingError(unknownError));
    } finally {
      setBusy(false);
    }
  }, []);

  useEffect(() => {
    if (active && !snapshot) void reload();
  }, [active, snapshot, reload]);

  function closeEditor(): void {
    setDraft(null);
    addButtonRef.current?.querySelector("button")?.focus();
  }

  async function persist(input: PricingSettings, success: string): Promise<void> {
    setBusy(true);
    setError(null);
    setMessage("");
    try {
      const result = await savePricingSettings(input);
      setSnapshot(result);
      setMessage(success);
      closeEditor();
      await onSaved();
    } catch (unknownError) {
      setError(userFacingError(unknownError));
    } finally {
      setBusy(false);
    }
  }

  function saveModel(): void {
    if (!snapshot || !draft) return;
    const { originalModel, aliasesText, ...fields } = draft;
    const model = fields.model.trim();
    const aliases = aliasesText.split(/[\n,]/).map((value) => value.trim()).filter(Boolean);
    const remaining = snapshot.overrides.models.filter((item) => item.model !== originalModel);
    const names = [...remaining.flatMap((item) => [item.model, ...item.aliases]), model, ...aliases].map(normalizeName);
    if (!model || names.some((name) => !name) || new Set(names).size !== names.length) {
      setError("模型名称或别名为空或重复，请修改后重试。");
      return;
    }
    const next: ModelPriceSettings = { ...fields, model, aliases };
    void persist({ ...snapshot.overrides, models: [...remaining, next] }, `已保存 ${model} 的价格。`);
  }

  function editModel(price: ModelPriceSettings, originalModel: string | null): void {
    setError(null);
    setMessage("");
    setDraft({ ...price, originalModel, aliasesText: price.aliases.join("\n") });
  }

  function addModel(): void {
    editModel({ model: "", inputPerMillion: "", cachedInputPerMillion: "", cacheWriteInputPerMillion: "", outputPerMillion: "", aliases: [] }, null);
  }

  function removeModel(price: ModelPriceSettings, builtin: boolean): void {
    if (!snapshot || !window.confirm(builtin ? `恢复 ${price.model} 的内置价格？` : `删除 ${price.model} 的自定义价格？`)) return;
    void persist({ ...snapshot.overrides, models: snapshot.overrides.models.filter((item) => item.model !== price.model) }, builtin ? "已恢复内置价格。" : "已删除自定义价格。");
  }

  const customModels = snapshot?.overrides.models ?? [];
  const customNames = new Set(customModels.flatMap((model) => [model.model, ...model.aliases]).map(normalizeName));
  const builtins = snapshot?.builtinModels ?? [];
  const builtinNames = new Set(builtins.map((model) => normalizeName(model.model)));
  const rows = [...customModels, ...builtins.filter((price) => !customNames.has(normalizeName(price.model)))];
  const providerIds = [...new Set([...Object.keys(providers), ...Object.keys(multipliers)])];

  return (
    <Panel eyebrow="成本设置" title="模型定价">
      <p className="field-hint">单位：美元 / 百万 Token。支持添加 GPT-6 等模型及别名；请按所用渠道填写价格。保存后更新历史成本估算，不影响实际账单。</p>
      {pricingPreview ? <p className="warning-box">浏览器预览：此处保存仅用于界面演示，不修改桌面应用定价或统计。</p> : null}
      <div className="actions" ref={addButtonRef}>
        <Button disabled={!snapshot || busy || Boolean(draft)} onClick={addModel}>添加模型价格</Button>
        {!snapshot ? <Button disabled={busy} onClick={() => { void reload(); }} variant="secondary">重新加载价格</Button> : null}
        {snapshot ? <span className="inline-muted">内置价格快照 {snapshot.pricingAsOf}</span> : null}
      </div>
      <div className="configuration-feedback" aria-live="polite">
        {busy ? <p role="status">正在处理定价…</p> : null}
        {message ? <p role="status">{message}</p> : null}
        {error ? <p className="error-banner" role="alert" id="pricing-error">{error}</p> : null}
      </div>
      {draft ? (
        <form aria-describedby={error ? "pricing-error" : undefined} className="configuration-fields" onSubmit={(event) => { event.preventDefault(); saveModel(); }}>
          <h3>{draft.originalModel ? "编辑模型价格" : "添加模型价格"}</h3>
          <fieldset disabled={busy} className="configuration-stack">
            <legend className="sr-only">模型与价格</legend>
            <Field label="模型名称">
              <input autoFocus required value={draft.model} placeholder="例如 gpt-6" onChange={(event) => setDraft({ ...draft, model: event.target.value })} />
            </Field>
            <div className="form-grid">
              {PRICE_FIELDS.map(([key, label]) => (
                <Field key={key} label={`${label}（美元 / 百万 Token）`}>
                  <input type="number" inputMode="decimal" required min="0" step="any" value={draft[key]}
                    onChange={(event) => setDraft({ ...draft, [key]: event.target.value })} />
                </Field>
              ))}
            </div>
            <Field label="模型别名（每行一个或逗号分隔，可留空）">
              <textarea rows={2} value={draft.aliasesText} onChange={(event) => setDraft({ ...draft, aliasesText: event.target.value })} />
            </Field>
            <div className="actions">
              <Button type="submit">保存模型价格</Button>
              <Button variant="ghost" onClick={closeEditor}>取消</Button>
            </div>
          </fieldset>
        </form>
      ) : null}
      <div className="pricing-model-list">
        {rows.map((price) => {
          const custom = customModels.includes(price);
          const builtin = builtinNames.has(normalizeName(price.model));
          return (
            <article className="pricing-model-row" key={price.model}>
              <div>
                <strong>{price.model}</strong> <Badge tone={custom ? "info" : "neutral"}>{custom ? "自定义" : "内置"}</Badge>
                <p className="field-hint">新输入 {price.inputPerMillion} · 缓存输入 {price.cachedInputPerMillion} · 缓存写入 {price.cacheWriteInputPerMillion} · 输出 {price.outputPerMillion}</p>
                {price.aliases.length ? <p className="field-hint">别名：{price.aliases.join("、")}</p> : null}
              </div>
              <div className="actions">
                <Button disabled={busy || Boolean(draft)} variant="secondary" label={`编辑 ${price.model} 价格`} onClick={() => editModel(price, custom ? price.model : null)}>编辑</Button>
                {custom ? <Button disabled={busy || Boolean(draft)} variant="ghost" label={`${builtin ? "恢复默认" : "删除"} ${price.model}`} onClick={() => removeModel(price, builtin)}>{builtin ? "恢复默认" : "删除"}</Button> : null}
              </div>
            </article>
          );
        })}
      </div>
      <details className="pricing-multipliers">
        <summary>账号成本倍率</summary>
        <p className="field-hint">留空使用 1 倍；可填写大于 0 的折扣或加价倍率，例如 0.8。仅用于成本估算。</p>
        <form onSubmit={(event) => {
          event.preventDefault();
          if (!snapshot) return;
          const providerMultipliers = Object.fromEntries(Object.entries(multipliers).filter(([, value]) => value.trim()));
          if (Object.values(providerMultipliers).some((value) => !Number.isFinite(Number(value)) || Number(value) <= 0)) {
            setError("账号成本倍率必须大于 0。");
            return;
          }
          void persist({ ...snapshot.overrides, providerMultipliers }, "已保存账号成本倍率。");
        }}>
          <fieldset className="configuration-stack" disabled={busy || !snapshot || Boolean(draft)}>
            <legend className="sr-only">各账号的成本倍率</legend>
            <div className="form-grid">
              {providerIds.map((id) => (
                <Field key={id} label={providers[id] ? providerAccountTitle(providers[id]) : `历史账号 ${id}`}>
                  <input type="number" step="any" min="0" placeholder="1" aria-describedby="multiplier-help" value={multipliers[id] ?? ""}
                    onChange={(event) => setMultipliers((current) => ({ ...current, [id]: event.target.value }))} />
                </Field>
              ))}
            </div>
            <p className="field-hint" id="multiplier-help">倍率必须大于 0，留空恢复默认。</p>
            <Button type="submit">保存账号倍率</Button>
          </fieldset>
        </form>
      </details>
    </Panel>
  );
}

function normalizeName(value: string): string {
  return value.trim().toLowerCase().split("/").pop() ?? "";
}
