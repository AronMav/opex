// ── workspace-file-tree.test.tsx ──────────────────────────────────────────────
// Runtime interaction test for WorkspaceFileTree.
// Goals:
//   1. Tree renders file/dir names correctly.
//   2. Name-button calls onLoadFile (file) / onNavigateTo (dir).
//   3. DropdownMenu row actions call correct handlers with correct args.
//   4. New-folder / new-file inline input flow (header buttons + create).
//   5. Drag-drop triggers onUpload.

import React from "react";
import { describe, it, expect, vi, beforeEach } from "vitest";
import "@testing-library/jest-dom/vitest";
import { render, screen, fireEvent } from "@testing-library/react";

import { WorkspaceFileTree } from "@/components/workspace/workspace-file-tree";
import type { FileEntry } from "@/types/api";

// ── Mock: use-translation ────────────────────────────────────────────────────

vi.mock("@/hooks/use-translation", () => ({
  useTranslation: () => ({ t: (key: string) => key, locale: "en" }),
}));

// ── Mock: lucide-react (stub all icons used in this component) ───────────────

vi.mock("lucide-react", () => {
  const Icon = () => null;
  return {
    Folder: Icon,
    FileCode: Icon,
    FileText: Icon,
    FileJson: Icon,
    Image: Icon,
    FilePlus: Icon,
    FolderPlus: Icon,
    CornerDownRight: Icon,
    Pencil: Icon,
    Download: Icon,
    Trash2: Icon,
    Upload: Icon,
    MoreVertical: Icon,
  };
});

// ── Mock: DropdownMenu (Radix portal doesn't work in jsdom) ──────────────────
// Strategy: render a flat structure. DropdownMenuTrigger renders a button,
// DropdownMenuContent renders inline (no portal), DropdownMenuItem renders a
// pressable button. This lets us fireEvent.click the trigger and items directly.

vi.mock("@/components/ui/dropdown-menu", () => {
  // Each DropdownMenu instance gets its own open state.
  // We use a simple controlled pattern: clicking the Trigger toggles open.
  const DropdownMenu = ({ children }: { children: React.ReactNode }) => {
    const [open, setOpen] = React.useState(false);
    return (
      <div data-testid="dropdown-menu" data-open={open ? "true" : "false"}>
        {React.Children.map(children, (child) => {
          if (!React.isValidElement(child)) return child;
          const el = child as React.ReactElement<{ onClick?: () => void; children?: React.ReactNode }>;
          if ((el.type as { displayName?: string }).displayName === "DropdownMenuTrigger") {
            return React.cloneElement(el, {
              onClick: () => setOpen((o) => !o),
            });
          }
          if ((el.type as { displayName?: string }).displayName === "DropdownMenuContent") {
            return open ? el : null;
          }
          return el;
        })}
      </div>
    );
  };

  const DropdownMenuTrigger = ({ children, onClick, asChild }: { children: React.ReactNode; onClick?: () => void; asChild?: boolean }) => {
    // When asChild, clone the single child and attach onClick.
    if (asChild && React.isValidElement(children)) {
      return React.cloneElement(children as React.ReactElement<{ onClick?: () => void }>, { onClick });
    }
    return <button onClick={onClick}>{children}</button>;
  };
  DropdownMenuTrigger.displayName = "DropdownMenuTrigger";

  const DropdownMenuContent = ({ children }: { children: React.ReactNode }) => (
    <div data-testid="dropdown-content">{children}</div>
  );
  DropdownMenuContent.displayName = "DropdownMenuContent";

  const DropdownMenuItem = ({
    children,
    onSelect,
    className,
  }: {
    children: React.ReactNode;
    onSelect?: () => void;
    className?: string;
  }) => (
    <button
      data-testid="dropdown-item"
      className={className}
      onClick={() => onSelect?.()}
    >
      {children}
    </button>
  );

  return { DropdownMenu, DropdownMenuTrigger, DropdownMenuContent, DropdownMenuItem };
});

// ── Fixture ───────────────────────────────────────────────────────────────────

const FILES: FileEntry[] = [
  { name: "SOUL.md", is_dir: false, display: "SOUL.md" },
  { name: "notes.txt", is_dir: false, display: "notes.txt" },
  { name: "docs", is_dir: true, display: "docs" },
];

