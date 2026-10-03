import {
  Button,
  Menu,
  MenuList,
  MenuPopover,
  MenuTrigger,
  SearchBox,
  Spinner,
  Text,
  ToggleButton,
  Tooltip,
} from "@fluentui/react-components";
import {
  ArrowClockwiseRegular,
  ArrowUpRegular,
  ChevronRightRegular,
  CopyRegular,
  CheckmarkRegular,
  DocumentMultipleRegular,
  FolderAddRegular,
  FolderFilled,
  FolderRegular,
  MoreHorizontalRegular,
  OpenRegular,
  SearchRegular,
  WarningRegular,
  AddRegular,
} from "@fluentui/react-icons";
import { open as openDialog } from "@tauri-apps/plugin-dialog";
import { useEffect, useMemo, useRef, useState, type ReactNode } from "react";

import {
  AlertDialog,
  ConfirmDialog,
  ContextMenu,
  MenuEntries,
  PromptDialog,
  type ContextMenuState,
  type MenuEntry,
} from "../../components/Dialogs";
import { FileIcon, ProviderIcon } from "../../components/FileVisuals";
import { Preview } from "../../components/Preview";
import { api, temporaryLinkDurations, type BucketObject, type DestinationConfig } from "../../lib/api";
import {
  folderDisplayName,
  formatBytes,
  formatDateTime,
  formatOutput,
  nameOfKey,
  parentOfFolder,
  parentOfKey,
  temporaryLinkTitle,
} from "../../lib/format";
import { hasTextSelection, useFlag, useSelection } from "../../lib/hooks";
import { useI18n, type Translate } from "../../lib/i18n";
import type { BucketUpload } from "../Library";
import { MAX_SEARCH_RESULTS, useBucketBrowser, type BucketBrowser } from "./useBucketBrowser";

/** Copy, open, rename, and delete actions for one object, shared by the
 * list's context menu and the detail pane's menu. */
function objectMenu(
  model: BucketBrowser,
  object: BucketObject,
  t: Translate,
  requestMove: () => void,
  requestDelete: () => void,
): MenuEntry[] {
  const name = nameOfKey(object.key);
  const url = model.publicURL(object.key);
  return [
    { label: t("Copy URL"), onClick: () => api.copyText(url) },
    { label: t("Copy Markdown"), onClick: () => api.copyText(formatOutput(url, "markdown", name)) },
    { label: t("Copy HTML"), onClick: () => api.copyText(formatOutput(url, "html", name)) },
    {
      submenu: t("Copy Temporary Link"),
      items: temporaryLinkDurations.map((seconds) => ({
        label: temporaryLinkTitle(seconds, t),
        onClick: () => {
          model
            .temporaryURL(object.key, seconds)
            .then((link) => api.copyText(link))
            .catch((error) => model.setActionError(String(error)));
        },
      })),
    },
    { label: t("Copy Object Key"), onClick: () => api.copyText(object.key) },
    "divider",
    { label: t("Open in Browser"), onClick: () => api.openUrl(url) },
    "divider",
    { label: t("Rename or Move…"), onClick: requestMove },
    { label: t("Delete Remote File…"), destructive: true, onClick: requestDelete },
  ];
}

