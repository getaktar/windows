import {
  Button,
  Dialog,
  DialogBody,
  DialogSurface,
  DialogTitle,
  DialogActions,
  DialogContent,
  Link,
  Menu,
  MenuItemRadio,
  MenuList,
  MenuPopover,
  MenuTrigger,
  MessageBar,
  MessageBarActions,
  MessageBarBody,
  ProgressBar,
  SearchBox,
  Spinner,
  Text,
} from "@fluentui/react-components";
import {
  AddRegular,
  CheckmarkRegular,
  ChevronDownRegular,
  CopyRegular,
  DismissRegular,
  MoreHorizontalRegular,
  TrayItemAddRegular,
} from "@fluentui/react-icons";
import { open as openDialog } from "@tauri-apps/plugin-dialog";
import { useEffect, useMemo, useRef, useState } from "react";

import { ConfirmDialog, ContextMenu, MenuEntries, type ContextMenuState, type MenuEntry } from "../../components/Dialogs";
import { FileIcon, Thumbnail, useThumbnailURL } from "../../components/FileVisuals";
import { Preview } from "../../components/Preview";
import { api, errorMessage, type Job, type UploadRecord } from "../../lib/api";
import { dayBucket, formatBytes, formatDateTime, formatOutput, formatTime, shortDay } from "../../lib/format";
import { hasTextSelection, useFlag, useHistory, useJobs, useSelection, useSettings } from "../../lib/hooks";
import { useI18n } from "../../lib/i18n";
import { DetailRow, LinkSection } from "./BucketView";

