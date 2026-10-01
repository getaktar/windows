import {
  Button,
  Dialog,
  DialogActions,
  DialogBody,
  DialogContent,
  DialogSurface,
  DialogTitle,
  Input,
  Link,
  Menu,
  MenuDivider,
  MenuItem,
  MenuItemRadio,
  MenuList,
  MenuPopover,
  MenuTrigger,
  ProgressBar,
  Text,
} from "@fluentui/react-components";
import {
  ArrowDownloadRegular,
  ArrowUploadRegular,
  ChevronDownRegular,
  ClipboardPasteRegular,
  DismissRegular,
  FolderOpenRegular,
  MoreHorizontalRegular,
  SettingsRegular,
  TimerRegular,
  TrayItemRemoveRegular,
} from "@fluentui/react-icons";
import { getCurrentWebview } from "@tauri-apps/api/webview";
import { getCurrentWindow } from "@tauri-apps/api/window";
import { ask, message, open } from "@tauri-apps/plugin-dialog";
import { useEffect, useLayoutEffect, useRef, useState } from "react";

import { ConfirmDialog, MenuEntries, type MenuEntry } from "../components/Dialogs";
import { QrCodeDialog } from "../components/QrCodeDialog";
import { temporaryLinkMenu } from "../components/temporaryLinkMenu";
import { ExpiryBadge, FileIcon, Thumbnail } from "../components/FileVisuals";
import {
  api,
  effectiveDeleteAfterDays,
  errorMessage,
  events,
  expiryDurations,
  expiryRulesActive,
  temporaryLinkDurations,
  type DestinationConfig,
  type Job,
  type NameRequest,
  type UploadRecord,
} from "../lib/api";
import {
  durationLabel,
  expiryRulesExplanation,
  formatOutput,
  providerName,
  rulesStatusMessage,
  temporaryLinkLabel,
  withoutScheme,
} from "../lib/format";
import { useDestinations, useHistory, useJobs, useLive, useSettings, useTauriEvent } from "../lib/hooks";
import { useI18n } from "../lib/i18n";

