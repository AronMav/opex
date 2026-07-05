"use client";

import { useCallback, useMemo, useRef, useEffect, useState } from "react";
import { useTheme } from "next-themes";
import CodeMirror from "@uiw/react-codemirror";
import { oneDark } from "@codemirror/theme-one-dark";
import { python } from "@codemirror/lang-python";
import { keymap } from "@codemirror/view";
import {
  Dialog,
  DialogContent,
  DialogHeader,
  DialogTitle,
  DialogFooter,
} from "@/components/ui/dialog";
import { Button } from "@/components/ui/button";
import { Input } from "@/components/ui/input";
import { Switch } from "@/components/ui/switch";
import {
  Select,
  SelectContent,
  SelectItem,
  SelectTrigger,
  SelectValue,
} from "@/components/ui/select";
import { Field } from "@/components/ui/field";
import { DialogTabs } from "@/components/ui/dialog-tabs";
import { Settings2, FileCode2 } from "lucide-react";
import { getToken } from "@/lib/api";
import { useTranslation } from "@/hooks/use-translation";
import { spliceDescriptor, DescriptorFields, ParamDescriptor } from "./handler-descriptor";

// ── Types ──────────────────────────────────────────────────────────────────

export interface HandlerEditorProps {
  /** Present for edit; absent for create. */
  id?: string;
  /** Full .py source text (descriptor block + code body). */
  initialSource: string;
  /** "builtin" | "override" | "workspace" — informational only. */
  sourceKind?: string;
  /** Called after a successful save; callers should invalidate qk.handlers. */
  onSaved: () => void;
  onClose: () => void;
}

interface SaveError {
  field?: string;
  message: string;
}

// ── Descriptor form state extracted from a DescriptorFields object ─────────

function defaultFields(id?: string): DescriptorFields {
  return {
    id: id ?? "",
    labels: { en: "", ru: "" },
    descriptions: { en: "", ru: "" },
    icon: "",
    mime: [],
    max_size_mb: null,
    execution: "sync",
    order: 100,
    enabled: true,
    capability: null,
    output: "text",
    params: [],
  };
}

function parseDescriptorFromApi(desc: Record<string, unknown>): DescriptorFields {
  return {
    id: (desc.id as string) ?? "",
    labels: (desc.labels as Record<string, string>) ?? {},
    descriptions: (desc.descriptions as Record<string, string>) ?? {},
    icon: (desc.icon as string) ?? "",
    mime: ((desc.match as { mime?: string[] } | undefined)?.mime) ?? [],
    max_size_mb: ((desc.match as { max_size_mb?: number | null } | undefined)?.max_size_mb) ?? null,
    execution: (desc.execution as "sync" | "async") ?? "sync",
    order: (desc.order as number) ?? 100,
    enabled: (desc.enabled as boolean) ?? true,
    // Passthrough fields — not form-editable in v1 but must round-trip so
    // editing a builtin's label doesn't strip <capability>/<output>/<params>.
    capability: (desc.capability as string | null | undefined) ?? null,
    output: (desc.output as string | null | undefined) ?? "text",
    params: (desc.params as ParamDescriptor[] | undefined) ?? [],
  };
}

// ── Inline Python editor (mirrors code-editor.tsx) ────────────────────────

function PythonEditor({
  value,
  onChange,
  onSave,
}: {
  value: string;
  onChange: (v: string) => void;
  onSave?: () => void;
}) {
  const { resolvedTheme } = useTheme();
  const onSaveRef = useRef(onSave);
  useEffect(() => { onSaveRef.current = onSave; }, [onSave]);

  const saveKeymap = useMemo(
    () =>
      keymap.of([
        { key: "Mod-s", run: () => { onSaveRef.current?.(); return true; } },
      ]),
    [],
  );

  const handleChange = useCallback((val: string) => onChange(val), [onChange]);

  return (
    <div className="flex-1 min-h-0 overflow-hidden border rounded-md">
      <CodeMirror
        value={value}
        onChange={handleChange}
        theme={resolvedTheme === "dark" ? oneDark : "light"}
        extensions={[python(), saveKeymap]}
        basicSetup={{
          lineNumbers: true,
          foldGutter: true,
          highlightActiveLine: true,
          bracketMatching: true,
          indentOnInput: true,
          tabSize: 4,
        }}
        className="h-full [&_.cm-editor]:h-full [&_.cm-scroller]:overflow-auto"
        height="100%"
      />
    </div>
  );
}

// ── HandlerEditor ─────────────────────────────────────────────────────────

