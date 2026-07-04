"use client";

import { useEffect, useState, useCallback } from "react";
import { useQueryClient } from "@tanstack/react-query";
import { apiGet, apiPatch, apiDelete } from "@/lib/api";
import { useMemoryStats, qk } from "@/lib/queries";
import { useTranslation } from "@/hooks/use-translation";
import { formatDate } from "@/lib/format";
import { Button } from "@/components/ui/button";
import { Badge } from "@/components/ui/badge";
import { Card } from "@/components/ui/card";
import { IconTile } from "@/components/ui/icon-tile";
import { SearchInput } from "@/components/ui/search-input";
import { Pagination } from "@/components/ui/pagination";
import { Separator } from "@/components/ui/separator";
import { CopyableCode } from "@/components/ui/copyable-code";
import { ErrorBanner } from "@/components/ui/error-banner";
import { Skeleton } from "@/components/ui/skeleton";
import { EmptyState } from "@/components/ui/empty-state";
import { PageHeader } from "@/components/ui/page-header";
import { ConfirmDialog } from "@/components/ui/confirm-dialog";
import { Markdown } from "@/components/ui/markdown";
import { Brain, Trash2, Pin, PinOff, ExternalLink, ArrowLeft, MessageSquare, FileText } from "lucide-react";
import { useSearchParams, useRouter } from "next/navigation";
import type { MemoryDocument } from "@/types/api";

// ── Full document view ──────────────────────────────────────────

function DocumentFullView({ id, onBack }: { id: string; onBack: () => void }) {
  const { t } = useTranslation();
  const [content, setContent] = useState<string | null>(null);
  const [loading, setLoading] = useState(true);
  const [error, setError] = useState<string | null>(null);

  useEffect(() => {
    apiGet<{ content: string }>(`/api/memory/documents/${id}`)
      .then((res) => setContent(res.content))
      .catch((err) => setError(err.message || t("memory.doc_load_error")))
      .finally(() => setLoading(false));
  }, [id, t]);

  if (loading) {
    return (
      <div className="flex flex-col h-full p-4 md:p-8 max-w-4xl mx-auto w-full">
        <Skeleton className="h-8 w-48 mb-4" />
        <Skeleton className="h-64 w-full" />
      </div>
    );
  }

  return (
    <div className="flex flex-col h-full p-4 md:p-8 max-w-4xl mx-auto w-full overflow-hidden">
      <div className="flex items-center justify-between mb-6 shrink-0">
        <Button variant="ghost" size="sm" onClick={onBack}>
          <ArrowLeft className="h-4 w-4 mr-2" /> {t("common.back")}
        </Button>
        {content && <CopyableCode value={content} display={t("common.copy")} className="max-w-xs" />}
      </div>
      {error && <ErrorBanner error={error} className="mb-4 shrink-0" />}
      <div className="flex-1 min-h-0 overflow-y-auto">
        <div className="prose prose-sm dark:prose-invert max-w-none break-words prose-p:my-2 prose-headings:my-3 prose-ul:my-2 prose-li:my-0.5 prose-pre:my-3 prose-pre:overflow-x-auto prose-pre:max-w-full">
          <Markdown>{content || ""}</Markdown>
        </div>
      </div>
    </div>
  );
}

// ── Memory list page ────────────────────────────────────────────