export function HistoryView({ active }: { active: boolean }) {
  const { t, locale } = useI18n();
  const [records] = useHistory();
  const [jobs] = useJobs();
  const [settings] = useSettings();
  const [searchText, setSearchText] = useState("");
  const [destinationFilter, setDestinationFilter] = useState<string | null>(null);
  const [pendingDeletion, setPendingDeletion] = useState<UploadRecord[]>([]);
  const [deleting, setDeleting] = useState<Set<string>>(new Set());
  const [deletionErrors, setDeletionErrors] = useState<Record<string, string>>({});
  const [zoomed, setZoomed] = useState<UploadRecord | null>(null);
  const [contextMenu, setContextMenu] = useState<ContextMenuState | null>(null);
  const searchBox = useRef<HTMLInputElement>(null);

  const activeJobs = jobs.filter((job) => job.state.kind !== "succeeded" && job.state.kind !== "cancelled");

  const availableDestinations = useMemo(() => {
    const seen = new Map<string, string>();
    for (const record of records) if (!seen.has(record.destinationId)) seen.set(record.destinationId, record.destinationName);
    return [...seen.entries()].map(([id, name]) => ({ id, name }));
  }, [records]);

  const filtered = useMemo(() => {
    const query = searchText.trim().toLocaleLowerCase();
    return records.filter(
      (record) =>
        (!destinationFilter || record.destinationId === destinationFilter) &&
        (!query ||
          record.localFilename.toLocaleLowerCase().includes(query) ||
          record.publicUrl.toLocaleLowerCase().includes(query) ||
          record.objectKey.toLocaleLowerCase().includes(query)),
    );
  }, [records, searchText, destinationFilter]);

  const ids = useMemo(() => filtered.map((record) => record.id), [filtered]);
  const selection = useSelection(ids);
  const selectedRecords = filtered.filter((record) => selection.selected.has(record.id));

  // Select the newest upload when nothing is selected yet.
  const selectionIsEmpty = selection.selected.size === 0;
  const selectRecords = selection.set;
  useEffect(() => {
    if (selectionIsEmpty && ids.length > 0) selectRecords([ids[0]]);
  }, [ids, selectionIsEmpty, selectRecords]);

  const sections = useMemo(() => {
    if (filtered.length <= 1) return filtered.length ? [{ key: "all", title: "", records: filtered }] : [];
    const order: { key: string; title: string; records: UploadRecord[] }[] = [];
    const byKey = new Map<string, (typeof order)[number]>();
    for (const record of filtered) {
      const bucket = dayBucket(record.createdAt, locale, t);
      let section = byKey.get(bucket.key);
      if (!section) {
        section = { key: bucket.key, title: bucket.title, records: [] };
        byKey.set(bucket.key, section);
        order.push(section);
      }
      section.records.push(record);
    }
    return order;
  }, [filtered, locale, t]);

  const copyAll = (targets: UploadRecord[], mode: "url" | "markdown" | "html" | "custom") =>
    api.copyText(
      targets
        .map((record) => formatOutput(record.publicUrl, mode, record.localFilename, settings?.customTemplate, record.mimeType))
        .join("\n"),
    );

  const removeFromHistory = (targets: UploadRecord[]) => api.removeFromHistory(targets.map((record) => record.id));

  const deleteRemote = async (targets: UploadRecord[]) => {
    const before = filtered;
    const anchor = targets.length === 1 ? before.findIndex((record) => record.id === targets[0].id) : -1;
    setDeleting((current) => new Set([...current, ...targets.map((record) => record.id)]));
    setDeletionErrors((current) => {
      const next = { ...current };
      targets.forEach((record) => delete next[record.id]);
      return next;
    });
    // A failed call (not a failed deletion) fails every record in it.
    const failures = await api
      .deleteRemote(targets.map((record) => record.id))
      .catch((error) => Object.fromEntries(targets.map((record) => [record.id, errorMessage(error)])) as Record<string, string>);
    setDeleting((current) => new Set([...current].filter((id) => !targets.some((record) => record.id === id))));
    setDeletionErrors((current) => ({ ...current, ...failures }));
    // Keep the selection where it was: the next upload in the list.
    if (targets.length === 1 && anchor >= 0 && !failures[targets[0].id]) {
      const remaining = before.filter((record) => record.id !== targets[0].id);
      if (remaining.length > 0) selection.set([remaining[Math.min(anchor, remaining.length - 1)].id]);
    }
  };

  const recordMenu = (record: UploadRecord): MenuEntry[] => {
    const targets =
      selection.selected.has(record.id) && selection.selected.size > 1 ? selectedRecords : [record];
    if (targets.length > 1) {
      return [
        { label: t("Copy {0} URLs", targets.length), onClick: () => copyAll(targets, "url") },
        { label: t("Copy {0} as Markdown", targets.length), onClick: () => copyAll(targets, "markdown") },
        "divider",
        { label: t("Delete {0} Remote Files…", targets.length), destructive: true, onClick: () => setPendingDeletion(targets) },
      ];
    }
    return [
      { label: t("Copy URL"), onClick: () => copyAll([record], "url") },
      { label: t("Copy Markdown"), onClick: () => copyAll([record], "markdown") },
      { label: t("Copy HTML"), onClick: () => copyAll([record], "html") },
      "divider",
      { label: t("Open in Browser"), onClick: () => api.openUrl(record.publicUrl) },
      { label: t("Reveal Details"), onClick: () => selection.set([record.id]) },
      "divider",
      { label: t("Delete Remote File…"), destructive: true, onClick: () => setPendingDeletion([record]) },
      { label: t("Remove from History"), destructive: true, onClick: () => removeFromHistory([record]) },
    ];
  };

  const chooseAndUpload = async () => {
    const paths = await openDialog({ multiple: true, directory: false });
    if (Array.isArray(paths) && paths.length > 0) api.uploadFiles(paths);
  };

  useEffect(() => {
    if (!active) return;
    const onKey = (event: KeyboardEvent) => {
      const typing = (event.target as HTMLElement | null)?.closest("input, textarea");
      if (event.ctrlKey && event.key.toLowerCase() === "f") {
        searchBox.current?.focus();
      } else if (event.ctrlKey && event.key.toLowerCase() === "o") {
        chooseAndUpload();
      } else if (typing || document.querySelector("[role='dialog'], [role='alertdialog'], [role='menu']")) {
        return;
      } else if (event.key === "ArrowDown" || event.key === "ArrowUp") {
        const id = selection.move(event.key === "ArrowDown" ? 1 : -1);
        if (id) document.getElementById(`record-${id}`)?.scrollIntoView({ block: "nearest" });
      } else if (event.ctrlKey && event.key.toLowerCase() === "c" && selectedRecords.length === 1 && !hasTextSelection()) {
        api.copyText(selectedRecords[0].publicUrl);
      } else if (event.ctrlKey && event.key.toLowerCase() === "a") {
        selection.set(ids);
      } else if (event.key === "Delete" && selectedRecords.length > 0) {
        setPendingDeletion(selectedRecords);
      } else if (event.key === "Enter" && selectedRecords.length === 1) {
        api.openUrl(selectedRecords[0].publicUrl);
      } else {
        return;
      }
      event.preventDefault();
    };
    window.addEventListener("keydown", onKey);
    return () => window.removeEventListener("keydown", onKey);
  });

  const deleteTitle =
    pendingDeletion.length > 1
      ? t("Delete {0} files?", pendingDeletion.length)
      : t("Delete “{0}” from {1}?", pendingDeletion[0]?.localFilename ?? "", pendingDeletion[0]?.destinationName ?? "");

  const filterLabel = destinationFilter
    ? t("Destination: {0}", availableDestinations.find((destination) => destination.id === destinationFilter)?.name ?? "")
    : t("Destination: All");

  const isEmpty = records.length === 0 && activeJobs.length === 0;

  return (
    <div className="library-content-and-detail" hidden={!active}>
      <section className="library-list">
        <div className="toolbar">
          <Text weight="semibold" size={400} className="toolbar-title">
            {t("Library")}
          </Text>
          <Button appearance="subtle" icon={<AddRegular />} onClick={chooseAndUpload}>
            {t("Upload")}
          </Button>
        </div>
        {isEmpty ? (
          <EmptyLibrary onUpload={chooseAndUpload} />
        ) : (
          <>
            <div className="search-row">
              <SearchBox
                ref={searchBox}
                className="search-box"
                placeholder={t("Search uploads")}
                value={searchText}
                onChange={(_, data) => setSearchText(data.value)}
              />
            </div>
            {availableDestinations.length > 1 && (
              <div className="filter-row">
                <Menu
                  checkedValues={{ destination: [destinationFilter ?? ""] }}
                  onCheckedValueChange={(_, data) => setDestinationFilter(data.checkedItems[0] || null)}
                >
                  <MenuTrigger disableButtonEnhancement>
                    <Button size="small" appearance="transparent" icon={<ChevronDownRegular />} iconPosition="after">
                      {filterLabel}
                    </Button>
                  </MenuTrigger>
                  <MenuPopover>
                    <MenuList>
                      <MenuItemRadio name="destination" value="">
                        {t("All Destinations")}
                      </MenuItemRadio>
                      {availableDestinations.map((destination) => (
                        <MenuItemRadio key={destination.id} name="destination" value={destination.id}>
                          {destination.name}
                        </MenuItemRadio>
                      ))}
                    </MenuList>
                  </MenuPopover>
                </Menu>
              </div>
            )}
            <div className="list-scroll">
              <div className="list" role="listbox" aria-multiselectable>
                {activeJobs.map((job) => (
                  <ActiveUploadRow key={job.id} job={job} />
                ))}
                {filtered.length === 0 && records.length > 0 ? (
                  <div className="centered no-results">
                    <Text className="secondary">{t("No matching uploads")}</Text>
                    <Link onClick={() => setSearchText("")}>{t("Clear Search")}</Link>
                  </div>
                ) : (
                  sections.map((section) => (
                    <div key={section.key} role="group" aria-label={section.title || undefined}>
                      {section.title && (
                        <Text size={200} weight="semibold" className="section-header secondary">
                          {section.title}
                        </Text>
                      )}
                      {section.records.map((record) => (
                        <RecordRow
                          key={record.id}
                          record={record}
                          selected={selection.selected.has(record.id)}
                          deleting={deleting.has(record.id)}
                          subtitle={`${record.destinationName} · ${shortDay(record.createdAt, locale, t)}, ${formatTime(record.createdAt, locale)}`}
                          onClick={(event) => selection.click(record.id, event)}
                          onContextMenu={(event) => {
                            event.preventDefault();
                            if (!selection.selected.has(record.id)) selection.set([record.id]);
                            setContextMenu({ x: event.clientX, y: event.clientY, items: recordMenu(record) });
                          }}
                        />
                      ))}
                    </div>
                  ))
                )}
              </div>
            </div>
          </>
        )}
      </section>

      <section className="library-detail">
        {selectedRecords.length > 1 ? (
          <MultiSelection
            records={selectedRecords}
            onCopy={(mode) => copyAll(selectedRecords, mode)}
            onDelete={() => setPendingDeletion(selectedRecords)}
          />
        ) : selectedRecords.length === 1 ? (
          <UploadDetail
            key={selectedRecords[0].id}
            record={selectedRecords[0]}
            customTemplate={settings?.customTemplate}
            deleting={deleting.has(selectedRecords[0].id)}
            deletionError={deletionErrors[selectedRecords[0].id] ?? null}
            onZoom={() => setZoomed(selectedRecords[0])}
            onDelete={() => setPendingDeletion([selectedRecords[0]])}
            onRetryDeletion={() => deleteRemote([selectedRecords[0]])}
            onRemove={() => removeFromHistory([selectedRecords[0]])}
          />
        ) : isEmpty ? (
          <EmptyLibrary onUpload={chooseAndUpload} />
        ) : (
          <div className="centered">
            <Text className="secondary">{t("Select an upload to view its details")}</Text>
          </div>
        )}
      </section>

      <ContextMenu state={contextMenu} onClose={() => setContextMenu(null)} />
      <ConfirmDialog
        open={pendingDeletion.length > 0}
        title={deleteTitle}
        message={
          pendingDeletion.length > 1
            ? t("The remote files will be removed and their links may stop working. This can’t be undone.")
            : t("The remote file will be removed and its link may stop working. This can’t be undone.")
        }
        confirmLabel={pendingDeletion.length > 1 ? t("Delete Remote Files") : t("Delete Remote File")}
        destructive
        onCancel={() => setPendingDeletion([])}
        onConfirm={() => {
          const targets = pendingDeletion;
          setPendingDeletion([]);
          deleteRemote(targets);
        }}
      />
      <ZoomedPreview record={zoomed} onClose={() => setZoomed(null)} />
    </div>
  );
}