function makeProps(overrides: Partial<Parameters<typeof WorkspaceFileTree>[0]> = {}) {
  return {
    files: FILES,
    currentPath: "",
    selectedFile: "",
    showNewFolder: false,
    showNewFile: false,
    newFolderName: "",
    newFileName: "",
    renameTarget: null,
    renameValue: "",
    onNavigateTo: vi.fn(),
    onNavigateUp: vi.fn(),
    onLoadFile: vi.fn(),
    onUpload: vi.fn(),
    onShowNewFolder: vi.fn(),
    onHideNewFolder: vi.fn(),
    onShowNewFile: vi.fn(),
    onHideNewFile: vi.fn(),
    onNewFolderNameChange: vi.fn(),
    onNewFileNameChange: vi.fn(),
    onCreateFolder: vi.fn(),
    onCreateFile: vi.fn(),
    onRenameStart: vi.fn(),
    onRenameValueChange: vi.fn(),
    onRenameCommit: vi.fn(),
    onRenameCancel: vi.fn(),
    onDeleteFile: vi.fn(),
    onDeleteRecursive: vi.fn(),
    onDownload: vi.fn(),
    ...overrides,
  };
}

// ── Tests ─────────────────────────────────────────────────────────────────────

describe("WorkspaceFileTree — renders", () => {
  it("renders all file and folder names", () => {
    render(<WorkspaceFileTree {...makeProps()} />);
    expect(screen.getByText("SOUL.md")).toBeInTheDocument();
    expect(screen.getByText("notes.txt")).toBeInTheDocument();
    expect(screen.getByText("docs")).toBeInTheDocument();
  });

  it("does NOT render navigate-up button when currentPath is empty", () => {
    render(<WorkspaceFileTree {...makeProps()} />);
    expect(screen.queryByText("..")).not.toBeInTheDocument();
  });

  it("renders navigate-up button when currentPath is set", () => {
    render(<WorkspaceFileTree {...makeProps({ currentPath: "docs" })} />);
    expect(screen.getByText("..")).toBeInTheDocument();
  });
});

describe("WorkspaceFileTree — name button interactions", () => {
  it("clicking a file name calls onLoadFile with the file name", () => {
    const props = makeProps();
    render(<WorkspaceFileTree {...props} />);
    // Find the button that contains the text "notes.txt"
    const noteBtn = screen.getByText("notes.txt").closest("button");
    expect(noteBtn).not.toBeNull();
    fireEvent.click(noteBtn!);
    expect(props.onLoadFile).toHaveBeenCalledTimes(1);
    expect(props.onLoadFile).toHaveBeenCalledWith("notes.txt");
    expect(props.onNavigateTo).not.toHaveBeenCalled();
  });

  it("clicking a dir name calls onNavigateTo with the dir name", () => {
    const props = makeProps();
    render(<WorkspaceFileTree {...props} />);
    const docsBtn = screen.getByText("docs").closest("button");
    expect(docsBtn).not.toBeNull();
    fireEvent.click(docsBtn!);
    expect(props.onNavigateTo).toHaveBeenCalledTimes(1);
    expect(props.onNavigateTo).toHaveBeenCalledWith("docs");
    expect(props.onLoadFile).not.toHaveBeenCalled();
  });

  it("clicking navigate-up calls onNavigateUp", () => {
    const props = makeProps({ currentPath: "docs" });
    render(<WorkspaceFileTree {...props} />);
    const upBtn = screen.getByText("..").closest("button");
    fireEvent.click(upBtn!);
    expect(props.onNavigateUp).toHaveBeenCalledTimes(1);
  });
});

