import { useCallback, useEffect, useRef, useState } from "react";
import { invoke } from "@tauri-apps/api/core";

type SaveState = "idle" | "saving" | "saved" | "error";

/** "planner-a" -> "Planner A" */
function plannerLabel(slot: string) {
  return slot
    .split("-")
    .map((part) => part.charAt(0).toUpperCase() + part.slice(1))
    .join(" ");
}

function ModelSelect({
  value,
  onChange,
  options,
  invalid,
}: {
  value: string;
  onChange: (value: string) => void;
  options: string[];
  invalid?: boolean;
}) {
  // Always offer the configured id, even when enumeration failed or the CLI no
  // longer advertises it — otherwise the select would silently blank the value.
  const choices = options.includes(value) || !value ? options : [value, ...options];
  return (
    <select
      value={value || ""}
      onChange={(e) => onChange(e.target.value)}
      className={`w-full appearance-none rounded-md bg-white py-1.5 pl-2.5 pr-8 text-sm text-slate-900 outline-none ring-1 ring-inset transition-shadow focus:ring-2 ${
        invalid
          ? "ring-amber-300 focus:ring-amber-500"
          : "ring-slate-200 focus:ring-blue-500"
      }`}
      style={{
        backgroundImage: `url("data:image/svg+xml,%3csvg xmlns='http://www.w3.org/2000/svg' fill='none' viewBox='0 0 20 20'%3e%3cpath stroke='%2394a3b8' stroke-linecap='round' stroke-linejoin='round' stroke-width='1.5' d='M6 8l4 4 4-4'/%3e%3c/svg%3e")`,
        backgroundPosition: "right 0.375rem center",
        backgroundRepeat: "no-repeat",
        backgroundSize: "1.25em 1.25em",
      }}
    >
      <option value="" disabled>
        Select a model
      </option>
      {choices.map((m) => (
        <option key={m} value={m}>
          {m}
        </option>
      ))}
    </select>
  );
}

function Section({
  title,
  children,
}: {
  title: string;
  children: React.ReactNode;
}) {
  return (
    <section className="mb-10">
      <h3 className="mb-2 text-xs font-semibold uppercase tracking-wider text-slate-400">
        {title}
      </h3>
      <div className="divide-y divide-slate-100 border-t border-slate-100">
        {children}
      </div>
    </section>
  );
}

function Row({
  label,
  hint,
  children,
}: {
  label: string;
  hint?: React.ReactNode;
  children: React.ReactNode;
}) {
  return (
    <div className="flex items-center justify-between gap-6 py-3">
      <div className="min-w-0">
        <div className="text-sm text-slate-900">{label}</div>
        {hint && <div className="mt-0.5 text-xs text-slate-400">{hint}</div>}
      </div>
      <div className="w-56 shrink-0">{children}</div>
    </div>
  );
}