export default function MemoryPage() {
  const { t, locale } = useTranslation();
  const searchParams = useSearchParams();
  const router = useRouter();
  const docId = searchParams.get("doc");
  const qc = useQueryClient();
  const { data: stats } = useMemoryStats();

  const [chunks, setChunks] = useState<MemoryDocument[]>([]);
  const [total, setTotal] = useState(0);
  const [search, setSearch] = useState("");
  const [offset, setOffset] = useState(0);
  const [error, setError] = useState("");
  const [loading, setLoading] = useState(false);
  const [deleteTarget, setDeleteTarget] = useState<string | null>(null);

  const limit = 20;

  const fetchChunks = useCallback(async () => {
    setLoading(true);
    setError("");
    try {
      const params = new URLSearchParams({
        limit: limit.toString(),
        offset: offset.toString(),
      });
      if (search) params.append("query", search);

      const res = await apiGet<{ documents: MemoryDocument[]; total: number }>(`/api/memory/documents?${params.toString()}`);
      setChunks(res.documents);
      setTotal(res.total);
    } catch (err) {
      setError(err instanceof Error ? err.message : t("memory.fetch_error"));
    } finally {
      setLoading(false);
    }
  }, [offset, search, t]);

  useEffect(() => {
    fetchChunks();
  }, [fetchChunks]);

  const doDelete = async () => {
    if (!deleteTarget) return;
    try {
      await apiDelete(`/api/memory/documents/${deleteTarget}`);
      setChunks(prev => prev.filter((c) => c.id !== deleteTarget));
      setDeleteTarget(null);
      qc.invalidateQueries({ queryKey: qk.memoryStats });
    } catch (err) {
      setError(err instanceof Error ? err.message : String(err));
    }
  };

  const togglePin = async (doc: MemoryDocument) => {
    try {
      const newPinned = !doc.pinned;
      await apiPatch(`/api/memory/documents/${doc.id}`, { pinned: newPinned });
      setChunks(chunks.map((c) => (c.id === doc.id ? { ...c, pinned: newPinned } : c)));
      qc.invalidateQueries({ queryKey: qk.memoryStats });
    } catch (err) {
      setError(err instanceof Error ? err.message : String(err));
    }
  };

  const page = Math.floor(offset / limit) + 1;
  const pageCount = Math.max(1, Math.ceil(total / limit));

  const next = () => setOffset(offset + limit);
  const prev = () => setOffset(Math.max(0, offset - limit));

  // Reset pagination on filter change
  useEffect(() => {
    setOffset(0);
  }, [search]);

  // Full document view mode
  if (docId) {
    return <DocumentFullView id={docId} onBack={() => router.push("/memory")} />;
  }

  return (
    <div className="flex flex-col h-full p-4 md:p-6 lg:p-8 w-full overflow-hidden">
      <PageHeader
        title={t("memory.title")}
        description={t("memory.subtitle")}
        className="mb-4 shrink-0"
      />

      {stats && (
        <div className="mb-6 flex items-center gap-4 shrink-0">
          <div className="flex flex-col">
            <span className="text-2xs uppercase tracking-wider text-muted-foreground">{t("memory.documents")}</span>
            <span className="font-mono text-sm font-bold text-foreground tabular-nums">{stats.total.toLocaleString()}</span>
          </div>
          <Separator orientation="vertical" className="h-8" />
          <div className="flex flex-col">
            <span className="text-2xs uppercase tracking-wider text-muted-foreground">{t("memory.pinned")}</span>
            <span className="font-mono text-sm font-bold text-foreground tabular-nums">{stats.pinned.toLocaleString()}</span>
          </div>
          {stats.embed_dim && (
            <>
              <Separator orientation="vertical" className="h-8" />
              <div className="flex flex-col">
                <span className="text-2xs uppercase tracking-wider text-muted-foreground">{t("memory.embed_dim")}</span>
                <span className="font-mono text-sm font-bold text-foreground tabular-nums">{stats.embed_dim.toLocaleString()}</span>
              </div>
            </>
          )}
        </div>
      )}

      {error && <ErrorBanner error={error} className="mb-4 shrink-0" />}

      <div className="flex flex-col flex-1 min-h-0">
          {/* Search */}
          <div className="mb-6 shrink-0">
            <SearchInput
              value={search}
              onChange={setSearch}
              placeholder={t("memory.search_placeholder")}
              debounceMs={300}
            />
          </div>

          {/* Document list */}
          <div className="flex-1 min-h-0 overflow-y-auto pr-1 -mr-1 custom-scrollbar">
            {loading && chunks.length === 0 ? (
              <div className="space-y-3">
                {[1, 2, 3].map((i) => (
                  <Skeleton key={i} className="h-20 w-full rounded-xl" />
                ))}
              </div>
            ) : chunks.length === 0 ? (
              <EmptyState icon={Brain} text={t("memory.nothing_found")} height="h-64" />
            ) : (
              <div className="grid gap-3">
                {chunks.map((doc) => {
                  const isSession = doc.source?.startsWith("auto:session") || doc.source?.startsWith("Session:");
                  return (
                    <Card key={doc.id} interactive className="group relative flex flex-col p-4">
                      <div className="flex flex-col gap-3 sm:flex-row sm:items-start sm:justify-between sm:gap-4">
                        <div className="flex-1 min-w-0">
                          <div className="flex items-center gap-2 mb-1">
                            <div className="flex items-center gap-2 min-w-0">
                              <IconTile tone={isSession ? "primary" : "muted"} size="sm">
                                {isSession ? <MessageSquare /> : <FileText />}
                              </IconTile>
                              <h3 className="font-semibold text-sm truncate text-foreground group-hover:text-primary transition-colors">
                                {doc.source?.replace("auto:session:", "Session: ") || t("memory.untitled")}
                              </h3>
                            </div>
                            {doc.pinned && (
                              <Badge variant="outline-primary" size="sm" className="shrink-0">
                                <Pin className="h-4 w-4" />
                              </Badge>
                            )}
                          </div>
                          <div className="flex flex-wrap items-center gap-x-3 gap-y-1">
                            <span className="text-2xs uppercase tracking-wider font-bold text-muted-foreground-subtle">
                              ID: {doc.id.split("-")[0]}
                            </span>
                            <span className="text-2xs text-muted-foreground-subtle">
                              {doc.created_at ? formatDate(doc.created_at, locale) : ""}
                            </span>
                            {doc.scope === "shared" && (
                              <Badge variant="secondary" size="sm">
                                shared
                              </Badge>
                            )}
                          </div>
                        </div>

                        <div className="flex flex-wrap items-center gap-1 sm:shrink-0 sm:opacity-0 sm:group-hover:opacity-100 sm:group-focus-within:opacity-100 focus-within:opacity-100 transition-opacity">
                          <Button variant="ghost" size="sm" className="text-xs px-2 min-w-0" onClick={() => router.push(`/memory?doc=${doc.id}`)}>
                            <ExternalLink className="h-4 w-4 mr-1.5 shrink-0" /> <span className="truncate">{t("memory.show_full_document")}</span>
                          </Button>
                          <Button variant="ghost" size="sm" onClick={() => togglePin(doc)} className="w-7 p-0">
                            {doc.pinned ? <PinOff className="h-3.5 w-3.5" /> : <Pin className="h-3.5 w-3.5" />}
                          </Button>
                          <Button variant="ghost" size="sm" onClick={() => setDeleteTarget(doc.id)} className="text-xs px-2 text-destructive hover:bg-destructive/10">
                            <Trash2 className="h-4 w-4 mr-1.5" /> {t("common.delete")}
                          </Button>
                        </div>
                      </div>
                    </Card>
                  );
                })}
              </div>
            )}
          </div>

          {/* Pagination */}
          {chunks.length > 0 && (
            <Pagination
              className="mt-6 shrink-0"
              page={page}
              total={pageCount}
              onPrev={prev}
              onNext={next}
            />
          )}
        </div>

      <ConfirmDialog
        open={!!deleteTarget}
        onClose={() => setDeleteTarget(null)}
        onConfirm={doDelete}
        title={t("memory.delete_chunk_title")}
        description={t("memory.delete_chunk_description")}
      />
    </div>
  );
}