describe("WorkspaceFileTree — DropdownMenu row actions (file)", () => {
  let props: ReturnType<typeof makeProps>;

  beforeEach(() => {
    props = makeProps();
    render(<WorkspaceFileTree {...props} />);
  });

  function openDropdownFor(fileName: string) {
    // The trigger button has aria-label "{fileName} actions"
    const trigger = screen.getAllByRole("button", { name: `${fileName} actions` })[0];
    expect(trigger).not.toBeNull();
    fireEvent.click(trigger);
  }

  it("opens DropdownMenu for notes.txt and renders action items", () => {
    openDropdownFor("notes.txt");
    // After opening, content renders inline
    const items = screen.getAllByTestId("dropdown-item");
    expect(items.length).toBeGreaterThanOrEqual(3); // Rename, Download, Delete
  });

  it("Rename action calls onRenameStart(name, false) AND onRenameValueChange(name) for a file", () => {
    openDropdownFor("notes.txt");
    // Find Rename item — it has the translation key 'workspace.rename'
    const items = screen.getAllByTestId("dropdown-item");
    const renameItem = items.find((el) => el.textContent?.includes("workspace.rename"));
    expect(renameItem).not.toBeUndefined();
    fireEvent.click(renameItem!);
    expect(props.onRenameStart).toHaveBeenCalledWith("notes.txt", false);
    expect(props.onRenameValueChange).toHaveBeenCalledWith("notes.txt");
  });

  it("Download action calls onDownload(name) for a file", () => {
    openDropdownFor("notes.txt");
    const items = screen.getAllByTestId("dropdown-item");
    const downloadItem = items.find((el) => el.textContent?.includes("workspace.download"));
    expect(downloadItem).not.toBeUndefined();
    fireEvent.click(downloadItem!);
    expect(props.onDownload).toHaveBeenCalledWith("notes.txt");
  });

  it("Delete action calls onDeleteFile with full path (currentPath/name) for a file", () => {
    openDropdownFor("notes.txt");
    const items = screen.getAllByTestId("dropdown-item");
    const deleteItem = items.find((el) => el.textContent?.includes("workspace.delete_file"));
    expect(deleteItem).not.toBeUndefined();
    fireEvent.click(deleteItem!);
    // currentPath is "" so entryPath = "notes.txt"
    expect(props.onDeleteFile).toHaveBeenCalledWith("notes.txt");
    expect(props.onDeleteRecursive).not.toHaveBeenCalled();
  });
});

describe("WorkspaceFileTree — DropdownMenu row actions (directory)", () => {
  let props: ReturnType<typeof makeProps>;

  beforeEach(() => {
    props = makeProps();
    render(<WorkspaceFileTree {...props} />);
  });

  function openDropdownFor(name: string) {
    const trigger = screen.getAllByRole("button", { name: `${name} actions` })[0];
    fireEvent.click(trigger);
  }

  it("Rename action calls onRenameStart(name, true) for a dir", () => {
    openDropdownFor("docs");
    const items = screen.getAllByTestId("dropdown-item");
    const renameItem = items.find((el) => el.textContent?.includes("workspace.rename"));
    expect(renameItem).not.toBeUndefined();
    fireEvent.click(renameItem!);
    expect(props.onRenameStart).toHaveBeenCalledWith("docs", true);
    expect(props.onRenameValueChange).toHaveBeenCalledWith("docs");
  });

  it("Delete action calls onDeleteRecursive (NOT onDeleteFile) for a dir", () => {
    openDropdownFor("docs");
    const items = screen.getAllByTestId("dropdown-item");
    // Dir delete uses workspace.delete_recursive_title key
    const deleteItem = items.find((el) =>
      el.textContent?.includes("workspace.delete_recursive_title")
    );
    expect(deleteItem).not.toBeUndefined();
    fireEvent.click(deleteItem!);
    expect(props.onDeleteRecursive).toHaveBeenCalledWith("docs");
    expect(props.onDeleteFile).not.toHaveBeenCalled();
  });

  it("does NOT show Download item for a dir", () => {
    openDropdownFor("docs");
    const items = screen.getAllByTestId("dropdown-item");
    const downloadItem = items.find((el) => el.textContent?.includes("workspace.download"));
    expect(downloadItem).toBeUndefined();
  });
});

