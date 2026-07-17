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
import {
  AlertDialog,
  AlertDialogAction,
  AlertDialogCancel,
  AlertDialogContent,
  AlertDialogDescription,
  AlertDialogFooter,
  AlertDialogHeader,
  AlertDialogTitle,
} from "@/components/ui/alert-dialog";
import { Sheet, SheetContent, SheetTrigger } from "@/components/ui/sheet";
import { SidebarTrigger } from "@/components/ui/sidebar";
import { EmptyState } from "@/components/ui/empty-state";
import { getLangFromFilename } from "@/components/workspace/code-editor";
import { useTranslation } from "@/hooks/use-translation";
import { Folder, FileCode, Save, Trash2, FolderTree, FolderMinus, Loader2 } from "lucide-react";
import type { FileEntry } from "@/types/api";
import { buildRenameTarget, encodeWorkspacePath } from "./file-ops";

// Pending navigation descriptor — stored when dirty guard intercepts an action
type PendingNav = () => void;

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
  const [loadingFile, setLoadingFile] = useState(false);
  // Pending navigation thunk captured when dirty guard intercepts an action
  const [pendingNav, setPendingNav] = useState<PendingNav | null>(null);
  const loadFileRequestRef = useRef(0);
  // Fix 3: track which file path is currently being loaded (set before fetch, cleared in finally)
  const loadingPathRef = useRef("");
  // Fix 5: store the "saved" flash timer id so we can cancel it on unmount or re-fire
  const savedTimerRef = useRef<ReturnType<typeof setTimeout> | null>(null);

  // Fix 5: clear any pending "saved" flash timer on unmount
  useEffect(() => () => {
    if (savedTimerRef.current) clearTimeout(savedTimerRef.current);
  }, []);

  const isDirty = content !== original;

  // Warn user before navigating away with unsaved changes (tab close / reload)
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

  // Guard helper: if dirty, stash the thunk and show the confirm dialog; otherwise run immediately.
  // Fix 4: if a pendingNav is already set (dialog already open), ignore new nav to avoid overwriting.
  const guardNav = useCallback((action: PendingNav) => {
    if (isDirty) {
      setPendingNav((prev) => prev !== null ? prev : action);
    } else {
      action();
    }
  }, [isDirty]);

  const navigateTo = useCallback((dirName: string) => {
    guardNav(() => {
      setSelectedFile("");
      setFileData(null);
      setContent("");
      setOriginal("");
      setCurrentPath((prev) => prev ? `${prev}/${dirName}` : dirName);
    });
  }, [guardNav]);

  const navigateUp = useCallback(() => {
    guardNav(() => {
      setSelectedFile("");
      setFileData(null);
      setContent("");
      setOriginal("");
      setCurrentPath((prev) => {
        const parts = prev.split("/").filter(Boolean);
        parts.pop();
        return parts.join("/");
      });
    });
  }, [guardNav]);

  const loadFile = useCallback(async (name: string) => {
    const requestId = ++loadFileRequestRef.current;
    const filePath = currentPath ? `${currentPath}/${name}` : name;
    // Fix 3: record which path is being loaded so mutations can detect in-flight loads
    loadingPathRef.current = filePath;
    setLoadingFile(true);
    try {
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
    } finally {
      // Only clear spinner and loadingPath if this is still the active request
      if (loadFileRequestRef.current === requestId) {
        loadingPathRef.current = "";
        setLoadingFile(false);
      }
    }
  }, [currentPath]);

  const guardLoadFile = useCallback((name: string) => {
    guardNav(() => { loadFile(name); });
  }, [guardNav, loadFile]);

  // Returns true on success so callers (e.g. "Save & continue") can gate
  // follow-up navigation on a clean save.
  const saveFile = async (): Promise<boolean> => {
    try {
      await apiPut(`/api/workspace/${encodeWorkspacePath(selectedFile)}`, { content });
      setOriginal(content);
      setSaved(true);
      // Fix 5: cancel any prior flash timer before scheduling a new one
      if (savedTimerRef.current) clearTimeout(savedTimerRef.current);
      savedTimerRef.current = setTimeout(() => setSaved(false), 2000);
      return true;
    } catch (e) {
      setError(`${e}`);
      return false;
    }
  };

  const doDelete = async () => {
    if (!deleteTarget) return;
    try {
      // Fix 2: clear stale error before mutation
      setError("");
      await apiDelete(`/api/workspace/${encodeWorkspacePath(deleteTarget)}`);
      // Fix 3: also clear editor if the file was being loaded in-flight
      if (selectedFile === deleteTarget || loadingPathRef.current === deleteTarget) {
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
      // Fix 2: clear stale error before mutation
      setError("");
      await wsDeleteRecursive(deleteRecursiveTarget);
      // Fix 3: also clear editor if the loading file was inside the deleted folder
      const wasOpen = selectedFile.startsWith(deleteRecursiveTarget + "/") || selectedFile === deleteRecursiveTarget;
      const loadingInside = loadingPathRef.current.startsWith(deleteRecursiveTarget + "/") || loadingPathRef.current === deleteRecursiveTarget;
      if (wasOpen || loadingInside) {
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

  const createFile = useCallback(async () => {
    const name = newFileName.trim();
    if (!name) return;
    try {
      // Fix 2: clear stale error before mutation
      setError("");
      const filePath = currentPath ? `${currentPath}/${name}` : name;
      await apiPut(`/api/workspace/${encodeWorkspacePath(filePath)}`, { content: "" });
      setNewFileName("");
      setShowNewFile(false);
      await fetchFiles();
      await loadFile(name);
    } catch (e) {
      setError(`${e}`);
    }
  }, [newFileName, currentPath, fetchFiles, loadFile]);

  const createFolder = useCallback(async () => {
    const name = newFolderName.trim();
    if (!name) return;
    try {
      // Fix 2: clear stale error before mutation
      setError("");
      const dirPath = currentPath ? `${currentPath}/${name}` : name;
      await wsMkdir(dirPath);
      setNewFolderName("");
      setShowNewFolder(false);
      await fetchFiles();
    } catch (e) {
      setError(`${e}`);
    }
  }, [newFolderName, currentPath, fetchFiles]);

  const doRename = useCallback(async () => {
    if (!renameTarget) return;
    const newName = renameValue.trim();
    if (!newName || newName === renameTarget.name) {
      setRenameTarget(null);
      return;
    }
    try {
      // Fix 2: clear stale error before mutation
      setError("");
      const { from, to } = buildRenameTarget(currentPath, renameTarget.name, newName);
      await wsRename(from, to);
      // Fix 3: also clear editor if the loading file is the one being renamed
      if (selectedFile === from || loadingPathRef.current === from) {
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
  }, [renameTarget, renameValue, currentPath, selectedFile, fetchFiles]);

  const downloadEntry = useCallback(async (name: string) => {
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
  }, [currentPath, fileData, selectedFile, t]);

  const doUpload = useCallback(async (fileList: FileList | File[]) => {
    const uploadedFiles = Array.from(fileList);
    if (uploadedFiles.length === 0) return;
    try {
      // Fix 2: clear stale error before mutation
      setError("");
      const result = await wsUpload(currentPath, uploadedFiles);
      // Fix 1: surface partial failures (name clash / >50MB) to the user
      if (result.errors.length > 0) {
        setError(t("workspace.upload_errors", { errors: result.errors.join(", ") }));
      }
      // Always refresh so the saved files appear even on partial failure
      await fetchFiles();
    } catch (e) {
      setError(`${e}`);
    }
  }, [currentPath, fetchFiles, t]);

  const isMarkdown = useMemo(() => selectedFile.endsWith(".md"), [selectedFile]);
  const language = useMemo(() => getLangFromFilename(selectedFile), [selectedFile]);
  const selectedFileName = selectedFile.split("/").pop() || selectedFile;
  const breadcrumbs = currentPath ? currentPath.split("/").filter(Boolean) : [];

  // Stable callbacks/object for both WorkspaceFileTree instances so the trees
  // don't re-render on every editor keystroke.
  const onShowNewFolder = useCallback(() => setShowNewFolder(true), []);
  const onHideNewFolder = useCallback(() => setShowNewFolder(false), []);
  const onShowNewFile = useCallback(() => setShowNewFile(true), []);
  const onHideNewFile = useCallback(() => setShowNewFile(false), []);
  const onRenameStart = useCallback((name: string, isDir: boolean) => setRenameTarget({ name, isDir }), []);
  const onRenameCancel = useCallback(() => setRenameTarget(null), []);

  const fileTreeProps = useMemo(() => ({
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
    onLoadFile: guardLoadFile,
    onUpload: doUpload,
    onShowNewFolder,
    onHideNewFolder,
    onShowNewFile,
    onHideNewFile,
    onNewFolderNameChange: setNewFolderName,
    onNewFileNameChange: setNewFileName,
    onCreateFolder: createFolder,
    onCreateFile: createFile,
    onRenameStart,
    onRenameValueChange: setRenameValue,
    onRenameCommit: doRename,
    onRenameCancel,
    onDeleteFile: setDeleteTarget,
    onDeleteRecursive: setDeleteRecursiveTarget,
    onDownload: downloadEntry,
  }), [
    files, currentPath, selectedFile, showNewFolder, showNewFile,
    newFolderName, newFileName, renameTarget, renameValue,
    navigateTo, navigateUp, guardLoadFile,
    onShowNewFolder, onHideNewFolder, onShowNewFile, onHideNewFile,
    onRenameStart, onRenameCancel,
    doUpload, createFile, createFolder, doRename, downloadEntry,
  ]);

  // Navigate to root — guarded
  const navigateToRoot = useCallback(() => {
    guardNav(() => {
      setCurrentPath("");
      setSelectedFile("");
      setFileData(null);
      setContent("");
      setOriginal("");
    });
  }, [guardNav]);

  return (
    <div className="flex h-full flex-col bg-background selection:bg-primary/20 overflow-hidden">
      {/* Header / Breadcrumbs */}
      <div className="flex h-[var(--toolbar-h)] shrink-0 items-center justify-between border-b border-border bg-card/30 px-4 md:px-6">
        <div className="flex items-center gap-3 overflow-hidden min-w-0">
          <SidebarTrigger className="md:hidden shrink-0 h-9 w-9" />
          <Sheet open={isSidebarOpen} onOpenChange={setIsSidebarOpen}>
            <SheetTrigger asChild>
              <Button variant="ghost" size="icon" aria-label={t("workspace.open_explorer")} className="md:hidden shrink-0 h-9 w-9">
                <FolderTree className="h-5 w-5" />
              </Button>
            </SheetTrigger>
            <SheetContent side="left" className="p-0 w-[75dvw] md:w-[var(--sidebar-w)] border-r border-border bg-sidebar">
              {/* Mobile instance — independent ref/state */}
              <WorkspaceFileTree {...fileTreeProps} />
            </SheetContent>
          </Sheet>

          <div className="flex items-center gap-2 font-mono text-sm overflow-hidden">
            <Folder className="h-4 w-4 text-primary shrink-0" />
            <div className="flex items-center whitespace-nowrap overflow-x-auto scrollbar-none pb-0.5">
              <Button variant="link" size="sm" onClick={navigateToRoot} className="text-muted-foreground hover:text-primary h-auto p-0">{t("workspace.breadcrumb_root")}</Button>
              <span className="mx-1 text-muted-foreground/30">/</span>
              {breadcrumbs.map((seg, i) => {
                const segPath = breadcrumbs.slice(0, i + 1).join("/");
                return (
                  <span key={segPath} className="flex items-center">
                    <Button
                      variant="link"
                      size="sm"
                      className="text-muted-foreground hover:text-primary h-auto p-0"
                      onClick={() => {
                        guardNav(() => {
                          setSelectedFile("");
                          setFileData(null);
                          setContent("");
                          setOriginal("");
                          setCurrentPath(segPath);
                        });
                      }}
                    >
                      {seg}
                    </Button>
                    <span className="mx-1 text-muted-foreground/30">/</span>
                  </span>
                );
              })}
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
        <div className="hidden md:flex flex-col w-60 shrink-0 border-r border-border">
          <WorkspaceFileTree {...fileTreeProps} />
        </div>

        {/* Editor Area */}
        <div className="flex min-w-0 flex-1 flex-col bg-background relative">
          {loadingFile ? (
            <div className="flex flex-1 items-center justify-center">
              <Loader2 className="h-5 w-5 animate-spin text-muted-foreground" />
            </div>
          ) : selectedFile ? (
            <>
              {/* Editor Toolbar */}
              <div className="sticky top-0 z-10 flex items-center justify-between border-b border-border/50 bg-background px-4 py-2">
                <div className="flex flex-col min-w-0">
                  <span className="font-mono text-sm font-bold text-foreground truncate">
                    {selectedFileName}
                  </span>
                  {!(fileData && isBinaryFile(fileData)) && (
                    isDirty
                      ? <span className="text-xs text-primary font-medium">{t("workspace.modified")}</span>
                      : saved
                        ? <span className="text-xs text-success font-medium">{t("workspace.saved")}</span>
                        : null
                  )}
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
                      guardLoadFile(fname);
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

      {/* Unsaved-changes guard dialog — Save & continue / Discard / Cancel */}
      <AlertDialog open={!!pendingNav} onOpenChange={(o) => { if (!o) setPendingNav(null); }}>
        <AlertDialogContent className="border-border rounded-xl">
          <AlertDialogHeader>
            <AlertDialogTitle className="text-base font-bold">
              {t("workspace.unsaved_title")}
            </AlertDialogTitle>
            <AlertDialogDescription className="text-sm text-muted-foreground mt-2">
              {t("workspace.unsaved_description")}
            </AlertDialogDescription>
          </AlertDialogHeader>
          <AlertDialogFooter className="mt-4">
            <AlertDialogCancel>{t("common.cancel")}</AlertDialogCancel>
            <AlertDialogAction
              variant="destructive"
              onClick={() => {
                const nav = pendingNav;
                setPendingNav(null);
                setContent(original); // discard edits so the follow-up load starts clean
                nav?.();
              }}
            >
              {t("workspace.unsaved_discard")}
            </AlertDialogAction>
            <AlertDialogAction
              onClick={async (e) => {
                // Save first; only proceed with navigation if the save succeeded.
                e.preventDefault();
                const nav = pendingNav;
                const ok = await saveFile();
                if (!ok) return; // keep the dialog open so the user sees the error
                setPendingNav(null);
                nav?.();
              }}
            >
              {t("workspace.unsaved_save")}
            </AlertDialogAction>
          </AlertDialogFooter>
        </AlertDialogContent>
      </AlertDialog>
    </div>
  );
}