function EmptyLibrary({ onUpload }: { onUpload: () => void }) {
  const { t } = useI18n();
  return (
    <div className="centered message">
      <TrayItemAddRegular fontSize={40} className="secondary" />
      <Text size={500} weight="semibold">
        {t("Your uploads will appear here")}
      </Text>
      <Button appearance="primary" onClick={onUpload}>
        {t("Upload File…")}
      </Button>
    </div>
  );
}

function RecordRow(props: {
  record: UploadRecord;
  selected: boolean;
  deleting: boolean;
  subtitle: string;
  onClick: (event: React.MouseEvent) => void;
  onContextMenu: (event: React.MouseEvent) => void;
}) {
  return (
    <div
      id={`record-${props.record.id}`}
      role="option"
      aria-selected={props.selected}
      tabIndex={-1}
      className={`row record-row ${props.selected ? "row-selected" : ""} ${props.deleting ? "row-busy" : ""}`}
      onClick={props.onClick}
      onDoubleClick={() => api.openUrl(props.record.publicUrl)}
      onContextMenu={props.onContextMenu}
    >
      <Thumbnail record={props.record} size={44} />
      <span className="row-text">
        <Text className="ellipsis" title={props.record.localFilename}>
          {props.record.localFilename}
        </Text>
        <Text size={200} className="secondary ellipsis">
          {props.subtitle}
        </Text>
      </span>
      {props.deleting && <Spinner size="extra-tiny" />}
    </div>
  );
}