export function BucketView({
  destination,
  active,
  registerUpload,
}: {
  destination: DestinationConfig;
  active: boolean;
  registerUpload: (id: string, upload: BucketUpload | null) => void;
}) {
  const { t, locale } = useI18n();
  const model = useBucketBrowser(destination);
  const keys = useMemo(() => model.visibleObjects.map((object) => object.key), [model.visibleObjects]);
  const selection = useSelection(keys);
  const [contextMenu, setContextMenu] = useState<ContextMenuState | null>(null);
  const [creatingFolder, setCreatingFolder] = useState(false);
  const [moving, setMoving] = useState<BucketObject | null>(null);
  const [pendingDeletion, setPendingDeletion] = useState<string[]>([]);
  const searchBox = useRef<HTMLInputElement>(null);

  useEffect(() => {
    registerUpload(destination.id, model.upload);
    return () => registerUpload(destination.id, null);
  }, [destination.id, model.upload, registerUpload]);

  useEffect(() => {
    selection.set([]);
  }, [model.prefix, selection.set]);

  const selectedObjects = model.visibleObjects.filter((object) => selection.selected.has(object.key));

  const chooseAndUpload = async () => {
    const paths = await openDialog({ multiple: true, directory: false });
    if (Array.isArray(paths) && paths.length > 0) model.upload(paths);
  };

  const deleteTitle = (targets: string[]) =>
    targets.length > 1
      ? t("Delete {0} files?", targets.length)
      : t("Delete “{0}” from {1}?", nameOfKey(targets[0] ?? ""), destination.name);

  useEffect(() => {
    if (!active) return;
    const onKey = (event: KeyboardEvent) => {
      const typing = (event.target as HTMLElement | null)?.closest("input, textarea");
      if (event.ctrlKey && event.key.toLowerCase() === "f") {
        searchBox.current?.focus();
      } else if (typing || document.querySelector("[role='dialog'], [role='alertdialog'], [role='menu']")) {
        return;
      } else if (event.key === "ArrowDown" || event.key === "ArrowUp") {
        selection.move(event.key === "ArrowDown" ? 1 : -1);
      } else if (event.key === "Delete" && selection.selected.size > 0) {
        setPendingDeletion([...selection.selected]);
      } else if (event.key === "Backspace" && model.parentPrefix !== null) {
        model.open(model.parentPrefix);
      } else if (event.ctrlKey && event.key.toLowerCase() === "c" && selectedObjects.length === 1 && !hasTextSelection()) {
        api.copyText(model.publicURL(selectedObjects[0].key));
      } else if (event.key === "F2" && selectedObjects.length === 1) {
        setMoving(selectedObjects[0]);
      } else {
        return;
      }
      event.preventDefault();
    };
    window.addEventListener("keydown", onKey);
    return () => window.removeEventListener("keydown", onKey);
  });

  const statusText = (() => {
    if (model.isSearchActive) {
      if (model.search.searching) return t("Searching… Scanned: {0}", model.search.scanned);
      const results = model.search.results.length + model.search.folderResults.length;
      if (model.search.results.length >= MAX_SEARCH_RESULTS) return t("Showing the first {0} results", results);
      return t("Results: {0} · Scanned: {1}", results, model.search.scanned);
    }
    return t("Folders: {0} · Files: {1}{2}", model.folders.length, model.objects.length, model.nextToken ? "+" : "");
  })();

  const scopeName =
    model.searchScope === "folder" && model.prefix ? `“${folderDisplayName(model.prefix)}”` : destination.bucket;

  const location = (parent: string) => (parent ? parent.slice(0, -1) : destination.bucket);

  const list = (
    <div className="list" role="listbox" aria-multiselectable>
      {model.visibleFolders.map((folder) => (
        <button
          type="button"
          key={folder}
          className="row folder-row"
          onClick={() => model.open(folder)}
          onContextMenu={(event) => {
            event.preventDefault();
            setContextMenu({
              x: event.clientX,
              y: event.clientY,
              items: [
                { label: t("Open"), onClick: () => model.open(folder) },
                { label: t("Copy Path"), onClick: () => api.copyText(folder) },
              ],
            });
          }}
        >
          <FolderFilled className="folder-icon" fontSize={24} />
          <span className="row-text">
            <Text className="ellipsis">{folderDisplayName(folder)}</Text>
            {model.isSearchActive && <LocationLabel text={location(parentOfFolder(folder))} />}
          </span>
          <ChevronRightRegular className="tertiary" />
        </button>
      ))}
      {model.visibleObjects.map((object) => {
        const busy = model.busyKeys.has(object.key);
        const selected = selection.selected.has(object.key);
        return (
          <div
            key={object.key}
            role="option"
            aria-selected={selected}
            tabIndex={-1}
            className={`row ${selected ? "row-selected" : ""} ${busy ? "row-busy" : ""}`}
            title={object.key}
            onClick={(event) => selection.click(object.key, event)}
            onDoubleClick={() => api.openUrl(model.publicURL(object.key))}
            onContextMenu={(event) => {
              event.preventDefault();
              const targets = selection.selected.has(object.key) && selection.selected.size > 1 ? [...selection.selected] : [object.key];
              if (!selection.selected.has(object.key)) selection.click(object.key, { ctrlKey: false, metaKey: false, shiftKey: false });
              setContextMenu({
                x: event.clientX,
                y: event.clientY,
                items:
                  targets.length > 1
                    ? [
                        {
                          label: t("Copy {0} URLs", targets.length),
                          onClick: () => api.copyText(targets.map((key) => model.publicURL(key)).join("\n")),
                        },
                        "divider",
                        {
                          label: t("Delete {0} Remote Files…", targets.length),
                          destructive: true,
                          onClick: () => setPendingDeletion(targets),
                        },
                      ]
                    : objectMenu(model, object, t, () => setMoving(object), () => setPendingDeletion([object.key])),
              });
            }}
          >
            <span className="row-icon">
              <FileIcon filename={object.key} size={22} />
            </span>
            <span className="row-text">
              <Text className="ellipsis">{nameOfKey(object.key)}</Text>
              <Text size={200} className="secondary ellipsis">
                {formatBytes(object.size, locale)}
                {object.lastModified ? ` · ${formatDateTime(object.lastModified, locale)}` : ""}
              </Text>
              {model.isSearchActive && <LocationLabel text={location(parentOfKey(object.key))} />}
            </span>
            {busy && <Spinner size="extra-tiny" />}
          </div>
        );
      })}
      {model.isSearchActive && model.search.searching && (
        <div className="list-footer">
          <Spinner size="tiny" />
        </div>
      )}
      {!model.isSearchActive && model.nextToken && (
        <div className="list-footer">
          {model.isLoading ? <Spinner size="tiny" /> : <Button onClick={model.loadMore}>{t("Load More")}</Button>}
        </div>
      )}
    </div>
  );

  const content = (() => {
    if (!model.hasLoaded && model.isLoading) return <CenteredSpinner />;
    if (model.loadError && !model.hasLoaded) {
      return (
        <Message
          icon={<WarningRegular />}
          title={t("Couldn’t list this bucket")}
          message={model.loadError}
          action={<Button onClick={model.reload}>{t("Try Again")}</Button>}
        />
      );
    }
    if (model.isSearchActive) {
      if (model.search.error && model.search.results.length === 0) {
        return (
          <Message
            icon={<WarningRegular />}
            title={t("Couldn’t list this bucket")}
            message={model.search.error}
            action={<Button onClick={model.refresh}>{t("Try Again")}</Button>}
          />
        );
      }
      if (model.visibleFolders.length === 0 && model.visibleObjects.length === 0) {
        if (model.search.searching || !model.search.complete) {
          return (
            <div className="centered">
              <Spinner />
              <Text className="secondary">{t("Searching… Scanned: {0}", model.search.scanned)}</Text>
            </div>
          );
        }
        return (
          <Message
            icon={<SearchRegular />}
            title={t("No Results")}
            message={t("Nothing in {0} matches “{1}”.", scopeName, model.searchText)}
          />
        );
      }
      return list;
    }
    if (model.hasLoaded && model.folders.length === 0 && model.objects.length === 0) {
      return (
        <Message
          icon={<FolderRegular />}
          title={t("This folder is empty")}
          message={t("Drop files here to upload them to this folder.")}
        />
      );
    }
    return list;
  })();

  return (
    <div className="library-content-and-detail" hidden={!active}>
      <section className="library-list">
        <div className="toolbar">
          <Text weight="semibold" size={400} className="ellipsis toolbar-title">
            {destination.name}
          </Text>
          <Tooltip content={t("Enclosing Folder")} relationship="label">
            <Button
              appearance="subtle"
              icon={<ArrowUpRegular />}
              disabled={model.parentPrefix === null}
              onClick={() => model.parentPrefix !== null && model.open(model.parentPrefix)}
            />
          </Tooltip>
          <Tooltip content={t("Upload to This Folder")} relationship="label">
            <Button appearance="subtle" icon={<AddRegular />} onClick={chooseAndUpload} />
          </Tooltip>
          <Tooltip content={t("New Folder")} relationship="label">
            <Button appearance="subtle" icon={<FolderAddRegular />} onClick={() => setCreatingFolder(true)} />
          </Tooltip>
          <Tooltip content={t("Refresh")} relationship="label">
            <Button appearance="subtle" icon={<ArrowClockwiseRegular />} disabled={model.isLoading} onClick={model.refresh} />
          </Tooltip>
        </div>
        <div className="search-row">
          <SearchBox
            ref={searchBox}
            className="search-box"
            placeholder={t("Search bucket")}
            value={model.searchText}
            onChange={(_, data) => model.setSearchText(data.value)}
          />
        </div>
        <nav className="breadcrumbs" aria-label={t("Folders")}>
          {model.breadcrumbs.map((crumb, index) => {
            const last = index === model.breadcrumbs.length - 1;
            return (
              <span key={crumb.prefix} className="crumb-wrap">
                {index > 0 && <ChevronRightRegular className="tertiary crumb-separator" />}
                <button
                  type="button"
                  className={`crumb ${last ? "crumb-current" : ""}`}
                  disabled={last}
                  onClick={() => model.open(crumb.prefix)}
                >
                  {index === 0 && <ProviderIcon preset={destination.preset} size={14} />}
                  {crumb.name}
                </button>
              </span>
            );
          })}
        </nav>
        {model.isSearchActive && model.prefix && (
          <div className="scope-bar">
            <Text size={200} className="secondary">
              {t("Search:")}
            </Text>
            <ToggleButton size="small" checked={model.searchScope === "bucket"} onClick={() => model.setSearchScope("bucket")}>
              {t("Entire Bucket")}
            </ToggleButton>
            <ToggleButton size="small" checked={model.searchScope === "folder"} onClick={() => model.setSearchScope("folder")}>
              “{folderDisplayName(model.prefix)}”
            </ToggleButton>
          </div>
        )}
        <div className="list-scroll">{content}</div>
        <div className="status-bar">
          {((model.isLoading && model.hasLoaded) || model.search.searching) && <Spinner size="extra-tiny" />}
          <Text size={200} className="secondary">
            {statusText}
          </Text>
        </div>
      </section>

      <section className="library-detail">
        {selectedObjects.length > 1 ? (
          <BucketMultiSelection model={model} objects={selectedObjects} onDelete={() => setPendingDeletion(selectedObjects.map((o) => o.key))} />
        ) : selectedObjects.length === 1 ? (
          <BucketObjectDetail
            key={selectedObjects[0].key}
            model={model}
            object={selectedObjects[0]}
            onMove={() => setMoving(selectedObjects[0])}
            onDelete={() => setPendingDeletion([selectedObjects[0].key])}
          />
        ) : (
          <div className="centered">
            <Text className="secondary">{t("Select a file to see its details")}</Text>
          </div>
        )}
      </section>

      <ContextMenu state={contextMenu} onClose={() => setContextMenu(null)} />
      <PromptDialog
        open={creatingFolder}
        title={t("New Folder")}
        label={t("Folder name")}
        initialValue=""
        confirmLabel={t("Create")}
        onCancel={() => setCreatingFolder(false)}
        onConfirm={(name) => {
          setCreatingFolder(false);
          model.createFolder(name);
        }}
      />
      <PromptDialog
        open={moving !== null}
        title={t("Rename or Move")}
        message={t("Change the name, or the folder part of the path to move it.")}
        label={t("Path")}
        initialValue={moving?.key ?? ""}
        confirmLabel={t("Save")}
        onCancel={() => setMoving(null)}
        onConfirm={async (target) => {
          const object = moving;
          setMoving(null);
          if (!object) return;
          const newKey = await model.move(object, target);
          if (newKey) selection.set([newKey]);
        }}
      />
      <ConfirmDialog
        open={pendingDeletion.length > 0}
        title={deleteTitle(pendingDeletion)}
        message={
          pendingDeletion.length > 1
            ? t("The remote files will be removed and their links may stop working. This can’t be undone.")
            : t("The remote file will be removed and its link may stop working. This can’t be undone.")
        }
        confirmLabel={pendingDeletion.length > 1 ? t("Delete Remote Files") : t("Delete Remote File")}
        destructive
        onCancel={() => setPendingDeletion([])}
        onConfirm={() => {
          const keys = pendingDeletion;
          setPendingDeletion([]);
          model.deleteKeys(keys);
        }}
      />
      <AlertDialog
        open={model.actionError !== null}
        title={t("Something went wrong")}
        message={model.actionError ?? ""}
        onClose={() => model.setActionError(null)}
      />
    </div>
  );
}

