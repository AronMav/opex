"use client";

import { useRef, useState } from "react";
import { Button } from "@/components/ui/button";
import { Input } from "@/components/ui/input";
import {
  DropdownMenu,
  DropdownMenuContent,
  DropdownMenuItem,
  DropdownMenuTrigger,
} from "@/components/ui/dropdown-menu";
import { useTranslation } from "@/hooks/use-translation";
import {
  Folder,
  FileCode,
  FileText,
  FileJson,
  Image as ImageIcon,
  FilePlus,
  FolderPlus,
  CornerDownRight,
  Pencil,
  Download,
  Trash2,
  Upload,
  MoreVertical,
  type LucideIcon,
} from "lucide-react";
import type { FileEntry } from "@/types/api";

const CORE_FILES = ["SOUL.md", "IDENTITY.md", "TOOLS.md", "HEARTBEAT.md", "USER.md", "AGENTS.md"];

// Map a filename to a lucide icon by extension so the tree isn't a wall of
// identical FileCode glyphs. Light switch — no library, just common buckets.
const CODE_EXTS = new Set([
  "js", "jsx", "ts", "tsx", "py", "rs", "go", "rb", "java", "c", "h", "cpp",
  "cs", "sh", "bash", "php", "sql", "css", "scss", "html", "vue", "svelte",
]);
const TEXT_EXTS = new Set(["md", "mdx", "txt", "log", "rst", "csv"]);
const JSON_EXTS = new Set(["json", "jsonc", "yaml", "yml", "toml", "xml", "ini", "env"]);
const IMAGE_EXTS = new Set(["png", "jpg", "jpeg", "gif", "webp", "svg", "bmp", "ico", "avif"]);

function fileIcon(name: string): LucideIcon {
  const ext = name.includes(".") ? name.slice(name.lastIndexOf(".") + 1).toLowerCase() : "";
  if (IMAGE_EXTS.has(ext)) return ImageIcon;
  if (JSON_EXTS.has(ext)) return FileJson;
  if (TEXT_EXTS.has(ext)) return FileText;
  if (CODE_EXTS.has(ext)) return FileCode;
  return FileText;
}

export interface WorkspaceFileTreeProps {
  files: FileEntry[];
  currentPath: string;
  selectedFile: string;
  showNewFolder: boolean;
  showNewFile: boolean;
  newFolderName: string;
  newFileName: string;
  renameTarget: { name: string; isDir: boolean } | null;
  renameValue: string;
  onNavigateTo: (dirName: string) => void;
  onNavigateUp: () => void;
  onLoadFile: (name: string) => void;
  onUpload: (files: FileList | File[]) => void;
  onShowNewFolder: () => void;
  onHideNewFolder: () => void;
  onShowNewFile: () => void;
  onHideNewFile: () => void;
  onNewFolderNameChange: (v: string) => void;
  onNewFileNameChange: (v: string) => void;
  onCreateFolder: () => void;
  onCreateFile: () => void;
  onRenameStart: (name: string, isDir: boolean) => void;
  onRenameValueChange: (v: string) => void;
  onRenameCommit: () => void;
  onRenameCancel: () => void;
  onDeleteFile: (path: string) => void;
  onDeleteRecursive: (path: string) => void;
  onDownload: (name: string) => void;
}

