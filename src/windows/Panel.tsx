import {
  Button,
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
import { message, open } from "@tauri-apps/plugin-dialog";
import { useEffect, useLayoutEffect, useRef, useState } from "react";

import { ConfirmDialog, MenuEntries, type MenuEntry } from "../components/Dialogs";
import { ExpiryBadge, FileIcon, Thumbnail } from "../components/FileVisuals";
import {
  api,
  effectiveDeleteAfterDays,
  errorMessage,
  events,
  expiryDurations,
  expiryRulesActive,
  type DestinationConfig,
  type Job,
  type UploadRecord,
} from "../lib/api";
import {
  durationLabel,
  expiryRulesExplanation,
  formatOutput,
  providerName,
  rulesStatusMessage,
  withoutScheme,
} from "../lib/format";
import { useDestinations, useHistory, useJobs, useSettings, useTauriEvent } from "../lib/hooks";
import { useI18n } from "../lib/i18n";

export default function Panel() {
  const { t } = useI18n();
  const [destinations] = useDestinations();
  const [jobs] = useJobs();
  const [records] = useHistory();
  const [settings] = useSettings();
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
        uploadDropped(payload.paths, tRef.current).then(setNotice);
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

  const browse = async () => {
    // The file picker takes focus; that mustn't close the panel.
    await api.setPanelShowingDialog(true);
    try {
      const selection = await open({ multiple: true, directory: false });
      if (Array.isArray(selection) && selection.length > 0) setNotice(await uploadDropped(selection, t));
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
  const deleteAfterDays = effectiveDeleteAfterDays(settings, currentDestination?.id);
  const rulesActive = expiryRulesActive(settings, currentDestination?.id);

  const setUpExpiry = async () => {
    if (!currentDestination) return;
    setIsSettingUpExpiry(true);
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

          <div className="picker-row">
            <DestinationPicker destinations={destinations} />
            <DeleteAfterPicker
              days={deleteAfterDays}
              available={rulesActive}
              canSetUp={currentDestination !== undefined}
              isSettingUp={isSettingUpExpiry}
              onSetUp={setUpExpiry}
            />
          </div>

          <div className={`dropzone ${isTargeted ? "dropzone-targeted" : ""}`}>
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
                  <Button size="small" icon={<FolderOpenRegular />} onClick={browse}>
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
                  <RecentRow key={record.id} record={record} onError={setNotice} />
                ))}
              </div>
            )}
          </div>
        </>
      )}
    </div>
  );
}

/** Queues dropped or picked files and returns a notice when some of them
 * couldn't be: folders, or virtual items (an Outlook attachment, an image
 * dragged out of a browser) that come with no file path at all. With no
 * destination, Rust itself explains and opens Welcome. */
async function uploadDropped(paths: string[], t: ReturnType<typeof useI18n>["t"]): Promise<string | null> {
  if (paths.length === 0) return t("This item can’t be uploaded. Save it as a file first, then drop the file.");
  const queued = await api.uploadFiles(paths).catch(() => -1);
  if (queued === 0) return t("Only files can be uploaded, not folders.");
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

/** "Delete after": sticky, and applies to every upload from the panel, the
 * shortcut, and the tray, not to ones into a chosen bucket folder. The
 * durations are only offered once the destination's bucket has the
 * lifecycle rules; until then the menu offers to set them up, and `days`
 * (the effective value) is 0. */
function DeleteAfterPicker({
  days,
  available,
  canSetUp,
  isSettingUp,
  onSetUp,
}: {
  days: number;
  available: boolean;
  canSetUp: boolean;
  isSettingUp: boolean;
  onSetUp: () => void;
}) {
  const { t } = useI18n();
  return (
    <div className="destination-picker delete-after-picker">
      <Text size={200} className="secondary">
        {t("Delete after")}
      </Text>
      <Menu
        checkedValues={{ deleteAfter: [String(days)] }}
        onCheckedValueChange={(_, data) => {
          const value = Number(data.checkedItems[0]);
          if (available && !Number.isNaN(value)) api.updateSettings({ deleteAfterDays: value });
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
          <ProgressBar thickness="medium" value={job.state.kind === "uploading" && job.state.progress > 0 ? job.state.progress : undefined} />
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

function RecentRow({ record, onError }: { record: UploadRecord; onError: (message: string) => void }) {
  const { t } = useI18n();
  const [settings] = useSettings();
  const [confirmingDelete, setConfirmingDelete] = useState(false);
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
    </div>
  );
}
