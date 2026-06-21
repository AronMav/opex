"use client";

import { useEffect, useState, useCallback, useMemo, useRef } from "react";
import dynamic from "next/dynamic";
import { apiGet, apiPut, apiDelete } from "@/lib/api";
import { Button } from "@/components/ui/button";
import { ErrorBanner } from "@/components/ui/error-banner";
import { Input } from "@/components/ui/input";
import { ConfirmDialog } from "@/components/ui/confirm-dialog";
import { Sheet, SheetContent, SheetTrigger } from "@/components/ui/sheet";
import { getLangFromFilename } from "@/components/workspace/code-editor";
import { useTranslation } from "@/hooks/use-translation";
import { Folder, FileCode, Save, Trash2, FilePlus, Menu, CornerDownRight, FolderMinus } from "lucide-react";
import type { FileEntry } from "@/types/api";

const MarkdownEditor = dynamic(
  () => import("@/components/workspace/markdown-editor").then((m) => m.MarkdownEditor),
  { ssr: false, loading: () => <div className="flex-1 animate-pulse bg-muted/20" /> },
);

const CodeEditor = dynamic(
  () => import("@/components/workspace/code-editor").then((m) => m.CodeEditor),
  { ssr: false, loading: () => <div className="flex-1 animate-pulse bg-muted/20" /> },
);

const CORE_FILES = ["SOUL.md", "IDENTITY.md", "TOOLS.md", "HEARTBEAT.md", "USER.md", "AGENTS.md"];