export function WorkspaceFileTree({
  files,
  currentPath,
  selectedFile,
  showNewFolder,
  showNewFile,
  newFolderName,
  newFileName,
  renameTarget,
  renameValue,
  onNavigateTo,
  onNavigateUp,
  onLoadFile,
  onUpload,
  onShowNewFolder,
  onHideNewFolder,
  onShowNewFile,
  onHideNewFile,
  onNewFolderNameChange,
  onNewFileNameChange,
  onCreateFolder,
  onCreateFile,
  onRenameStart,
  onRenameValueChange,
  onRenameCommit,
  onRenameCancel,
  onDeleteFile,
  onDeleteRecursive,
  onDownload,
}: WorkspaceFileTreeProps) {
  const { t } = useTranslation();
  // Each WorkspaceFileTree instance owns its own upload ref and drag state
  const uploadInputRef = useRef<HTMLInputElement>(null);
  const [isDragOver, setIsDragOver] = useState(false);

  return (
    <div
      className="flex h-full flex-col bg-card/50"
      onDragOver={(e) => { e.preventDefault(); setIsDragOver(true); }}
      onDragLeave={(e) => { if (!e.currentTarget.contains(e.relatedTarget as Node)) setIsDragOver(false); }}
      onDrop={(e) => {
        e.preventDefault();
        setIsDragOver(false);
        if (e.dataTransfer.files.length > 0) onUpload(e.dataTransfer.files);
      }}
    >
      {/* Header */}
      <div className="flex items-center justify-between p-4 border-b border-border/50">
        <span className="text-sm font-semibold text-foreground">{t("workspace.title")}</span>
        <div className="flex items-center gap-1">
          <Button
            variant="ghost"
            size="icon-sm"
            aria-label={t("workspace.upload")}
            className="hover:bg-primary/10"
            onClick={() => uploadInputRef.current?.click()}
          >
            <Upload className="h-4 w-4" />
          </Button>
          <Button
            variant="ghost"
            size="icon-sm"
            aria-label={t("workspace.create_folder")}
            className="hover:bg-primary/10"
            onClick={() => { onHideNewFile(); onShowNewFolder(); }}
          >
            <FolderPlus className="h-4 w-4" />
          </Button>
          <Button
            variant="ghost"
            size="icon-sm"
            aria-label={t("workspace.create")}
            className="hover:bg-primary/10"
            onClick={() => { onHideNewFolder(); onShowNewFile(); }}
          >
            <FilePlus className="h-4 w-4" />
          </Button>
        </div>
      </div>

      {isDragOver && (
        <div className="mx-2 mb-1 mt-1 flex items-center justify-center rounded-md border-2 border-dashed border-primary/50 bg-primary/5 py-3 text-xs text-primary/80 pointer-events-none">
          {t("workspace.drop_to_upload")}
        </div>
      )}

      <input
        ref={uploadInputRef}
        type="file"
        multiple
        className="hidden"
        onChange={(e) => { if (e.target.files) { onUpload(e.target.files); e.target.value = ""; } }}
      />

      <div className="flex-1 min-h-0 overflow-y-auto overscroll-contain">
        <div className="p-2 space-y-0.5">
          {currentPath && (
            <Button
              variant="ghost"
              size="sm"
              onClick={onNavigateUp}
              className="flex w-full items-center gap-2 justify-start font-mono text-sm text-muted-foreground hover:bg-muted/50 h-auto py-2"
            >
              <CornerDownRight className="h-4 w-4 rotate-180" />
              <span className="opacity-60">..</span>
            </Button>
          )}

          {files.map((f) => {
            const entryPath = currentPath ? `${currentPath}/${f.name}` : f.name;
            const FileIcon = fileIcon(f.name);

            return (
              <div key={entryPath} role="listitem">
                {renameTarget?.name === f.name ? (
                  <div className="mt-1 p-2 border border-primary/30 rounded-lg bg-primary/5">
                    <Input
                      placeholder={f.name}
                      className="h-9 font-mono text-sm bg-background border-border focus:border-primary/50 mb-2"
                      value={renameValue}
                      onChange={(e) => onRenameValueChange(e.target.value)}
                      onKeyDown={(e) => {
                        if (e.key === "Enter") onRenameCommit();
                        if (e.key === "Escape") onRenameCancel();
                      }}
                      autoFocus
                    />
                    <div className="flex gap-1">
                      <Button size="sm" className="flex-1" onClick={onRenameCommit}>{t("workspace.rename")}</Button>
                      <Button size="sm" variant="ghost" onClick={onRenameCancel}>{t("common.cancel")}</Button>
                    </div>
                  </div>
                ) : (
                  /* Row: name button + actions dropdown as siblings in a flex div */
                  <div className="flex items-center gap-1 rounded-md">
                    <button
                      onClick={() => {
                        if (f.is_dir) onNavigateTo(f.name);
                        else onLoadFile(f.name);
                      }}
                      className={`flex flex-1 min-w-0 items-center gap-2 rounded-md px-3 py-2 text-left font-mono text-sm transition-all focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-ring focus-visible:ring-inset ${
                        !f.is_dir && selectedFile === entryPath
                          ? "bg-primary/20 text-primary font-bold shadow-sm"
                          : f.is_dir
                            ? "text-primary/80 hover:bg-primary/10"
                            : CORE_FILES.includes(f.name)
                              ? "text-primary hover:bg-primary/5"
                              : "text-muted-foreground hover:bg-muted/50"
                      }`}
                    >
                      {f.is_dir ? <Folder className="h-4 w-4 shrink-0" /> : <FileIcon className="h-4 w-4 shrink-0" />}
                      <span className="truncate flex-1 min-w-0" title={f.name}>{f.name}</span>
                    </button>

                    {/* Touch-accessible actions dropdown — always visible, not hover-gated */}
                    <DropdownMenu>
                      <DropdownMenuTrigger asChild>
                        <Button
                          variant="ghost"
                          size="icon-sm"
                          aria-label={`${f.name} actions`}
                          className="shrink-0 text-muted-foreground hover:text-foreground"
                        >
                          <MoreVertical className="h-3.5 w-3.5" />
                        </Button>
                      </DropdownMenuTrigger>
                      <DropdownMenuContent align="end" className="min-w-[140px]">
                        <DropdownMenuItem
                          onSelect={() => { onRenameStart(f.name, f.is_dir); onRenameValueChange(f.name); }}
                        >
                          <Pencil className="h-3.5 w-3.5 mr-2" />
                          {t("workspace.rename")}
                        </DropdownMenuItem>
                        {!f.is_dir && (
                          <DropdownMenuItem onSelect={() => onDownload(f.name)}>
                            <Download className="h-3.5 w-3.5 mr-2" />
                            {t("workspace.download")}
                          </DropdownMenuItem>
                        )}
                        <DropdownMenuItem
                          className="text-destructive focus:text-destructive"
                          onSelect={() => {
                            if (f.is_dir) onDeleteRecursive(entryPath);
                            else onDeleteFile(entryPath);
                          }}
                        >
                          <Trash2 className="h-3.5 w-3.5 mr-2" />
                          {f.is_dir ? t("workspace.delete_recursive_title") : t("workspace.delete_file")}
                        </DropdownMenuItem>
                      </DropdownMenuContent>
                    </DropdownMenu>
                  </div>
                )}
              </div>
            );
          })}

          {showNewFolder && (
            <div className="mt-2 p-2 border border-primary/30 rounded-lg bg-primary/5">
              <Input
                placeholder="folder-name"
                className="h-9 font-mono text-sm bg-background border-border focus:border-primary/50 mb-2"
                value={newFolderName}
                onChange={(e) => onNewFolderNameChange(e.target.value)}
                onKeyDown={(e) => {
                  if (e.key === "Enter") onCreateFolder();
                  if (e.key === "Escape") onHideNewFolder();
                }}
                autoFocus
              />
              <div className="flex gap-1">
                <Button size="sm" className="flex-1" onClick={onCreateFolder}>{t("workspace.create")}</Button>
                <Button size="sm" variant="ghost" onClick={onHideNewFolder}>{t("common.cancel")}</Button>
              </div>
            </div>
          )}

          {showNewFile && (
            <div className="mt-2 p-2 border border-primary/30 rounded-lg bg-primary/5">
              <Input
                placeholder="filename.md"
                className="h-9 font-mono text-sm bg-background border-border focus:border-primary/50 mb-2"
                value={newFileName}
                onChange={(e) => onNewFileNameChange(e.target.value)}
                onKeyDown={(e) => {
                  if (e.key === "Enter") onCreateFile();
                  if (e.key === "Escape") onHideNewFile();
                }}
                autoFocus
              />
              <div className="flex gap-1">
                <Button size="sm" className="flex-1" onClick={onCreateFile}>{t("workspace.create")}</Button>
                <Button size="sm" variant="ghost" onClick={onHideNewFile}>{t("common.cancel")}</Button>
              </div>
            </div>
          )}
        </div>
      </div>
    </div>
  );
}