describe("WorkspaceFileTree — entryPath with nested currentPath", () => {
  it("onDeleteFile called with currentPath/name when currentPath is set", () => {
    const props = makeProps({ currentPath: "agents/Bot" });
    render(<WorkspaceFileTree {...props} />);
    const trigger = screen.getAllByRole("button", { name: "notes.txt actions" })[0];
    fireEvent.click(trigger);
    const items = screen.getAllByTestId("dropdown-item");
    const deleteItem = items.find((el) => el.textContent?.includes("workspace.delete_file"));
    fireEvent.click(deleteItem!);
    expect(props.onDeleteFile).toHaveBeenCalledWith("agents/Bot/notes.txt");
  });

  it("onDeleteRecursive called with currentPath/name for dir", () => {
    const props = makeProps({ currentPath: "agents/Bot" });
    render(<WorkspaceFileTree {...props} />);
    const trigger = screen.getAllByRole("button", { name: "docs actions" })[0];
    fireEvent.click(trigger);
    const items = screen.getAllByTestId("dropdown-item");
    const deleteItem = items.find((el) =>
      el.textContent?.includes("workspace.delete_recursive_title")
    );
    fireEvent.click(deleteItem!);
    expect(props.onDeleteRecursive).toHaveBeenCalledWith("agents/Bot/docs");
  });
});

describe("WorkspaceFileTree — new-folder inline flow", () => {
  it("header FolderPlus button calls onHideNewFile then onShowNewFolder", () => {
    const props = makeProps();
    render(<WorkspaceFileTree {...props} />);
    const folderBtn = screen.getByRole("button", { name: "workspace.create_folder" });
    fireEvent.click(folderBtn);
    expect(props.onHideNewFile).toHaveBeenCalledTimes(1);
    expect(props.onShowNewFolder).toHaveBeenCalledTimes(1);
  });

  it("shows folder input when showNewFolder=true, Create button calls onCreateFolder", () => {
    const props = makeProps({ showNewFolder: true, newFolderName: "my-folder" });
    render(<WorkspaceFileTree {...props} />);
    const input = screen.getByPlaceholderText("folder-name");
    expect(input).toBeInTheDocument();
    // Simulate typing
    fireEvent.change(input, { target: { value: "new-dir" } });
    expect(props.onNewFolderNameChange).toHaveBeenCalledWith("new-dir");
    // Create button — use getAllByRole and pick the non-icon button (has text content)
    const createBtns = screen.getAllByRole("button", { name: "workspace.create" });
    const inlineCreate = createBtns.find((btn) => btn.textContent === "workspace.create");
    expect(inlineCreate).not.toBeUndefined();
    fireEvent.click(inlineCreate!);
    expect(props.onCreateFolder).toHaveBeenCalledTimes(1);
  });

  it("Cancel button in folder input calls onHideNewFolder", () => {
    const props = makeProps({ showNewFolder: true });
    render(<WorkspaceFileTree {...props} />);
    const cancelBtn = screen.getByRole("button", { name: "common.cancel" });
    fireEvent.click(cancelBtn);
    expect(props.onHideNewFolder).toHaveBeenCalledTimes(1);
  });

  it("Enter key in folder input calls onCreateFolder", () => {
    const props = makeProps({ showNewFolder: true });
    render(<WorkspaceFileTree {...props} />);
    const input = screen.getByPlaceholderText("folder-name");
    fireEvent.keyDown(input, { key: "Enter" });
    expect(props.onCreateFolder).toHaveBeenCalledTimes(1);
  });

  it("Escape key in folder input calls onHideNewFolder", () => {
    const props = makeProps({ showNewFolder: true });
    render(<WorkspaceFileTree {...props} />);
    const input = screen.getByPlaceholderText("folder-name");
    fireEvent.keyDown(input, { key: "Escape" });
    expect(props.onHideNewFolder).toHaveBeenCalledTimes(1);
  });
});