export default function Panel() {
  const { t } = useI18n();
  const [destinations] = useDestinations();
  const [jobs] = useJobs();
  const [records] = useHistory();
  const [settings] = useSettings();
  const [names, refreshNames] = useLive<NameRequest[]>(api.pendingNames, [events.namesChanged], []);
  const [isSettingUpExpiry, setIsSettingUpExpiry] = useState(false);
  const [isTargeted, setIsTargeted] = useState(false);
  const [notice, setNotice] = useState<string | null>(null);
  const root = useRef<HTMLDivElement>(null);

  // The window sizes itself to its content.
  useLayoutEffect(() => {
    const element = root.current;
    if (!element) return;
    const report = () => api.setPanelHeight(Math.ceil(element.getBoundingClientRect().height));
    const observer = new ResizeObserver(report);
    observer.observe(element);
    report();
    return () => observer.disconnect();
  }, []);

  useTauriEvent(events.panelShown, () => setNotice(null));

  const tRef = useRef(t);
  tRef.current = t;

  useEffect(() => {
    const unlisten = getCurrentWebview().onDragDropEvent((event) => {
      const payload = event.payload;
      if (payload.type === "enter" || payload.type === "over") {
        setIsTargeted(true);
      } else if (payload.type === "leave") {
        setIsTargeted(false);
      } else if (payload.type === "drop") {
        setIsTargeted(false);
        // Alt held while dropping: name each file first. The drop itself
        // doesn't say which keys were down, so ask right away.
        api
          .altKeyDown()
          .catch(() => false)
          .then((rename) => uploadDropped(payload.paths, tRef.current, rename))
          .then(setNotice)
          .then(refreshNames);
        // Take focus, so the next click elsewhere closes the panel.
        getCurrentWindow().setFocus();
      }
    });
    return () => {
      unlisten.then((stop) => stop());
    };
  }, []);

  const uploadClipboard = async () => {
    const uploaded = await api.uploadClipboard().catch(() => false);
    setNotice(uploaded ? null : t("The clipboard has no file or image to upload."));
  };

  /** With `rename` (Alt held on Browse), each file is named first. */
  const browse = async (rename = false) => {
    // The file picker takes focus; that mustn't close the panel.
    await api.setPanelShowingDialog(true);
    try {
      const selection = await open({ multiple: true, directory: false });
      if (Array.isArray(selection) && selection.length > 0) setNotice(await uploadDropped(selection, t, rename));
      refreshNames();
    } finally {
      await api.setPanelShowingDialog(false);
      getCurrentWindow().setFocus();
    }
  };

  // Shortcuts that work while the panel has focus.
  useEffect(() => {
    const onKey = (event: KeyboardEvent) => {
      // Escape that closed a menu or dialog is theirs, not the panel's.
      if (event.defaultPrevented || document.querySelector("[role='dialog'], [role='alertdialog'], [role='menu']")) return;
      const typing = (event.target as HTMLElement | null)?.closest("input, textarea");
      if (event.key === "Escape") api.hidePanel();
      else if (event.ctrlKey && event.key.toLowerCase() === "v" && !typing) uploadClipboard();
      else if (event.ctrlKey && event.key === ",") api.openWindow("settings");
      else if (event.ctrlKey && event.key.toLowerCase() === "o") browse();
      else if (event.ctrlKey && event.key.toLowerCase() === "q") api.quitApp();
      else return;
      event.preventDefault();
    };
    window.addEventListener("keydown", onKey);
    return () => window.removeEventListener("keydown", onKey);
  });

  const activeJobs = jobs.filter((job) => job.state.kind !== "succeeded" && job.state.kind !== "cancelled");
  const recent = records.slice(0, 3);
  const currentDestination = defaultDestination(destinations);
  // Off for a destination whose bucket doesn't have the lifecycle rules,
  // whatever the setting says: nothing uploaded there expires.
  const deleteAfterDays = effectiveDeleteAfterDays(settings, currentDestination);
  const rulesActive = expiryRulesActive(settings, currentDestination?.id);

  const setUpExpiry = async () => {
    if (!currentDestination) return;
    setIsSettingUpExpiry(true);
    // Files already in those folders would start expiring with the rules,
    // so that's confirmed first.
    const inUse = await api.expiryPrefixesInUse(currentDestination, null).catch(() => [] as string[]);
    if (inUse.length > 0) {
      await api.setPanelShowingDialog(true);
      let confirmed = false;
      try {
        confirmed = await ask(
          t(
            "{0} already hold files. Once the rules are set up, the bucket deletes them too when they're older than the folder's number of days.",
            inUse.join(", "),
          ),
          {
            title: t("Files already in these folders will be deleted"),
            kind: "warning",
            okLabel: t("Set Up Anyway"),
            cancelLabel: t("Cancel"),
          },
        );
      } finally {
        await api.setPanelShowingDialog(false);
        getCurrentWindow().setFocus();
      }
      if (!confirmed) {
        setIsSettingUpExpiry(false);
        return;
      }
    }
    let failure: string | null = null;
    try {
      // Saved credentials; success updates the settings, which enables
      // the durations.
      const check = await api.setUpExpiryRules(currentDestination, null);
      if (check.status.kind !== "active") failure = rulesStatusMessage(check.status.kind, t);
    } catch (error) {
      failure = errorMessage(error);
    } finally {
      setIsSettingUpExpiry(false);
    }
    if (failure === null) return;
    // The dialog takes focus; that mustn't close the panel.
    await api.setPanelShowingDialog(true);
    try {
      await message(expiryRulesExplanation(t), { title: failure, kind: "warning", okLabel: t("OK") });
    } finally {
      await api.setPanelShowingDialog(false);
      getCurrentWindow().setFocus();
    }
  };

  return (
    <div ref={root} className={`panel ${isTargeted ? "panel-targeted" : ""}`}>
      {destinations.length === 0 ? (
        <div className="panel-empty">
          <Text weight="semibold" size={400}>
            Aktar
          </Text>
          <Text size={200} className="secondary">
            {t("Connect your own S3-compatible storage to start uploading.")}
          </Text>
          <Button appearance="primary" onClick={() => api.openWindow("onboarding")}>
            {t("Connect Storage")}
          </Button>
        </div>
      ) : (
        <>
          <div className="panel-header">
            <Text weight="semibold" size={400}>
              {t("Upload")}
            </Text>
            <Button
              appearance="subtle"
              size="small"
              icon={<SettingsRegular />}
              aria-label={t("Settings")}
              title={t("Settings")}
              onClick={() => api.openWindow("settings")}
            />
          </div>

          <DestinationPicker destinations={destinations} />
          <div className="picker-row">
            <DeleteAfterPicker
              destination={currentDestination}
              days={deleteAfterDays}
              available={rulesActive}
              isSettingUp={isSettingUpExpiry}
              onSetUp={setUpExpiry}
            />
            <LinkPicker destination={currentDestination} />
          </div>

          <div
            className={`dropzone ${isTargeted ? "dropzone-targeted" : ""}`}
            title={t("Hold Alt to rename files before they upload")}
          >
            {isTargeted ? (
              <>
                <ArrowDownloadRegular fontSize={28} className="accent" />
                <Text weight="semibold">{t("Release to upload")}</Text>
              </>
            ) : (
              <>
                <ArrowUploadRegular fontSize={28} className="secondary" />
                <Text weight="semibold">{t("Drop files here")}</Text>
                <div className="dropzone-buttons">
                  <Button size="small" icon={<ClipboardPasteRegular />} onClick={uploadClipboard}>
                    {t("Paste")}
                  </Button>
                  <Button size="small" icon={<FolderOpenRegular />} onClick={(event) => browse(event.altKey)}>
                    {t("Browse")}
                  </Button>
                </div>
              </>
            )}
            {/* Always in view while it's on, so it's never forgotten. */}
            {deleteAfterDays > 0 && (
              <Text size={200} className="dropzone-expiry">
                <TimerRegular />
                {deleteAfterDays === 1 ? t("Deletes after 1 day") : t("Deletes after {0} days", deleteAfterDays)}
              </Text>
            )}
          </div>
          {notice && (
            <Text size={200} className="secondary panel-notice">
              {notice}
            </Text>
          )}

          <div className="recent">
            <div className="recent-header">
              <Text weight="semibold" size={300}>
                {t("Recent uploads")}
              </Text>
              <Link onClick={() => api.openWindow("library")}>{t("See all")}</Link>
            </div>
            {activeJobs.length === 0 && recent.length === 0 ? (
              <div className="recent-empty">
                <TrayItemRemoveRegular fontSize={22} className="secondary" />
                <Text size={200} className="secondary">
                  {t("No uploads yet")}
                </Text>
                <Text size={100} className="secondary">
                  {t("Your recent uploads will appear here.")}
                </Text>
              </div>
            ) : (
              // Scrolls once there are many uploads in flight, so the list
              // never pushes past the bottom of the screen.
              <div className="recent-list">
                {activeJobs.map((job) => (
                  <JobRow key={job.id} job={job} />
                ))}
                {recent.map((record) => (
                  <RecentRow key={record.id} record={record} destinations={destinations} onError={setNotice} />
                ))}
              </div>
            )}
          </div>
        </>
      )}
      <NameDialog request={names[0] ?? null} onResolved={refreshNames} />
    </div>
  );
}