function LocationLabel({ text }: { text: string }) {
  return (
    <Text size={200} className="secondary ellipsis location">
      <FolderRegular /> {text}
    </Text>
  );
}

function CenteredSpinner() {
  return (
    <div className="centered">
      <Spinner />
    </div>
  );
}

export function Message({ icon, title, message, action }: { icon: ReactNode; title: string; message: string; action?: ReactNode }) {
  return (
    <div className="centered message">
      <span className="message-icon">{icon}</span>
      <Text weight="semibold">{title}</Text>
      <Text size={200} className="secondary selectable" align="center">
        {message}
      </Text>
      {action}
    </div>
  );
}

function BucketObjectDetail({
  model,
  object,
  onMove,
  onDelete,
}: {
  model: BucketBrowser;
  object: BucketObject;
  onMove: () => void;
  onDelete: () => void;
}) {
  const { t, locale } = useI18n();
  const [previewURL, setPreviewURL] = useState<string | null>(null);
  const [copied, flashCopied] = useFlag();
  const url = model.publicURL(object.key);
  const name = nameOfKey(object.key);

  useEffect(() => {
    let cancelled = false;
    model.previewURL(object.key).then((next) => !cancelled && setPreviewURL(next));
    return () => {
      cancelled = true;
    };
  }, [object.key, model.previewURL]);

  const copyURL = () => {
    api.copyText(url);
    flashCopied();
  };

  return (
    <div className="detail">
      <div className="detail-toolbar">
        <Button icon={copied ? <CheckmarkRegular /> : <CopyRegular />} onClick={copyURL}>
          {copied ? t("Copied") : t("Copy URL")}
        </Button>
        <Menu>
          <MenuTrigger disableButtonEnhancement>
            <Button appearance="subtle" icon={<MoreHorizontalRegular />} aria-label={t("More")} />
          </MenuTrigger>
          <MenuPopover>
            <MenuList>
              <MenuEntries items={objectMenu(model, object, t, onMove, onDelete)} />
            </MenuList>
          </MenuPopover>
        </Menu>
      </div>
      <div className="detail-scroll">
        <header className="detail-header">
          <Text size={500} weight="semibold" className="selectable break">
            {name}
          </Text>
          <Text className="secondary ellipsis">
            {model.destination.bucket}/{parentOfKey(object.key)}
          </Text>
        </header>
        <Preview url={previewURL} filename={name} browserURL={url} size={object.size} />
        <LinkSection url={url} copied={copied} onCopy={copyURL} />
        <section className="detail-section">
          <Text weight="semibold" className="secondary">
            {t("Details")}
          </Text>
          <DetailRow label={t("File size")} value={formatBytes(object.size, locale)} />
          {object.lastModified && (
            <DetailRow
              label={t("Modified")}
              value={formatDateTime(object.lastModified, locale)}
              tooltip={formatDateTime(object.lastModified, locale, "full")}
            />
          )}
          <DetailRow label={t("Object key")} value={object.key} />
        </section>
      </div>
    </div>
  );
}

