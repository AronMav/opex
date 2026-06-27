"use client";

import { useEffect, useState, useCallback, useMemo, useRef } from "react";
import dynamic from "next/dynamic";
import { apiGet, apiPut, apiDelete, isBinaryFile, wsMkdir, wsRename, wsDeleteRecursive, wsUpload, signWorkspacePaths } from "@/lib/api";
import type { WorkspaceFile } from "@/types/api";
import { BinaryViewer } from "@/components/workspace/binary-viewer";
import { WorkspaceFileTree } from "@/components/workspace/workspace-file-tree";
import { Button } from "@/components/ui/button";
import { ErrorBanner } from "@/components/ui/error-banner";
import { ConfirmDialog } from "@/components/ui/confirm-dialog";
import { Sheet, SheetContent, SheetTrigger } from "@/components/ui/sheet";
import { SidebarTrigger } from "@/components/ui/sidebar";
import { EmptyState } from "@/components/ui/empty-state";
import { getLangFromFilename } from "@/components/workspace/code-editor";
import { useTranslation } from "@/hooks/use-translation";
import { Folder, FileCode, Save, Trash2, FolderTree, FolderMinus } from "lucide-react";
import type { FileEntry } from "@/types/api";
import { buildRenameTarget, encodeWorkspacePath } from "./file-ops";

const ObsidianEditor = dynamic(
  () => import("@/components/workspace/obsidian-editor").then((m) => m.ObsidianEditor),
  { ssr: false, loading: () => <div className="flex-1 animate-pulse bg-muted/20" /> },
);

const CodeEditor = dynamic(
  () => import("@/components/workspace/code-editor").then((m) => m.CodeEditor),
  { ssr: false, loading: () => <div className="flex-1 animate-pulse bg-muted/20" /> },
);

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
  const [deleteRecursiveTarget, setDeleteRecursiveTarget] = useState<string | null>(null);
  const [renameTarget, setRenameTarget] = useState<{ name: string; isDir: boolean } | null>(null);
  const [renameValue, setRenameValue] = useState("");
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
      const endpoint = currentPath
        ? `/api/workspace/${encodeWorkspacePath(currentPath)}`
        : "/api/workspace";
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
      const data = await apiGet<WorkspaceFile>(`/api/workspace/${encodeWorkspacePath(filePath)}`);
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
      await apiPut(`/api/workspace/${encodeWorkspacePath(selectedFile)}`, { content });
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
      await apiDelete(`/api/workspace/${encodeWorkspacePath(deleteTarget)}`);
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
      // If we just deleted the current folder itself, navigate up
      if (deleteRecursiveTarget === currentPath) {
        const parts = currentPath.split("/").filter(Boolean);
        parts.pop();
        setCurrentPath(parts.join("/"));
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
      await apiPut(`/api/workspace/${encodeWorkspacePath(filePath)}`, { content: "" });
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
    const uploadFiles = Array.from(fileList);
    if (uploadFiles.length === 0) return;
    try {
      await wsUpload(currentPath, uploadFiles);
      await fetchFiles();
    } catch (e) {
      setError(`${e}`);
    }
  };

  const isMarkdown = useMemo(() => selectedFile.endsWith(".md"), [selectedFile]);
  const language = useMemo(() => getLangFromFilename(selectedFile), [selectedFile]);
  const selectedFileName = selectedFile.split("/").pop() || selectedFile;
  const breadcrumbs = currentPath ? currentPath.split("/").filter(Boolean) : [];

  // Shared props for both WorkspaceFileTree instances (mobile Sheet + desktop sidebar)
  const fileTreeProps = {
    files,
    currentPath,
    selectedFile,
    showNewFolder,
    showNewFile,
    newFolderName,
    newFileName,
    renameTarget,
    renameValue,
    onNavigateTo: navigateTo,
    onNavigateUp: navigateUp,
    onLoadFile: loadFile,
    onUpload: doUpload,
    onShowNewFolder: () => setShowNewFolder(true),
    onHideNewFolder: () => setShowNewFolder(false),
    onShowNewFile: () => setShowNewFile(true),
    onHideNewFile: () => setShowNewFile(false),
    onNewFolderNameChange: setNewFolderName,
    onNewFileNameChange: setNewFileName,
    onCreateFolder: createFolder,
    onCreateFile: createFile,
    onRenameStart: (name: string, isDir: boolean) => setRenameTarget({ name, isDir }),
    onRenameValueChange: setRenameValue,
    onRenameCommit: doRename,
    onRenameCancel: () => setRenameTarget(null),
    onDeleteFile: setDeleteTarget,
    onDeleteRecursive: setDeleteRecursiveTarget,
    onDownload: downloadEntry,
  };

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
              {/* Mobile instance — independent ref/state */}
              <WorkspaceFileTree {...fileTreeProps} />
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
            <Button size="sm" variant="destructive" onClick={() => setDeleteRecursiveTarget(currentPath)}>
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
        {/* Desktop Sidebar — independent instance */}
        <div className="hidden md:flex flex-col w-[240px] shrink-0 border-r border-border">
          <WorkspaceFileTree {...fileTreeProps} />
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
                    onNavigate={(target) => {
                      const fname = target.endsWith(".md") ? target : `${target}.md`;
                      loadFile(fname);
                    }}
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