/** "Name This Upload", for each file waiting for its name: dropped with
 * Alt held, or from the "Rename and upload clipboard" shortcut. The name
 * replaces the file's own; the extension stays. */
function NameDialog({ request, onResolved }: { request: NameRequest | null; onResolved: () => void }) {
  const { t } = useI18n();
  const [value, setValue] = useState("");
  const input = useRef<HTMLInputElement>(null);

  useEffect(() => {
    if (!request) return;
    setValue(request.name);
    window.setTimeout(() => {
      input.current?.focus();
      input.current?.select();
    }, 0);
  }, [request?.id]);

  const resolve = (name: string | null) => {
    if (!request) return;
    api
      .resolveName(request.id, name)
      .catch(() => {})
      .then(onResolved);
  };

  return (
    <Dialog open={request !== null} onOpenChange={(_, data) => !data.open && resolve(null)}>
      <DialogSurface className="name-surface">
        <form
          onSubmit={(event) => {
            event.preventDefault();
            resolve(value);
          }}
        >
          <DialogBody>
            <DialogTitle>{t("Name This Upload")}</DialogTitle>
            <DialogContent>
              <Input
                ref={input}
                className="name-input"
                value={value}
                aria-label={t("Name This Upload")}
                contentAfter={request?.extension ? <Text className="secondary">.{request.extension}</Text> : undefined}
                onChange={(_, data) => setValue(data.value)}
              />
            </DialogContent>
            <DialogActions>
              <Button appearance="secondary" onClick={() => resolve(null)}>
                {t("Cancel")}
              </Button>
              <Button appearance="primary" type="submit">
                {t("Upload")}
              </Button>
            </DialogActions>
          </DialogBody>
        </form>
      </DialogSurface>
    </Dialog>
  );
}