export function LinkSection({ url, copied, onCopy }: { url: string; copied: boolean; onCopy: () => void }) {
  const { t } = useI18n();
  return (
    <section className="detail-section">
      <Text weight="semibold" className="secondary">
        {t("Link")}
      </Text>
      <div className="link-line">
        <Text className="ellipsis selectable" title={url}>
          {url}
        </Text>
        <Tooltip content={t("Copy URL")} relationship="label">
          <Button appearance="subtle" size="small" icon={copied ? <CheckmarkRegular /> : <CopyRegular />} onClick={onCopy} />
        </Tooltip>
        <Tooltip content={t("Open in Browser")} relationship="label">
          <Button appearance="subtle" size="small" icon={<OpenRegular />} onClick={() => api.openUrl(url)} />
        </Tooltip>
      </div>
    </section>
  );
}

export function DetailRow({ label, value, tooltip }: { label: string; value: string; tooltip?: string }) {
  return (
    <div className="detail-row" title={tooltip}>
      <Text className="secondary detail-label">{label}</Text>
      <Text className="selectable break">{value}</Text>
    </div>
  );
}

function BucketMultiSelection({ model, objects, onDelete }: { model: BucketBrowser; objects: BucketObject[]; onDelete: () => void }) {
  const { t } = useI18n();
  return (
    <div className="centered multi">
      <DocumentMultipleRegular fontSize={40} className="secondary" />
      <Text size={500} weight="semibold">
        {t("{0} items selected", objects.length)}
      </Text>
      <div className="button-row">
        <Button onClick={() => api.copyText(objects.map((object) => model.publicURL(object.key)).join("\n"))}>
          {t("Copy URLs")}
        </Button>
        <Button
          onClick={() =>
            api.copyText(
              objects.map((object) => formatOutput(model.publicURL(object.key), "markdown", nameOfKey(object.key))).join("\n"),
            )
          }
        >
          {t("Copy Markdown")}
        </Button>
        <Button className="destructive-outline" onClick={onDelete}>
          {t("Delete Remote Files…")}
        </Button>
      </div>
    </div>
  );
}