function ActiveUploadRow({ job }: { job: Job }) {
  const { t } = useI18n();
  const failed = job.state.kind === "failed";
  return (
    <div className="row record-row">
      <div className="thumb thumb-icon" style={{ width: 44, height: 44, borderRadius: 8 }}>
        <FileIcon filename={job.filename} size={20} />
      </div>
      <span className="row-text">
        <Text className="ellipsis">{job.filename}</Text>
        {job.state.kind === "uploading" ? (
          <ProgressBar value={job.state.progress > 0 ? job.state.progress : undefined} />
        ) : job.state.kind === "waiting" ? (
          <Text size={200} className="secondary">
            {t("Waiting…")}
          </Text>
        ) : job.state.kind === "failed" ? (
          <span className="inline-row">
            <Text size={200} className="text-error ellipsis" title={job.state.message}>
              {job.state.message}
            </Text>
            <Link onClick={() => api.retryJob(job.id)}>{t("Retry")}</Link>
          </span>
        ) : null}
      </span>
      <Button
        size="small"
        appearance="subtle"
        icon={<DismissRegular />}
        aria-label={failed ? t("Remove") : t("Cancel")}
        title={failed ? t("Remove") : t("Cancel")}
        onClick={() => (failed ? api.dismissJob(job.id) : api.cancelJob(job.id))}
      />
    </div>
  );
}