/** Queues dropped or picked files and folders, and returns a notice when
 * none of them could be: virtual items (an Outlook attachment, an image
 * dragged out of a browser) come with no file path at all. With no
 * destination, Rust itself explains and opens Welcome. */
async function uploadDropped(paths: string[], t: ReturnType<typeof useI18n>["t"], rename = false): Promise<string | null> {
  const queued = paths.length === 0 ? 0 : await api.uploadFiles(paths, undefined, rename).catch(() => -1);
  if (queued === 0) return t("This item can’t be uploaded. Save it as a file first, then drop the file.");
  return null;
}

function destinationLabel(destination: DestinationConfig, t: ReturnType<typeof useI18n>["t"]) {
  return `${destination.name} · ${providerName(destination.preset, t)}`;
}

function defaultDestination(destinations: DestinationConfig[]): DestinationConfig | undefined {
  return destinations.find((destination) => destination.isDefault) ?? destinations[0];
}

function DestinationPicker({ destinations }: { destinations: DestinationConfig[] }) {
  const { t } = useI18n();
  const current = defaultDestination(destinations);
  return (
    <div className="destination-picker">
      <Text size={200} className="secondary">
        {t("Destination")}
      </Text>
      <Menu
        checkedValues={{ destination: current ? [current.id] : [] }}
        onCheckedValueChange={(_, data) => {
          const id = data.checkedItems[0];
          if (id) api.setDefaultDestination(id);
        }}
      >
        <MenuTrigger disableButtonEnhancement>
          <Button className="destination-button" icon={<ChevronDownRegular />} iconPosition="after">
            <span className="ellipsis">{current ? destinationLabel(current, t) : t("No destination")}</span>
          </Button>
        </MenuTrigger>
        <MenuPopover>
          <MenuList>
            {destinations.map((destination) => (
              <MenuItemRadio key={destination.id} name="destination" value={destination.id}>
                {destinationLabel(destination, t)}
              </MenuItemRadio>
            ))}
          </MenuList>
        </MenuPopover>
      </Menu>
    </div>
  );
}

/** "Delete after", kept for the selected destination: it applies to every
 * upload from the panel, the shortcut, and the tray, not to ones into a
 * chosen bucket folder. The durations are only offered once the
 * destination's bucket has the lifecycle rules; until then the menu offers
 * to set them up, and `days` (the effective value) is 0. */