export function HandlerEditor({ id, initialSource, sourceKind, onSaved, onClose }: HandlerEditorProps) {
  const { t } = useTranslation();
  const isEdit = Boolean(id);

  const [tab, setTab] = useState<"settings" | "code">("settings");
  const [source, setSource] = useState(initialSource);
  const [fields, setFields] = useState<DescriptorFields>(() => defaultFields(id));
  const [errors, setErrors] = useState<SaveError[]>([]);
  const [saving, setSaving] = useState(false);
  const [syncing, setSyncing] = useState(false);

  // ── Keep source in sync when form fields change ──────────────────────────
  function updateField<K extends keyof DescriptorFields>(key: K, value: DescriptorFields[K]) {
    const next = { ...fields, [key]: value };
    setFields(next);
    setSource((src) => spliceDescriptor(src, next));
  }

  // ── Populate form from initialSource on mount (edit mode) ────────────────
  useEffect(() => {
    if (!initialSource) return;
    const headers = {
      "Content-Type": "application/json",
      Authorization: `Bearer ${getToken()}`,
    };
    fetch("/api/handlers/validate", {
      method: "POST",
      headers,
      body: JSON.stringify({ source: initialSource }),
    })
      .then((res) => res.ok ? res.json() : Promise.reject(res.status))
      .then((body: Record<string, unknown>) => {
        const desc = (body.descriptor ?? body) as Record<string, unknown>;
        setFields(parseDescriptorFromApi(desc));
      })
      .catch(() => { /* leave form at defaults; raw code is still editable */ });
  // eslint-disable-next-line react-hooks/exhaustive-deps
  }, []);

  // ── Sync form from code via /api/handlers/validate ───────────────────────
  async function syncFromCode() {
    setSyncing(true);
    setErrors([]);
    try {
      const headers = {
        "Content-Type": "application/json",
        Authorization: `Bearer ${getToken()}`,
      };
      const res = await fetch("/api/handlers/validate", {
        method: "POST",
        headers,
        body: JSON.stringify({ source }),
      });
      const body = await res.json().catch(() => ({})) as Record<string, unknown>;
      if (!res.ok) {
        setErrors((body.errors as SaveError[] | undefined) ?? [{ message: (body.error as string | undefined) ?? `HTTP ${res.status}` }]);
        return;
      }
      const desc = (body.descriptor ?? body) as Record<string, unknown>;
      setFields(parseDescriptorFromApi(desc));
    } catch (e) {
      setErrors([{ message: String(e) }]);
    } finally {
      setSyncing(false);
    }
  }

  // ── Save ─────────────────────────────────────────────────────────────────
  async function handleSave() {
    setSaving(true);
    setErrors([]);
    try {
      const headers = {
        "Content-Type": "application/json",
        Authorization: `Bearer ${getToken()}`,
      };
      const res = isEdit
        ? await fetch(`/api/handlers/${id}`, {
            method: "PUT",
            headers,
            body: JSON.stringify({ source }),
          })
        : await fetch("/api/handlers", {
            method: "POST",
            headers,
            body: JSON.stringify({ id: fields.id, source }),
          });

      if (!res.ok) {
        const body = await res.json().catch(() => ({})) as Record<string, unknown>;
        setErrors(
          (body.errors as SaveError[] | undefined) ??
          [{ field: "", message: (body.error as string | undefined) ?? `HTTP ${res.status}` }],
        );
        return;
      }
      onSaved();
    } catch (e) {
      setErrors([{ message: String(e) }]);
    } finally {
      setSaving(false);
    }
  }

  // ── Mime glob list helpers ────────────────────────────────────────────────
  const mimeStr = fields.mime.join(", ");
  function handleMimeChange(val: string) {
    const parsed = val.split(",").map((s) => s.trim()).filter(Boolean);
    updateField("mime", parsed);
  }

  const title = isEdit
    ? sourceKind
      ? t("tools.handler_edit_title_kind", { id: id ?? "", kind: sourceKind })
      : t("tools.handler_edit_title", { id: id ?? "" })
    : t("tools.handler_new_title");

  return (
    <Dialog open onOpenChange={(open) => { if (!open) onClose(); }}>
      <DialogContent className="w-[92vw] max-w-6xl sm:max-w-6xl h-[90vh] flex flex-col gap-0 p-0">
        <DialogHeader className="px-6 pt-5 pb-0 shrink-0">
          <DialogTitle className="pb-3">{title}</DialogTitle>
          <DialogTabs
            items={[
              { value: "settings", label: t("tools.handler_tab_settings"), icon: Settings2 },
              { value: "code", label: t("tools.handler_tab_code"), icon: FileCode2 },
            ]}
            value={tab}
            onChange={setTab}
            className="-mx-6 px-6"
          />
        </DialogHeader>
        <div className="border-t border-border bg-muted/10" />

        <div className="flex-1 min-h-0 overflow-hidden flex flex-col px-6 pt-3">
          {/* ── Descriptor form ── */}
          {tab === "settings" && (
          <div className="min-h-0 flex-1 overflow-y-auto pr-1 pb-2">
            <div className="grid grid-cols-1 sm:grid-cols-2 gap-4">
              <Field label={t("tools.handler_field_id")}>
                <Input
                  value={fields.id}
                  readOnly={isEdit}
                  disabled={isEdit}
                  placeholder="my_handler"
                  onChange={(e) => updateField("id", e.target.value)}
                />
              </Field>

              <Field label={t("tools.handler_field_icon")}>
                <Input
                  value={fields.icon}
                  placeholder="file"
                  onChange={(e) => updateField("icon", e.target.value)}
                />
              </Field>

              <Field label={t("tools.handler_field_label_en")}>
                <Input
                  value={fields.labels.en ?? ""}
                  placeholder={t("tools.handler_field_label_en_ph")}
                  onChange={(e) => updateField("labels", { ...fields.labels, en: e.target.value })}
                />
              </Field>

              <Field label={t("tools.handler_field_label_ru")}>
                <Input
                  value={fields.labels.ru ?? ""}
                  placeholder={t("tools.handler_field_label_ru_ph")}
                  onChange={(e) => updateField("labels", { ...fields.labels, ru: e.target.value })}
                />
              </Field>

              <Field label={t("tools.handler_field_desc_en")}>
                <Input
                  value={fields.descriptions.en ?? ""}
                  placeholder={t("tools.handler_field_desc_en_ph")}
                  onChange={(e) => updateField("descriptions", { ...fields.descriptions, en: e.target.value })}
                />
              </Field>

              <Field label={t("tools.handler_field_mime")}>
                <Input
                  value={mimeStr}
                  placeholder="image/*, application/pdf"
                  onChange={(e) => handleMimeChange(e.target.value)}
                />
              </Field>

              <Field label={t("tools.handler_field_max_size")}>
                <Input
                  type="number"
                  value={fields.max_size_mb ?? ""}
                  placeholder={t("tools.handler_field_max_size_ph")}
                  onChange={(e) => {
                    const v = e.target.value.trim();
                    updateField("max_size_mb", v === "" ? null : Number(v));
                  }}
                />
              </Field>

              <Field label={t("tools.handler_field_execution")}>
                <Select
                  value={fields.execution}
                  onValueChange={(v) => updateField("execution", v as "sync" | "async")}
                >
                  <SelectTrigger>
                    <SelectValue />
                  </SelectTrigger>
                  <SelectContent>
                    <SelectItem value="sync">{t("tools.handler_sync")}</SelectItem>
                    <SelectItem value="async">{t("tools.handler_async")}</SelectItem>
                  </SelectContent>
                </Select>
              </Field>

              <Field label={t("tools.handler_field_order")}>
                <Input
                  type="number"
                  value={fields.order}
                  onChange={(e) => updateField("order", Number(e.target.value))}
                />
              </Field>

              <div className="flex items-center gap-3 self-end pb-2">
                <Switch
                  id="handler-enabled"
                  checked={fields.enabled}
                  onCheckedChange={(v) => updateField("enabled", v)}
                />
                <label htmlFor="handler-enabled" className="text-sm font-medium cursor-pointer">
                  {t("tools.handler_field_enabled")}
                </label>
              </div>
            </div>

            <Button
              variant="outline"
              size="sm"
              onClick={syncFromCode}
              disabled={syncing}
              className="mt-4"
            >
              {syncing ? t("tools.handler_syncing") : t("tools.handler_sync_from_code")}
            </Button>
          </div>
          )}

          {/* ── Python editor ── */}
          {tab === "code" && (
          <div className="min-h-0 flex-1 flex flex-col gap-2 pb-2">
            <p className="text-xs text-muted-foreground shrink-0">
              {t("tools.handler_code_hint")}
            </p>
            <PythonEditor
              value={source}
              onChange={setSource}
              onSave={handleSave}
            />
          </div>
          )}
        </div>

        {/* ── Errors + footer ── */}
        <div className="shrink-0 px-6 pb-5 pt-3 border-t space-y-2">
          {errors.length > 0 && (
            <ul className="space-y-1">
              {errors.map((e, i) => (
                <li key={i} className="text-sm text-destructive break-words">
                  {e.field ? <span className="font-medium">{e.field}: </span> : null}
                  {e.message}
                </li>
              ))}
            </ul>
          )}
          <DialogFooter>
            <Button variant="outline" onClick={onClose} disabled={saving}>
              {t("tools.handler_cancel")}
            </Button>
            <Button onClick={handleSave} disabled={saving}>
              {saving ? t("tools.handler_saving") : isEdit ? t("tools.handler_save") : t("tools.handler_create")}
            </Button>
          </DialogFooter>
        </div>
      </DialogContent>
    </Dialog>
  );
}