export default function Settings() {
  const [config, setConfig] = useState<any>(null);
  const [models, setModels] = useState<string[]>([]);
  const [loadError, setLoadError] = useState<string | null>(null);
  const [modelsError, setModelsError] = useState<string | null>(null);
  const [saveState, setSaveState] = useState<SaveState>("idle");
  const [saveError, setSaveError] = useState<string | null>(null);
  const dirty = useRef(false);
  // The newest config awaiting persistence, and whether a write is in flight.
  // Saves are serialized through `flush`, so a slow write can never land after —
  // and clobber — a newer one, and a stale result can never overwrite a newer
  // status.
  const pending = useRef<any>(null);
  const saving = useRef(false);

  // Config and the model list load independently: `available_models` shells out to
  // the `copilot` CLI, and its absence must not block editing everything else.
  useEffect(() => {
    let cancelled = false;
    invoke("read_config")
      .then((loaded) => !cancelled && setConfig(loaded))
      .catch((e) => !cancelled && setLoadError(String(e)));
    invoke("available_models")
      .then((loaded) => !cancelled && setModels(loaded as string[]))
      .catch((e) => !cancelled && setModelsError(String(e)));
    return () => {
      cancelled = true;
    };
  }, []);

  const flush = useCallback(async () => {
    if (saving.current) return;
    saving.current = true;
    try {
      while (pending.current) {
        const target = pending.current;
        setSaveState("saving");
        try {
          await invoke("write_config", { config: target });
          setSaveError(null);
          setSaveState("saved");
          if (pending.current === target) pending.current = null;
        } catch (e: any) {
          setSaveError(String(e));
          setSaveState("error");
          // Keep the change pending so the next edit retries it, but stop looping
          // on a value we already know the core rejects.
          if (pending.current === target) break;
        }
      }
    } finally {
      saving.current = false;
    }
  }, []);

  // Auto-save, debounced, once the user has actually changed something.
  useEffect(() => {
    if (!config || !dirty.current) return;
    pending.current = config;
    const timer = setTimeout(() => void flush(), 500);
    return () => clearTimeout(timer);
  }, [config, flush]);

  useEffect(() => {
    if (saveState !== "saved") return;
    const timer = setTimeout(() => setSaveState("idle"), 2000);
    return () => clearTimeout(timer);
  }, [saveState]);

  const updateModel = (key: string, value: string) => {
    dirty.current = true;
    setConfig((c: any) => ({ ...c, models: { ...c.models, [key]: value } }));
  };

  const updatePlanner = (key: string, value: string) => {
    dirty.current = true;
    setConfig((c: any) => ({ ...c, planners: { ...c.planners, [key]: value } }));
  };

  if (loadError) {
    return (
      <div className="mx-auto max-w-2xl rounded-md border border-red-200 bg-red-50 p-4">
        <h3 className="text-sm font-medium text-red-800">
          Failed to load settings
        </h3>
        <p className="mt-1 text-sm text-red-700">{loadError}</p>
      </div>
    );
  }

  if (!config) {
    return (
      <div className="mx-auto max-w-2xl py-16 text-center text-sm text-slate-400">
        Loading…
      </div>
    );
  }

  const conflict =
    !!config.models.reviewer &&
    config.models.reviewer === config.models.implementer;

  return (
    <div className="mx-auto max-w-2xl pb-16">
      <div className="flex h-5 items-center justify-end text-xs">
        {saveState === "saving" && <span className="text-slate-400">Saving…</span>}
        {saveState === "saved" && (
          <span className="text-emerald-600">Saved</span>
        )}
        {saveState === "error" && (
          <span className="text-red-600">{saveError}</span>
        )}
      </div>

      {modelsError && (
        <div className="mb-6 rounded-md border border-amber-200 bg-amber-50 px-3 py-2 text-xs text-amber-800">
          Could not list models from the Copilot CLI, so the dropdowns show only
          what is already configured. {modelsError}
        </div>
      )}

      <Section title="Coordination">
        <Row label="Coordinator" hint="Merges, convergence, final synthesis">
          <ModelSelect
            value={config.models.coordinator}
            onChange={(v) => updateModel("coordinator", v)}
            options={models}
          />
        </Row>
      </Section>

      <Section title="Planners">
        {Object.keys(config.planners || {})
          .sort()
          .map((key) => (
            <Row key={key} label={plannerLabel(key)}>
              <ModelSelect
                value={config.planners[key]}
                onChange={(v) => updatePlanner(key, v)}
                options={models}
              />
            </Row>
          ))}
      </Section>

      <Section title="Execution">
        <Row label="Implementer" hint="Writes code and runs tasks">
          <ModelSelect
            value={config.models.implementer}
            onChange={(v) => updateModel("implementer", v)}
            options={models}
          />
        </Row>
        <Row
          label="Reviewer"
          hint={
            conflict ? (
              <span className="text-amber-600">
                Must differ from the Implementer
              </span>
            ) : (
              "Reviews the Implementer's work"
            )
          }
        >
          <ModelSelect
            value={config.models.reviewer}
            onChange={(v) => updateModel("reviewer", v)}
            options={models}
            invalid={conflict}
          />
        </Row>
      </Section>
    </div>
  );
}