function DeleteAfterPicker({
  destination,
  days,
  available,
  isSettingUp,
  onSetUp,
}: {
  destination: DestinationConfig | undefined;
  days: number;
  available: boolean;
  isSettingUp: boolean;
  onSetUp: () => void;
}) {
  const { t } = useI18n();
  const canSetUp = destination !== undefined;
  return (
    <div className="destination-picker">
      <Text size={200} className="secondary">
        {t("Delete after")}
      </Text>
      <Menu
        checkedValues={{ deleteAfter: [String(days)] }}
        onCheckedValueChange={(_, data) => {
          const value = Number(data.checkedItems[0]);
          if (destination && available && !Number.isNaN(value)) api.setDestinationExpiry(destination.id, value);
        }}
      >
        <MenuTrigger disableButtonEnhancement>
          <Button
            className="destination-button"
            icon={<ChevronDownRegular />}
            iconPosition="after"
            aria-label={t("Delete after")}
          >
            <span className="ellipsis">{days > 0 ? durationLabel(days, t) : t("Off")}</span>
          </Button>
        </MenuTrigger>
        <MenuPopover>
          <MenuList>
            <MenuItemRadio name="deleteAfter" value="0" disabled={!available}>
              {t("Off")}
            </MenuItemRadio>
            {expiryDurations.map((duration) => (
              <MenuItemRadio key={duration} name="deleteAfter" value={String(duration)} disabled={!available}>
                {durationLabel(duration, t)}
              </MenuItemRadio>
            ))}
            {!available && canSetUp && (
              <>
                <MenuDivider />
                {/* Stays open, so "Setting Up…" shows and the durations
                    turn on in place once it's done. */}
                <MenuItem persistOnClick disabled={isSettingUp} onClick={onSetUp}>
                  {isSettingUp ? t("Setting Up…") : t("Set Up Auto-Delete…")}
                </MenuItem>
              </>
            )}
          </MenuList>
        </MenuPopover>
      </Menu>
    </div>
  );
}

/** Public URL or a temporary link, for the selected destination. */
function LinkPicker({ destination }: { destination: DestinationConfig | undefined }) {
  const { t } = useI18n();
  const current = destination?.temporaryLink ?? null;
  return (
    <div className="destination-picker">
      <Text size={200} className="secondary">
        {t("Link")}
      </Text>
      <Menu
        checkedValues={{ link: [String(current ?? "")] }}
        onCheckedValueChange={(_, data) => {
          const value = data.checkedItems[0];
          if (destination) api.setDestinationLink(destination.id, value ? Number(value) : null);
        }}
      >
        <MenuTrigger disableButtonEnhancement>
          <Button
            className="destination-button"
            icon={<ChevronDownRegular />}
            iconPosition="after"
            disabled={!destination}
            aria-label={t("Link")}
            title={t(
              "Copy the public URL after an upload, or a temporary link that stops working after this long. Temporary links also work for private buckets.",
            )}
          >
            <span className="ellipsis">{temporaryLinkLabel(current, t)}</span>
          </Button>
        </MenuTrigger>
        <MenuPopover>
          <MenuList>
            <MenuItemRadio name="link" value="">
              {temporaryLinkLabel(null, t)}
            </MenuItemRadio>
            {temporaryLinkDurations.map((seconds) => (
              <MenuItemRadio key={seconds} name="link" value={String(seconds)}>
                {temporaryLinkLabel(seconds, t)}
              </MenuItemRadio>
            ))}
          </MenuList>
        </MenuPopover>
      </Menu>
    </div>
  );
}