export default function WorkspacePage() {
  const { t } = useTranslation();
  const [currentPath, setCurrentPath] = useState("");
  const [files, setFiles] = useState<FileEntry[]>([]);
  const [selectedFile, setSelectedFile] = useState("");
  const [content, setContent] = useState("");
  const [original, setOriginal] = useState("");
  const [error, setError] = useState("");
  const [saved, setSaved] = useState(false);
  const [newFileName, setNewFileName] = useState("");
  const [showNewFile, setShowNewFile] = useState(false);
  const [deleteTarget, setDeleteTarget] = useState<string | null>(null);
  const [deleteDirTarget, setDeleteDirTarget] = useState<string | null>(null);
  const [isSidebarOpen, setIsSidebarOpen] = useState(false);
  const loadFileRequestRef = useRef(0);

  const isDirty = content !== original;

  // Warn user before navigating away with unsaved changes
  useEffect(() => {
    const handler = isDirty
      ? (e: BeforeUnloadEvent) => { e.preventDefault(); }
      : undefined;
    if (handler) window.addEventListener("beforeunload", handler);
    return () => { if (handler) window.removeEventListener("beforeunload", handler); };
  }, [isDirty]);

  const fetchFiles = useCallback(async () => {
    try {
      const endpoint = currentPath ? `/api/workspace/${currentPath}` : "/api/workspace";
      const data = await apiGet<{ files: FileEntry[] }>(endpoint);
      setFiles(data.files);
      setError("");
    } catch (e) {
      setError(`${e}`);
      setFiles([]);
    }
  }, [currentPath]);

  useEffect(() => { fetchFiles(); }, [fetchFiles]);

  const navigateTo = (dirName: string) => {
    setSelectedFile("");
    setContent("");
    setOriginal("");
    setCurrentPath(currentPath ? `${currentPath}/${dirName}` : dirName);
  };

  const navigateUp = () => {
    setSelectedFile("");
    setContent("");
    setOriginal("");
    const parts = currentPath.split("/").filter(Boolean);
    parts.pop();
    setCurrentPath(parts.join("/"));
  };

  const loadFile = async (name: string) => {
    const requestId = ++loadFileRequestRef.current;
    try {
      const filePath = currentPath ? `${currentPath}/${name}` : name;
      const data = await apiGet<{ content: string }>(`/api/workspace/${filePath}`);
      // Discard stale response if user clicked another file
      if (loadFileRequestRef.current !== requestId) return;
      setSelectedFile(filePath);
      setContent(data.content);
      setOriginal(data.content);
      setSaved(false);
      setError("");
      setIsSidebarOpen(false);
    } catch (e) {
      if (loadFileRequestRef.current !== requestId) return;
      setError(`${e}`);
    }
  };

  const saveFile = async () => {
    try {
      await apiPut(`/api/workspace/${selectedFile}`, { content });
      setOriginal(content);
      setSaved(true);
      setTimeout(() => setSaved(false), 2000);
    } catch (e) {
      setError(`${e}`);
    }
  };

  const doDelete = async () => {
    if (!deleteTarget) return;
    try {
      await apiDelete(`/api/workspace/${deleteTarget}`);
      if (selectedFile === deleteTarget) {
        setSelectedFile("");
        setContent("");
        setOriginal("");
      }
      setDeleteTarget(null);
      await fetchFiles();
    } catch (e) {
      setError(`${e}`);
    }
  };

  const doDeleteDir = async () => {
    if (!deleteDirTarget) return;
    try {
      await apiDelete(`/api/workspace/${deleteDirTarget}`);
      setDeleteDirTarget(null);
      navigateUp();
    } catch (e) {
      setError(`${e}`);
    }
  };

  const createFile = async () => {
    const name = newFileName.trim();
    if (!name) return;
    try {
      const filePath = currentPath ? `${currentPath}/${name}` : name;
      await apiPut(`/api/workspace/${filePath}`, { content: "" });
      setNewFileName("");
      setShowNewFile(false);
      await fetchFiles();
      loadFile(name);
    } catch (e) {
      setError(`${e}`);
    }
  };

  const isMarkdown = useMemo(() => selectedFile.endsWith(".md"), [selectedFile]);
  const language = useMemo(() => getLangFromFilename(selectedFile), [selectedFile]);
  const selectedFileName = selectedFile.split("/").pop() || selectedFile;
  const breadcrumbs = currentPath ? currentPath.split("/").filter(Boolean) : [];

  const fileList = (
    <div className="flex h-full flex-col bg-card/50">
      <div className="flex items-center justify-between p-4 border-b border-border/50">
        <span className="text-sm font-semibold text-foreground">{t("workspace.title")}</span>
        <Button variant="ghost" size="icon-sm" className="hover:bg-primary/10" onClick={() => setShowNewFile(true)}>
          <FilePlus className="h-4 w-4" />
        </Button>
      </div>
      <div className="flex-1 min-h-0 overflow-y-auto overscroll-contain">
        <div className="p-2 space-y-0.5">
          {currentPath && (
            <button
              onClick={navigateUp}
              className="flex w-full items-center gap-2 rounded-md px-3 py-2 text-left font-mono text-sm text-muted-foreground hover:bg-muted/50 transition-colors"
            >
              <CornerDownRight className="h-4 w-4 rotate-180" />
              <span className="opacity-60">..</span>
            </button>
          )}
          {files.map((f) => (
            <button
              key={f.name}
              onClick={() => {
                if (f.is_dir) navigateTo(f.name);
                else loadFile(f.name);
              }}
              className={`flex w-full items-center gap-2 rounded-md px-3 py-2 text-left font-mono text-sm transition-all ${
                !f.is_dir && selectedFile.endsWith(f.name) && selectedFile === (currentPath ? `${currentPath}/${f.name}` : f.name)
                  ? "bg-primary/20 text-primary font-bold shadow-sm"
                  : f.is_dir
                    ? "text-primary/80 hover:bg-primary/10"
                    : CORE_FILES.includes(f.name)
                      ? "text-primary hover:bg-primary/5"
                      : "text-muted-foreground hover:bg-muted/50"
              }`}
            >
              {f.is_dir ? <Folder className="h-4 w-4 shrink-0" /> : <FileCode className="h-4 w-4 shrink-0" />}
              <span className="truncate">{f.name}</span>
            </button>
          ))}

          {showNewFile && (
            <div className="mt-2 p-2 border border-primary/30 rounded-lg bg-primary/5">
              <Input
                placeholder="filename.md"
                className="h-9 font-mono text-sm bg-background border-border focus:border-primary/50 mb-2"
                value={newFileName}
                onChange={(e) => setNewFileName(e.target.value)}
                onKeyDown={(e) => {
                  if (e.key === "Enter") createFile();
                  if (e.key === "Escape") setShowNewFile(false);
                }}
                autoFocus
              />
              <div className="flex gap-1">
                <Button size="sm" className="flex-1" onClick={createFile}>{t("workspace.create")}</Button>
                <Button size="sm" variant="ghost" onClick={() => setShowNewFile(false)}>{t("common.cancel")}</Button>
              </div>
            </div>
          )}
        </div>
      </div>
    </div>
  );

  return (
    <div className="flex h-full flex-col bg-background selection:bg-primary/20 overflow-hidden">
      {/* Header / Breadcrumbs */}
      <div className="flex h-14 shrink-0 items-center justify-between border-b border-border bg-card/40 px-4 md:px-6">
        <div className="flex items-center gap-3 overflow-hidden">
          <Sheet open={isSidebarOpen} onOpenChange={setIsSidebarOpen}>
            <SheetTrigger asChild>
              <Button variant="ghost" size="icon" className="md:hidden shrink-0">
                <Menu className="h-5 w-5" />
              </Button>
            </SheetTrigger>
            <SheetContent side="left" className="p-0 w-[75vw] md:w-[280px] border-r border-border bg-sidebar">
              {fileList}
            </SheetContent>
          </Sheet>
          
          <div className="flex items-center gap-2 font-mono text-sm overflow-hidden">
            <Folder className="h-4 w-4 text-primary shrink-0" />
            <div className="flex items-center whitespace-nowrap overflow-x-auto scrollbar-none pb-0.5">
              <button onClick={() => { setCurrentPath(""); setSelectedFile(""); }} className="text-muted-foreground hover:text-primary transition-colors">{t("workspace.breadcrumb_root")}</button>
              <span className="mx-1 text-muted-foreground/30">/</span>
              {breadcrumbs.map((seg, i) => (
                <span key={i} className="flex items-center">
                  <button
                    className="text-muted-foreground hover:text-primary transition-colors"
                    onClick={() => {
                      setSelectedFile("");
                      setContent("");
                      setOriginal("");
                      setCurrentPath(breadcrumbs.slice(0, i + 1).join("/"));
                    }}
                  >
                    {seg}
                  </button>
                  <span className="mx-1 text-muted-foreground/30">/</span>
                </span>
              ))}
            </div>
          </div>
        </div>

        <div className="flex items-center gap-2">
          {saved && <span className="hidden sm:inline text-xs text-success font-medium">{t("workspace.saved")}</span>}
          {currentPath && !selectedFile && (
            <Button size="sm" variant="destructive" onClick={() => setDeleteDirTarget(currentPath)}>
              <FolderMinus className="h-4 w-4 md:mr-2" />
              <span className="hidden md:inline">{t("workspace.delete_folder")}</span>
            </Button>
          )}
          {selectedFile && (
            <Button size="sm" variant="destructive" onClick={() => setDeleteTarget(selectedFile)}>
              <Trash2 className="h-4 w-4 md:mr-2" />
              <span className="hidden md:inline">{t("workspace.delete_file")}</span>
            </Button>
          )}
        </div>
      </div>

      {error && <ErrorBanner error={error} className="m-4" />}

      <div className="flex min-h-0 flex-1 relative">
        {/* Desktop Sidebar */}
        <div className="hidden md:flex flex-col w-[240px] shrink-0 border-r border-border">
          {fileList}
        </div>

        {/* Editor Area */}
        <div className="flex min-w-0 flex-1 flex-col bg-background relative">
          {selectedFile ? (
            <>
              {/* Editor Toolbar */}
              <div className="sticky top-0 z-10 flex items-center justify-between border-b border-border/50 bg-background px-4 py-2">
                <div className="flex flex-col min-w-0">
                  <span className="font-mono text-sm font-bold text-foreground truncate">
                    {selectedFileName}
                  </span>
                  {isDirty && <span className="text-xs text-primary font-medium">{t("workspace.modified")}</span>}
                </div>
                <Button
                  size="sm"
                  onClick={saveFile}
                  disabled={!isDirty}
                >
                  <Save className="h-4 w-4 mr-2" />
                  {t("workspace.save")}
                </Button>
              </div>
              
              {/* Dynamic Editor Height Adjustment */}
              <div className="flex-1 min-h-0 flex flex-col overflow-hidden">
                {isMarkdown ? (
                  <MarkdownEditor
                    value={content}
                    onChange={setContent}
                    onSave={() => { if (isDirty) saveFile(); }}
                  />
                ) : (
                  <CodeEditor
                    value={content}
                    onChange={setContent}
                    onSave={() => { if (isDirty) saveFile(); }}
                    language={language}
                  />
                )}
              </div>
            </>
          ) : (
            <div className="flex flex-1 flex-col items-center justify-center p-8 text-center">
              <div className="relative mb-6">
                <div className="absolute inset-0 bg-primary/10 blur-3xl rounded-full" />
                <FileCode className="h-16 w-16 text-muted-foreground/20 relative" />
              </div>
              <h3 className="text-base font-semibold text-muted-foreground/60">{t("workspace.no_file_selected")}</h3>
              <p className="mt-2 text-sm text-muted-foreground/60 max-w-[240px]">{t("workspace.no_file_hint")}</p>
              <Button variant="outline" className="mt-6 md:hidden" onClick={() => setIsSidebarOpen(true)}>
                {t("workspace.open_explorer")}
              </Button>
            </div>
          )}
        </div>
      </div>

      <ConfirmDialog
        open={!!deleteTarget}
        onClose={() => setDeleteTarget(null)}
        onConfirm={doDelete}
        title={t("workspace.delete_file_title")}
        description={t("workspace.delete_file_description", { name: deleteTarget?.split("/").pop() ?? "" })}
      />

      <ConfirmDialog
        open={!!deleteDirTarget}
        onClose={() => setDeleteDirTarget(null)}
        onConfirm={doDeleteDir}
        title={t("workspace.delete_folder_title")}
        description={t("workspace.delete_folder_description", { name: deleteDirTarget?.split("/").pop() ?? "" })}
        confirmLabel={t("workspace.delete_folder_action")}
      />
    </div>
  );
}