describe("WorkspaceFileTree — new-file inline flow", () => {
  it("header FilePlus button calls onHideNewFolder then onShowNewFile", () => {
    const props = makeProps();
    render(<WorkspaceFileTree {...props} />);
    const fileBtn = screen.getByRole("button", { name: "workspace.create" });
    fireEvent.click(fileBtn);
    expect(props.onHideNewFolder).toHaveBeenCalledTimes(1);
    expect(props.onShowNewFile).toHaveBeenCalledTimes(1);
  });

  it("shows file input when showNewFile=true, Create button calls onCreateFile", () => {
    const props = makeProps({ showNewFile: true, newFileName: "note.md" });
    render(<WorkspaceFileTree {...props} />);
    const input = screen.getByPlaceholderText("filename.md");
    expect(input).toBeInTheDocument();
    fireEvent.change(input, { target: { value: "new-note.md" } });
    expect(props.onNewFileNameChange).toHaveBeenCalledWith("new-note.md");
    // Two buttons named "workspace.create": icon button in header + inline Create button
    const createBtns = screen.getAllByRole("button", { name: "workspace.create" });
    const inlineCreate = createBtns.find((btn) => btn.textContent === "workspace.create");
    expect(inlineCreate).not.toBeUndefined();
    fireEvent.click(inlineCreate!);
    expect(props.onCreateFile).toHaveBeenCalledTimes(1);
  });

  it("Enter key in file input calls onCreateFile", () => {
    const props = makeProps({ showNewFile: true });
    render(<WorkspaceFileTree {...props} />);
    const input = screen.getByPlaceholderText("filename.md");
    fireEvent.keyDown(input, { key: "Enter" });
    expect(props.onCreateFile).toHaveBeenCalledTimes(1);
  });

  it("Escape key in file input calls onHideNewFile", () => {
    const props = makeProps({ showNewFile: true });
    render(<WorkspaceFileTree {...props} />);
    const input = screen.getByPlaceholderText("filename.md");
    fireEvent.keyDown(input, { key: "Escape" });
    expect(props.onHideNewFile).toHaveBeenCalledTimes(1);
  });
});

describe("WorkspaceFileTree — rename inline flow", () => {
  it("shows rename input for the targeted file", () => {
    const props = makeProps({
      renameTarget: { name: "notes.txt", isDir: false },
      renameValue: "notes.txt",
    });
    render(<WorkspaceFileTree {...props} />);
    const input = screen.getByPlaceholderText("notes.txt");
    expect(input).toBeInTheDocument();
  });

  it("rename Commit button calls onRenameCommit", () => {
    const props = makeProps({
      renameTarget: { name: "notes.txt", isDir: false },
      renameValue: "renamed.txt",
    });
    render(<WorkspaceFileTree {...props} />);
    // Find Rename button (translated key)
    const commitBtn = screen.getByRole("button", { name: "workspace.rename" });
    fireEvent.click(commitBtn);
    expect(props.onRenameCommit).toHaveBeenCalledTimes(1);
  });

  it("rename Cancel button calls onRenameCancel", () => {
    const props = makeProps({
      renameTarget: { name: "notes.txt", isDir: false },
      renameValue: "notes.txt",
    });
    render(<WorkspaceFileTree {...props} />);
    const cancelBtn = screen.getByRole("button", { name: "common.cancel" });
    fireEvent.click(cancelBtn);
    expect(props.onRenameCancel).toHaveBeenCalledTimes(1);
  });

  it("Enter in rename input calls onRenameCommit", () => {
    const props = makeProps({
      renameTarget: { name: "notes.txt", isDir: false },
      renameValue: "notes.txt",
    });
    render(<WorkspaceFileTree {...props} />);
    const input = screen.getByPlaceholderText("notes.txt");
    fireEvent.keyDown(input, { key: "Enter" });
    expect(props.onRenameCommit).toHaveBeenCalledTimes(1);
  });
});

describe("WorkspaceFileTree — drag-drop upload", () => {
  it("drop event with files calls onUpload with the FileList", () => {
    const props = makeProps();
    const { container } = render(<WorkspaceFileTree {...props} />);
    const root = container.firstElementChild as HTMLElement;

    // Build a minimal DataTransfer-like object
    const file = new File(["content"], "upload.txt", { type: "text/plain" });
    const fileList = {
      0: file,
      length: 1,
      item: (i: number) => (i === 0 ? file : null),
      [Symbol.iterator]: function* () { yield file; },
    };

    fireEvent.drop(root, {
      dataTransfer: { files: fileList },
    });

    expect(props.onUpload).toHaveBeenCalledTimes(1);
    expect(props.onUpload).toHaveBeenCalledWith(fileList);
  });

  it("drop event with no files does NOT call onUpload", () => {
    const props = makeProps();
    const { container } = render(<WorkspaceFileTree {...props} />);
    const root = container.firstElementChild as HTMLElement;

    fireEvent.drop(root, {
      dataTransfer: { files: { length: 0 } },
    });

    expect(props.onUpload).not.toHaveBeenCalled();
  });
});
