"use client";

import { useEffect, useState, useCallback, useMemo, useRef } from "react";
import dynamic from "next/dynamic";
import { apiGet, apiPut, apiDelete, isBinaryFile, wsMkdir, wsRename, wsDeleteRecursive, wsUpload, signWorkspacePaths } from "@/lib/api";
import type { WorkspaceFile } from "@/types/api";
import { BinaryViewer } from "@/components/workspace/binary-viewer";
import { Button } from "@/components/ui/button";
import { ErrorBanner } from "@/components/ui/error-banner";
import { Input } from "@/components/ui/input";
import { ConfirmDialog } from "@/components/ui/confirm-dialog";
import { Sheet, SheetContent, SheetTrigger } from "@/components/ui/sheet";
import { SidebarTrigger } from "@/components/ui/sidebar";
import { EmptyState } from "@/components/ui/empty-state";
import { getLangFromFilename } from "@/components/workspace/code-editor";
import { useTranslation } from "@/hooks/use-translation";
import { Folder, FileCode, Save, Trash2, FilePlus, FolderTree, CornerDownRight, FolderMinus, FolderPlus, Pencil, Download, Upload } from "lucide-react";
import type { FileEntry } from "@/types/api";
import { buildRenameTarget } from "./file-ops";

const ObsidianEditor = dynamic(
  () => import("@/components/workspace/obsidian-editor").then((m) => m.ObsidianEditor),
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
  const [fileData, setFileData] = useState<WorkspaceFile | null>(null);
  const [content, setContent] = useState("");
  const [original, setOriginal] = useState("");
  const [error, setError] = useState("");
  const [saved, setSaved] = useState(false);
  const [newFileName, setNewFileName] = useState("");
  const [showNewFile, setShowNewFile] = useState(false);
  const [newFolderName, setNewFolderName] = useState("");
  const [showNewFolder, setShowNewFolder] = useState(false);
  const [deleteTarget, setDeleteTarget] = useState<string | null>(null);
  const [deleteDirTarget, setDeleteDirTarget] = useState<string | null>(null);
  const [deleteRecursiveTarget, setDeleteRecursiveTarget] = useState<string | null>(null);
  const [renameTarget, setRenameTarget] = useState<{ name: string; isDir: boolean } | null>(null);
  const [renameValue, setRenameValue] = useState("");
  const [isSidebarOpen, setIsSidebarOpen] = useState(false);
  const [isDragOver, setIsDragOver] = useState(false);
  const loadFileRequestRef = useRef(0);
  const uploadInputRef = useRef<HTMLInputElement>(null);

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
    setFileData(null);
    setContent("");
    setOriginal("");
    setCurrentPath(currentPath ? `${currentPath}/${dirName}` : dirName);
  };

  const navigateUp = () => {
    setSelectedFile("");
    setFileData(null);
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
      const data = await apiGet<WorkspaceFile>(`/api/workspace/${filePath}`);
      // Discard stale response if user clicked another file
      if (loadFileRequestRef.current !== requestId) return;
      setSelectedFile(filePath);
      setFileData(data);
      if (!("is_binary" in data)) {
        setContent(data.content);
        setOriginal(data.content);
      } else {
        setContent("");
        setOriginal("");
      }
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
        setFileData(null);
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

  const doDeleteRecursive = async () => {
    if (!deleteRecursiveTarget) return;
    try {
      await wsDeleteRecursive(deleteRecursiveTarget);
      const wasOpen = selectedFile.startsWith(deleteRecursiveTarget + "/") || selectedFile === deleteRecursiveTarget;
      if (wasOpen) {
        setSelectedFile("");
        setFileData(null);
        setContent("");
        setOriginal("");
      }
      setDeleteRecursiveTarget(null);
      await fetchFiles();
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

  const createFolder = async () => {
    const name = newFolderName.trim();
    if (!name) return;
    try {
      const dirPath = currentPath ? `${currentPath}/${name}` : name;
      await wsMkdir(dirPath);
      setNewFolderName("");
      setShowNewFolder(false);
      await fetchFiles();
    } catch (e) {
      setError(`${e}`);
    }
  };

  const doRename = async () => {
    if (!renameTarget) return;
    const newName = renameValue.trim();
    if (!newName || newName === renameTarget.name) {
      setRenameTarget(null);
      return;
    }
    try {
      const { from, to } = buildRenameTarget(currentPath, renameTarget.name, newName);
      await wsRename(from, to);
      if (selectedFile === from) {
        setSelectedFile("");
        setFileData(null);
        setContent("");
        setOriginal("");
      }
      setRenameTarget(null);
      await fetchFiles();
    } catch (e) {
      setError(`${e}`);
    }
  };

  const downloadEntry = async (name: string) => {
    const path = currentPath ? `${currentPath}/${name}` : name;
    let url: string;
    if (fileData && selectedFile === path && isBinaryFile(fileData)) {
      url = fileData.url;
    } else {
      const map = await signWorkspacePaths([path]);
      url = map[path];
      if (!url) { setError(t("workspace.sign_error")); return; }
    }
    const a = document.createElement("a");
    a.href = url; a.download = name; a.click();
  };

  const doUpload = async (fileList: FileList | File[]) => {
    const files = Array.from(fileList);
    if (files.length === 0) return;
    try {
      await wsUpload(currentPath, files);
      await fetchFiles();
    } catch (e) {
      setError(`${e}`);
    }
  };

  const isMarkdown = useMemo(() => selectedFile.endsWith(".md"), [selectedFile]);
  const language = useMemo(() => getLangFromFilename(selectedFile), [selectedFile]);
  const selectedFileName = selectedFile.split("/").pop() || selectedFile;
  const breadcrumbs = currentPath ? currentPath.split("/").filter(Boolean) : [];

  const fileList = (
    <div
      className="flex h-full flex-col bg-card/50"
      onDragOver={(e) => { e.preventDefault(); setIsDragOver(true); }}
      onDragLeave={(e) => { if (!e.currentTarget.contains(e.relatedTarget as Node)) setIsDragOver(false); }}
      onDrop={(e) => {
        e.preventDefault();
        setIsDragOver(false);
        if (e.dataTransfer.files.length > 0) doUpload(e.dataTransfer.files);
      }}
    >
      <div className="flex items-center justify-between p-4 border-b border-border/50">
        <span className="text-sm font-semibold text-foreground">{t("workspace.title")}</span>
        <div className="flex items-center gap-1">
          <Button variant="ghost" size="icon-sm" aria-label={t("workspace.upload")} className="hover:bg-primary/10" onClick={() => uploadInputRef.current?.click()}>
            <Upload className="h-4 w-4" />
          </Button>
          <Button variant="ghost" size="icon-sm" aria-label={t("workspace.create_folder")} className="hover:bg-primary/10" onClick={() => { setShowNewFile(false); setShowNewFolder(true); }}>
            <FolderPlus className="h-4 w-4" />
          </Button>
          <Button variant="ghost" size="icon-sm" aria-label={t("workspace.create")} className="hover:bg-primary/10" onClick={() => { setShowNewFolder(false); setShowNewFile(true); }}>
            <FilePlus className="h-4 w-4" />
          </Button>
        </div>
      </div>
      {isDragOver && (
        <div className="mx-2 mb-1 mt-1 flex items-center justify-center rounded-md border-2 border-dashed border-primary/50 bg-primary/5 py-3 text-xs text-primary/70 pointer-events-none">
          {t("workspace.drop_to_upload")}
        </div>
      )}
      <input
        ref={uploadInputRef}
        type="file"
        multiple
        className="hidden"
        onChange={(e) => { if (e.target.files) { doUpload(e.target.files); e.target.value = ""; } }}
      />
      <div className="flex-1 min-h-0 overflow-y-auto overscroll-contain">
        <div className="p-2 space-y-0.5">
          {currentPath && (
            <button
              onClick={navigateUp}
              className="flex w-full items-center gap-2 rounded-md px-3 py-2 text-left font-mono text-sm text-muted-foreground hover:bg-muted/50 transition-colors focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-ring focus-visible:ring-inset"
            >
              <CornerDownRight className="h-4 w-4 rotate-180" />
              <span className="opacity-60">..</span>
            </button>
          )}
          {files.map((f) => (
            <div key={f.name} className="group relative">
              {renameTarget?.name === f.name ? (
                <div className="mt-1 p-2 border border-primary/30 rounded-lg bg-primary/5">
                  <Input
                    placeholder={f.name}
                    className="h-9 font-mono text-sm bg-background border-border focus:border-primary/50 mb-2"
                    value={renameValue}
                    onChange={(e) => setRenameValue(e.target.value)}
                    onKeyDown={(e) => {
                      if (e.key === "Enter") doRename();
                      if (e.key === "Escape") setRenameTarget(null);
                    }}
                    autoFocus
                  />
                  <div className="flex gap-1">
                    <Button size="sm" className="flex-1" onClick={doRename}>{t("workspace.rename")}</Button>
                    <Button size="sm" variant="ghost" onClick={() => setRenameTarget(null)}>{t("common.cancel")}</Button>
                  </div>
                </div>
              ) : (
                <button
                  onClick={() => {
                    if (f.is_dir) navigateTo(f.name);
                    else loadFile(f.name);
                  }}
                  className={`flex w-full items-center gap-2 rounded-md px-3 py-2 text-left font-mono text-sm transition-all focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-ring focus-visible:ring-inset ${
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
                  <span className="truncate flex-1">{f.name}</span>
                  {/* Per-entry action icons — visible on hover */}
                  <span className="hidden group-hover:flex items-center gap-0.5 shrink-0" onClick={(e) => e.stopPropagation()}>
                    <span
                      role="button"
                      tabIndex={0}
                      aria-label={t("workspace.rename")}
                      className="rounded p-0.5 hover:bg-muted/70 text-muted-foreground hover:text-foreground"
                      onClick={(e) => { e.stopPropagation(); setRenameTarget({ name: f.name, isDir: f.is_dir }); setRenameValue(f.name); }}
                      onKeyDown={(e) => { if (e.key === "Enter" || e.key === " ") { e.preventDefault(); e.stopPropagation(); setRenameTarget({ name: f.name, isDir: f.is_dir }); setRenameValue(f.name); } }}
                    >
                      <Pencil className="h-3 w-3" />
                    </span>
                    {!f.is_dir && (
                      <span
                        role="button"
                        tabIndex={0}
                        aria-label={t("workspace.download")}
                        className="rounded p-0.5 hover:bg-muted/70 text-muted-foreground hover:text-foreground"
                        onClick={(e) => { e.stopPropagation(); downloadEntry(f.name); }}
                        onKeyDown={(e) => { if (e.key === "Enter" || e.key === " ") { e.preventDefault(); e.stopPropagation(); downloadEntry(f.name); } }}
                      >
                        <Download className="h-3 w-3" />
                      </span>
                    )}
                    <span
                      role="button"
                      tabIndex={0}
                      aria-label={f.is_dir ? t("workspace.delete_recursive_title") : t("workspace.delete_file")}
                      className="rounded p-0.5 hover:bg-destructive/20 text-muted-foreground hover:text-destructive"
                      onClick={(e) => {
                        e.stopPropagation();
                        const path = currentPath ? `${currentPath}/${f.name}` : f.name;
                        if (f.is_dir) setDeleteRecursiveTarget(path);
                        else setDeleteTarget(path);
                      }}
                      onKeyDown={(e) => {
                        if (e.key === "Enter" || e.key === " ") {
                          e.preventDefault();
                          e.stopPropagation();
                          const path = currentPath ? `${currentPath}/${f.name}` : f.name;
                          if (f.is_dir) setDeleteRecursiveTarget(path);
                          else setDeleteTarget(path);
                        }
                      }}
                    >
                      <Trash2 className="h-3 w-3" />
                    </span>
                  </span>
                </button>
              )}
            </div>
          ))}

          {showNewFolder && (
            <div className="mt-2 p-2 border border-primary/30 rounded-lg bg-primary/5">
              <Input
                placeholder="folder-name"
                className="h-9 font-mono text-sm bg-background border-border focus:border-primary/50 mb-2"
                value={newFolderName}
                onChange={(e) => setNewFolderName(e.target.value)}
                onKeyDown={(e) => {
                  if (e.key === "Enter") createFolder();
                  if (e.key === "Escape") setShowNewFolder(false);
                }}
                autoFocus
              />
              <div className="flex gap-1">
                <Button size="sm" className="flex-1" onClick={createFolder}>{t("workspace.create")}</Button>
                <Button size="sm" variant="ghost" onClick={() => setShowNewFolder(false)}>{t("common.cancel")}</Button>
              </div>
            </div>
          )}

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
        <div className="flex items-center gap-3 overflow-hidden min-w-0">
          <SidebarTrigger className="md:hidden shrink-0 h-9 w-9" />
          <Sheet open={isSidebarOpen} onOpenChange={setIsSidebarOpen}>
            <SheetTrigger asChild>
              <Button variant="ghost" size="icon" aria-label={t("workspace.open_explorer")} className="md:hidden shrink-0 h-9 w-9">
                <FolderTree className="h-5 w-5" />
              </Button>
            </SheetTrigger>
            <SheetContent side="left" className="p-0 w-[75vw] md:w-[280px] border-r border-border bg-sidebar">
              {fileList}
            </SheetContent>
          </Sheet>

          <div className="flex items-center gap-2 font-mono text-sm overflow-hidden">
            <Folder className="h-4 w-4 text-primary shrink-0" />
            <div className="flex items-center whitespace-nowrap overflow-x-auto scrollbar-none pb-0.5">
              <button onClick={() => { setCurrentPath(""); setSelectedFile(""); setFileData(null); setContent(""); setOriginal(""); }} className="text-muted-foreground hover:text-primary transition-colors rounded focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-ring focus-visible:ring-inset">{t("workspace.breadcrumb_root")}</button>
              <span className="mx-1 text-muted-foreground/30">/</span>
              {breadcrumbs.map((seg, i) => (
                <span key={i} className="flex items-center">
                  <button
                    className="text-muted-foreground hover:text-primary transition-colors rounded focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-ring focus-visible:ring-inset"
                    onClick={() => {
                      setSelectedFile("");
                      setFileData(null);
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
                  {!(fileData && isBinaryFile(fileData)) && isDirty && <span className="text-xs text-primary font-medium">{t("workspace.modified")}</span>}
                </div>
                {!(fileData && isBinaryFile(fileData)) && (
                  <Button
                    size="sm"
                    onClick={saveFile}
                    disabled={!isDirty}
                  >
                    <Save className="h-4 w-4 mr-2" />
                    {t("workspace.save")}
                  </Button>
                )}
              </div>

              {/* Dynamic Editor Height Adjustment */}
              <div className="flex-1 min-h-0 flex flex-col overflow-hidden">
                {fileData && isBinaryFile(fileData) ? (
                  <BinaryViewer file={fileData} />
                ) : isMarkdown ? (
                  <ObsidianEditor
                    value={content}
                    onChange={setContent}
                    onSave={() => { if (isDirty) saveFile(); }}
                    noteDir={selectedFile.split("/").slice(0, -1).join("/")}
                    onNavigate={() => {}}
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
            <EmptyState
              icon={FileCode}
              text={t("workspace.no_file_selected")}
              height="flex-1"
              className="p-8"
              hint={
                <Button variant="outline" className="mt-6 md:hidden" onClick={() => setIsSidebarOpen(true)}>
                  {t("workspace.open_explorer")}
                </Button>
              }
            />
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

      <ConfirmDialog
        open={!!deleteRecursiveTarget}
        onClose={() => setDeleteRecursiveTarget(null)}
        onConfirm={doDeleteRecursive}
        title={t("workspace.delete_recursive_title")}
        description={t("workspace.delete_recursive_description", { name: deleteRecursiveTarget?.split("/").pop() ?? "" })}
        confirmLabel={t("workspace.delete_recursive_action")}
      />
    </div>
  );
}