function UploadDetail(props: {
  record: UploadRecord;
  customTemplate?: string;
  deleting: boolean;
  deletionError: string | null;
  onZoom: () => void;
  onDelete: () => void;
  onRetryDeletion: () => void;
  onRemove: () => void;
}) {
  const { t, locale } = useI18n();
  const { record } = props;
  const [copied, flashCopied] = useFlag();
  const thumbnail = useThumbnailURL(record);
  const copy = (mode: "markdown" | "html" | "custom") =>
    api.copyText(formatOutput(record.publicUrl, mode, record.localFilename, props.customTemplate, record.mimeType));
  const copyURL = () => {
    api.copyText(record.publicUrl);
    flashCopied();
  };

  const menu: MenuEntry[] = [
    {
      submenu: t("Copy As"),
      items: [
        { label: "Markdown", onClick: () => copy("markdown") },
        { label: "HTML", onClick: () => copy("html") },
        { label: t("Custom"), onClick: () => copy("custom") },
      ],
    },
    { label: t("Copy Object Key"), onClick: () => api.copyText(record.objectKey) },
    "divider",
    { label: t("Open in Browser"), onClick: () => api.openUrl(record.publicUrl) },
    "divider",
    props.deleting
      ? { label: t("Deleting…"), disabled: true, onClick: () => {} }
      : { label: t("Delete Remote File…"), destructive: true, onClick: props.onDelete },
    { label: t("Remove from History"), destructive: true, onClick: props.onRemove },
  ];

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
              <MenuEntries items={menu} />
            </MenuList>
          </MenuPopover>
        </Menu>
      </div>
      <div className="detail-scroll">
        <header className="detail-header">
          <Text size={500} weight="semibold" className="selectable break">
            {record.localFilename}
          </Text>
          <Text className="secondary">
            {t("{0} · Uploaded {1}", record.destinationName, formatDateTime(record.createdAt, locale))}
          </Text>
        </header>
        <Preview
          url={record.publicUrl}
          filename={record.localFilename}
          mimeType={record.mimeType}
          browserURL={record.publicUrl}
          placeholder={thumbnail}
          onZoom={props.onZoom}
        />
        {props.deletionError && (
          <MessageBar intent="warning">
            <MessageBarBody>{props.deletionError}</MessageBarBody>
            <MessageBarActions>
              <Button size="small" onClick={props.onRetryDeletion}>
                {t("Try Again")}
              </Button>
            </MessageBarActions>
          </MessageBar>
        )}
        <LinkSection url={record.publicUrl} copied={copied} onCopy={copyURL} />
        <section className="detail-section">
          <Text weight="semibold" className="secondary">
            {t("Details")}
          </Text>
          <DetailRow label={t("Destination")} value={record.destinationName} />
          <DetailRow label={t("File size")} value={formatBytes(record.byteSize, locale)} />
          <DetailRow
            label={t("Uploaded")}
            value={formatDateTime(record.createdAt, locale)}
            tooltip={formatDateTime(record.createdAt, locale, "full")}
          />
        </section>
      </div>
    </div>
  );
}

function MultiSelection(props: {
  records: UploadRecord[];
  onCopy: (mode: "url" | "markdown") => void;
  onDelete: () => void;
}) {
  const { t } = useI18n();
  return (
    <div className="multi-selection">
      <Text size={500} weight="semibold">
        {t("{0} items selected", props.records.length)}
      </Text>
      <div className="thumb-grid">
        {props.records.map((record) => (
          <div key={record.id} className="thumb-grid-item">
            <Thumbnail record={record} size={96} />
            <Text size={100} className="ellipsis">
              {record.localFilename}
            </Text>
          </div>
        ))}
      </div>
      <div className="button-row">
        <Button onClick={() => props.onCopy("url")}>{t("Copy URLs")}</Button>
        <Button onClick={() => props.onCopy("markdown")}>{t("Copy Markdown")}</Button>
        <Button className="destructive-outline" onClick={props.onDelete}>
          {t("Delete Remote Files…")}
        </Button>
      </div>
    </div>
  );
}

function ZoomedPreview({ record, onClose }: { record: UploadRecord | null; onClose: () => void }) {
  const { t } = useI18n();
  return (
    <Dialog open={record !== null} onOpenChange={(_, data) => !data.open && onClose()}>
      <DialogSurface className="zoom-surface">
        <DialogBody>
          <DialogTitle>{record?.localFilename}</DialogTitle>
          <DialogContent className="zoom-content checkerboard">
            {record && <img src={record.publicUrl} alt="" draggable={false} />}
          </DialogContent>
          <DialogActions>
            <Button appearance="primary" onClick={onClose}>
              {t("Done")}
            </Button>
          </DialogActions>
        </DialogBody>
      </DialogSurface>
    </Dialog>
  );
}