function JobRow({ job }: { job: Job }) {
  const { t } = useI18n();
  const failed = job.state.kind === "failed";
  return (
    <div className="recent-row">
      <div className="thumb thumb-icon" style={{ width: 32, height: 32, borderRadius: 6 }}>
        <FileIcon filename={job.filename} size={16} />
      </div>
      <div className="recent-text">
        <Text size={200} className="ellipsis">
          {job.filename}
        </Text>
        {job.state.kind === "uploading" || job.state.kind === "waiting" ? (
          <>
            <ProgressBar thickness="medium" value={job.state.kind === "uploading" && job.state.progress > 0 ? job.state.progress : undefined} />
            {job.state.kind === "uploading" && job.state.resuming && (
              <Text size={100} className="secondary">
                {t("Resuming upload…")}
              </Text>
            )}
          </>
        ) : job.state.kind === "failed" ? (
          <div className="inline-row">
            <Text size={100} className="text-error ellipsis" title={job.state.message}>
              {t("Upload failed")}
            </Text>
            <Link className="small-link" onClick={() => api.retryJob(job.id)}>
              {t("Retry")}
            </Link>
          </div>
        ) : null}
      </div>
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

function RecentRow({
  record,
  destinations,
  onError,
}: {
  record: UploadRecord;
  destinations: DestinationConfig[];
  onError: (message: string) => void;
}) {
  const { t } = useI18n();
  const [settings] = useSettings();
  const [confirmingDelete, setConfirmingDelete] = useState(false);
  const [showingQr, setShowingQr] = useState(false);
  const copy = (mode: "url" | "markdown" | "html") =>
    api.copyText(formatOutput(record.publicUrl, mode, record.localFilename, settings?.customTemplate, record.mimeType));

  const deleteRemote = async () => {
    setConfirmingDelete(false);
    try {
      const failures = await api.deleteRemote([record.id]);
      if (failures[record.id]) onError(failures[record.id]);
    } catch (error) {
      onError(errorMessage(error));
    }
  };

  const items: MenuEntry[] = [
    { label: t("Copy URL"), onClick: () => copy("url") },
    { label: t("Copy Markdown"), onClick: () => copy("markdown") },
    { label: t("Copy HTML"), onClick: () => copy("html") },
    temporaryLinkMenu(record, t, onError),
    { label: t("Show QR Code"), onClick: () => setShowingQr(true) },
    "divider",
    { label: t("Open in Browser"), onClick: () => api.openUrl(record.publicUrl) },
    { label: t("Show in Library"), onClick: () => api.openWindow("library") },
    "divider",
    { label: t("Delete Remote File…"), destructive: true, onClick: () => setConfirmingDelete(true) },
  ];

  return (
    <div className="recent-row">
      <Thumbnail record={record} size={32} />
      <button type="button" className="recent-text plain-button" onClick={() => copy("url")} title={t("Copy URL")}>
        <Text size={200} className="ellipsis">
          {record.localFilename}
        </Text>
        <span className="inline-row recent-subtitle">
          <Text size={100} className="secondary ellipsis">
            {withoutScheme(record.publicUrl)}
          </Text>
          <ExpiryBadge expiresAt={record.expiresAt} />
        </span>
      </button>
      <Button size="small" onClick={() => copy("url")}>
        {t("Copy")}
      </Button>
      <Menu>
        <MenuTrigger disableButtonEnhancement>
          <Button size="small" appearance="subtle" icon={<MoreHorizontalRegular />} aria-label={t("More")} />
        </MenuTrigger>
        <MenuPopover>
          <MenuList>
            <MenuEntries items={items} />
          </MenuList>
        </MenuPopover>
      </Menu>
      <ConfirmDialog
        open={confirmingDelete}
        title={t("Delete “{0}” from {1}?", record.localFilename, record.destinationName)}
        message={t("The remote file will be removed and its link may stop working. This can’t be undone.")}
        confirmLabel={t("Delete Remote File")}
        destructive
        onCancel={() => setConfirmingDelete(false)}
        onConfirm={deleteRemote}
      />
      <QrCodeDialog record={showingQr ? record : null} destinations={destinations} onClose={() => setShowingQr(false)} />
    </div>
  );
}
