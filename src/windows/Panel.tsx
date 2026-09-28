import {
  Button,
  Link,
  Menu,
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
  FolderOpenRegular,
  MoreHorizontalRegular,
  SettingsRegular,
  TrayItemRemoveRegular,
} from "@fluentui/react-icons";
import { getCurrentWebview } from "@tauri-apps/api/webview";
import { getCurrentWindow } from "@tauri-apps/api/window";
import { open } from "@tauri-apps/plugin-dialog";
import { useEffect, useLayoutEffect, useRef, useState } from "react";

import { MenuEntries, type MenuEntry } from "../components/Dialogs";
import { FileIcon, Thumbnail } from "../components/FileVisuals";
import { api, events, type DestinationConfig, type Job, type UploadRecord } from "../lib/api";
import { formatOutput, providerName, withoutScheme } from "../lib/format";
import { useDestinations, useHistory, useJobs, useSettings, useTauriEvent } from "../lib/hooks";
import { useI18n } from "../lib/i18n";

export default function Panel() {
  const { t } = useI18n();
  const [destinations] = useDestinations();
  const [jobs] = useJobs();
  const [records] = useHistory();
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

  useEffect(() => {
    const unlisten = getCurrentWebview().onDragDropEvent((event) => {
      const payload = event.payload;
      if (payload.type === "enter" || payload.type === "over") {
        setIsTargeted(true);
      } else if (payload.type === "leave") {
        setIsTargeted(false);
      } else if (payload.type === "drop") {
        setIsTargeted(false);
        if (payload.paths.length > 0) api.uploadFiles(payload.paths);
        // Take focus, so the next click elsewhere closes the panel.
        getCurrentWindow().setFocus();
      }
    });
    return () => {
      unlisten.then((stop) => stop());
    };
  }, []);

  const uploadClipboard = async () => {
    const uploaded = await api.uploadClipboard();
    setNotice(uploaded ? null : t("The clipboard has no file or image to upload."));
  };

  const browse = async () => {
    // The file picker takes focus; that mustn't close the panel.
    await api.setPanelShowingDialog(true);
    try {
      const selection = await open({ multiple: true, directory: false });
      if (Array.isArray(selection) && selection.length > 0) api.uploadFiles(selection);
    } finally {
      await api.setPanelShowingDialog(false);
      getCurrentWindow().setFocus();
    }
  };

  // Shortcuts that work while the panel has focus.
  useEffect(() => {
    const onKey = (event: KeyboardEvent) => {
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
              <>
                {activeJobs.map((job) => (
                  <JobRow key={job.id} job={job} />
                ))}
                {recent.map((record) => (
                  <RecentRow key={record.id} record={record} />
                ))}
              </>
            )}
          </div>
        </>
      )}
    </div>
  );
}

function destinationLabel(destination: DestinationConfig, t: ReturnType<typeof useI18n>["t"]) {
  return `${destination.name} · ${providerName(destination.preset, t)}`;
}

function DestinationPicker({ destinations }: { destinations: DestinationConfig[] }) {
  const { t } = useI18n();
  const current = destinations.find((destination) => destination.isDefault) ?? destinations[0];
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

function JobRow({ job }: { job: Job }) {
  const { t } = useI18n();
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
    </div>
  );
}

function RecentRow({ record }: { record: UploadRecord }) {
  const { t } = useI18n();
  const [settings] = useSettings();
  const copy = (mode: "url" | "markdown" | "html") =>
    api.copyText(formatOutput(record.publicUrl, mode, record.localFilename, settings?.customTemplate));

  const items: MenuEntry[] = [
    { label: t("Copy URL"), onClick: () => copy("url") },
    { label: t("Copy Markdown"), onClick: () => copy("markdown") },
    { label: t("Copy HTML"), onClick: () => copy("html") },
    "divider",
    { label: t("Open in Browser"), onClick: () => api.openUrl(record.publicUrl) },
    { label: t("Show in Library"), onClick: () => api.openWindow("library") },
    "divider",
    { label: t("Delete"), destructive: true, onClick: () => api.deleteRemote([record.id]) },
  ];

  return (
    <div className="recent-row">
      <Thumbnail record={record} size={32} />
      <button type="button" className="recent-text plain-button" onClick={() => copy("url")} title={t("Copy URL")}>
        <Text size={200} className="ellipsis">
          {record.localFilename}
        </Text>
        <Text size={100} className="secondary ellipsis">
          {withoutScheme(record.publicUrl)}
        </Text>
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
    </div>
  );
}
