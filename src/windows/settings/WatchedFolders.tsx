import {
  Button,
  Dialog,
  DialogActions,
  DialogBody,
  DialogContent,
  DialogSurface,
  DialogTitle,
  Menu,
  MenuDivider,
  MenuItem,
  MenuList,
  MenuPopover,
  MenuTrigger,
  MessageBar,
  MessageBarActions,
  MessageBarBody,
  Switch,
  Text,
} from "@fluentui/react-components";
import {
  AddRegular,
  ChevronDownRegular,
  EyeRegular,
  FolderRegular,
  MoreHorizontalRegular,
  ScreenshotRegular,
} from "@fluentui/react-icons";
import { message, open } from "@tauri-apps/plugin-dialog";
import { useEffect, useState } from "react";

import { ConfirmDialog } from "../../components/Dialogs";
import { Card, CardRow, SettingsPage, SettingsSection, ToggleRow } from "../../components/SettingsLayout";
import { WatchedFolderForm } from "../../components/WatchedFolderForm";
import {
  api,
  errorMessage,
  type DestinationConfig,
  type FolderCheck,
  type WatchedFolder,
  type WatchedFolderInfo,
  type WatchOverview,
} from "../../lib/api";
import { formatDateTime, formatTime } from "../../lib/format";
import { useDestinations, useWatched } from "../../lib/hooks";
import { useI18n, type Translate } from "../../lib/i18n";

/** "Watched Folders" in Settings. `pendingPath` is a folder to add, from
 * File Explorer's "Watch with Aktar". */
export function WatchedFoldersSettings({ pendingPath, onPendingHandled }: { pendingPath: string | null; onPendingHandled: () => void }) {
  const { t, locale } = useI18n();
  const [overview] = useWatched();
  const [destinations] = useDestinations();
  const [editing, setEditing] = useState<WatchedFolder | null>(null);
  const [asking, setAsking] = useState<FolderCheck | null>(null);
  const [removing, setRemoving] = useState<WatchedFolderInfo | null>(null);
  const [resetting, setResetting] = useState<WatchedFolderInfo | null>(null);
  const [launchAtLogin, setLaunchAtLogin] = useState<boolean | null>(null);

  useEffect(() => {
    api.getLaunchAtLogin().then(setLaunchAtLogin).catch(() => setLaunchAtLogin(null));
  }, []);

  const showError = (error: unknown) => message(errorMessage(error), { kind: "error" });

  /** Checks the folder, asks about the files already in it, adds it, and
   * opens its form. */
  const startAdding = async (path: string) => {
    try {
      const check = await api.checkWatchFolder(path);
      if (check.existingFiles > 0) setAsking(check);
      else setEditing(await api.addWatchedFolder(check.path, false));
    } catch (error) {
      showError(error);
    }
  };

  const finishAdding = async (uploadExisting: boolean) => {
    const check = asking;
    setAsking(null);
    if (!check) return;
    try {
      setEditing(await api.addWatchedFolder(check.path, uploadExisting));
    } catch (error) {
      showError(error);
    }
  };

  useEffect(() => {
    if (!pendingPath) return;
    onPendingHandled();
    startAdding(pendingPath);
  }, [pendingPath]);

  const pickFolder = async () => {
    const selection = await open({ directory: true, multiple: false });
    if (typeof selection === "string") startAdding(selection);
  };

  const addScreenshots = async () => {
    try {
      setEditing(await api.addScreenshotsFolder());
    } catch (error) {
      showError(error);
    }
  };

  if (!overview) return null;
  const folders = overview.folders;

  return (
    <SettingsPage title={t("Watched Folders")} subtitle={t("Upload files the moment they land in a folder.")}>
      {folders.length > 0 && (
        <SettingsSection title={t("Watching")}>
          <Card>
            <CardRow title={watchingStatus(overview, t, locale)}>
              <PauseControl overview={overview} />
            </CardRow>
            <ToggleRow
              title={t("Pause on battery power")}
              subtitle={t("Wait until this PC is plugged in.")}
              checked={overview.pauseOnBattery}
              onChange={(value) => api.setWatchPauseConditions(value, null)}
            />
            <ToggleRow
              title={t("Pause on metered networks")}
              subtitle={t("Wait for a connection that isn’t metered, like your home Wi-Fi.")}
              checked={overview.pauseOnMetered}
              onChange={(value) => api.setWatchPauseConditions(null, value)}
            />
          </Card>
        </SettingsSection>
      )}

      <LargeBatchBanners folders={folders} />

      {folders.length === 0 ? (
        <div className="empty-state">
          <EyeRegular fontSize={36} className="secondary" />
          <Text weight="semibold" size={400}>
            {t("No watched folders yet")}
          </Text>
          <Text className="secondary" align="center">
            {t("Upload files the moment they land in a folder.")}
          </Text>
          <div className="inline-row">
            <Button appearance="primary" icon={<ScreenshotRegular />} onClick={addScreenshots}>
              {t("Upload Screenshots Automatically")}
            </Button>
            <Button icon={<AddRegular />} onClick={pickFolder}>
              {t("Add Folder…")}
            </Button>
          </div>
        </div>
      ) : (
        <>
          <SettingsSection title={t("Folders")}>
            <Card>
              {folders.map((folder) => (
                <FolderRow
                  key={folder.id}
                  folder={folder}
                  destinations={destinations}
                  onEdit={() => setEditing(folder)}
                  onReset={() => setResetting(folder)}
                  onRemove={() => setRemoving(folder)}
                />
              ))}
            </Card>
          </SettingsSection>
          <div>
            <Button icon={<AddRegular />} onClick={pickFolder}>
              {t("Add Folder…")}
            </Button>
          </div>
        </>
      )}

      {launchAtLogin === false && (
        <MessageBar intent="info">
          <MessageBarBody>{t("Turn on Open at Login so watching continues after a restart")}</MessageBarBody>
          <MessageBarActions>
            <Button
              size="small"
              onClick={() =>
                api
                  .setLaunchAtLogin(true)
                  .then(setLaunchAtLogin)
                  .catch((error) => showError(error))
              }
            >
              {t("Turn On")}
            </Button>
          </MessageBarActions>
        </MessageBar>
      )}

      <WatchedFolderForm folder={editing} destinations={destinations} onClose={() => setEditing(null)} />

      <Dialog open={asking !== null} onOpenChange={(_, data) => !data.open && finishAdding(false)}>
        <DialogSurface>
          <DialogBody>
            <DialogTitle>
              {asking?.existingFiles === 1
                ? t("This folder already has 1 file. Upload it now?")
                : t("This folder already has {0} files. Upload them now?", asking?.existingFiles ?? 0)}
            </DialogTitle>
            <DialogContent>
              {t("Either way, Aktar uploads every new file that lands in “{0}” from now on.", asking?.name ?? "")}
            </DialogContent>
            <DialogActions>
              <Button appearance="secondary" onClick={() => finishAdding(true)}>
                {t("Upload Them")}
              </Button>
              <Button appearance="primary" onClick={() => finishAdding(false)}>
                {t("Skip Existing Files")}
              </Button>
            </DialogActions>
          </DialogBody>
        </DialogSurface>
      </Dialog>

      <ConfirmDialog
        open={removing !== null}
        title={t("Stop watching “{0}”?", removing?.name ?? "")}
        message={t("Its files and what’s already uploaded stay where they are.")}
        confirmLabel={t("Remove")}
        destructive
        onCancel={() => setRemoving(null)}
        onConfirm={() => {
          if (removing) api.removeWatchedFolder(removing.id).catch(showError);
          setRemoving(null);
        }}
      />
      <ConfirmDialog
        open={resetting !== null}
        title={t("Forget what “{0}” uploaded?", resetting?.name ?? "")}
        message={t("Every file in the folder counts as new again, and Aktar asks before uploading many at once.")}
        confirmLabel={t("Reset")}
        onCancel={() => setResetting(null)}
        onConfirm={() => {
          if (resetting) api.resetWatchedFolder(resetting.id).catch(showError);
          setResetting(null);
        }}
      />
    </SettingsPage>
  );
}

/** "Watching 2 folders", "Paused until 14:00", "Paused". */
export function watchingStatus(overview: WatchOverview, t: Translate, locale: string) {
  if (overview.pauseReason === "battery") return t("Paused on battery power");
  if (overview.pauseReason === "metered") return t("Paused on a metered network");
  if (overview.paused) {
    const until = overview.pausedUntil && overview.pausedUntil !== "forever" ? Date.parse(overview.pausedUntil) : NaN;
    if (Number.isNaN(until)) return t("Paused");
    const sameDay = new Date(until).toDateString() === new Date().toDateString();
    return t("Paused until {0}", sameDay ? formatTime(until, locale) : formatDateTime(until, locale));
  }
  const watching = overview.folders.filter((folder) => folder.enabled).length;
  return watching === 1 ? t("Watching 1 folder") : t("Watching {0} folders", watching);
}

/** Pause (For 1 Hour / Until Tomorrow / Until I Resume), or Resume. */
export function PauseControl({ overview, size = "medium" }: { overview: WatchOverview; size?: "small" | "medium" }) {
  const { t } = useI18n();
  if (overview.pauseReason === "user") {
    return (
      <Button size={size} onClick={() => api.resumeWatching()}>
        {t("Resume")}
      </Button>
    );
  }
  return (
    <Menu>
      <MenuTrigger disableButtonEnhancement>
        <Button size={size} icon={<ChevronDownRegular />} iconPosition="after">
          {t("Pause")}
        </Button>
      </MenuTrigger>
      <MenuPopover>
        <MenuList>
          <MenuItem onClick={() => api.pauseWatching(60)}>{t("For 1 Hour")}</MenuItem>
          <MenuItem onClick={() => api.pauseWatching(null, true)}>{t("Until Tomorrow")}</MenuItem>
          <MenuItem onClick={() => api.pauseWatching(null)}>{t("Until I Resume")}</MenuItem>
        </MenuList>
      </MenuPopover>
    </Menu>
  );
}

/** "312 new files in Screenshots", with Upload and Skip; and "312 files
 * were removed from Screenshots", with Delete from Bucket and Keep. */
export function LargeBatchBanners({ folders }: { folders: WatchedFolderInfo[] }) {
  const { t } = useI18n();
  return (
    <>
      {folders
        .filter((folder) => folder.awaitingDeleteConfirmation > 0)
        .map((folder) => (
          <MessageBar key={`${folder.id}-deleted`} intent="warning" className="watch-banner">
            <MessageBarBody>
              {folder.awaitingDeleteConfirmation === 1 && folder.awaitingDeleteName
                ? t("{0} was removed from {1}. Delete it from the bucket too?", folder.awaitingDeleteName, folder.name)
                : t("{0} files were removed from {1}. Delete them from the bucket too?", folder.awaitingDeleteConfirmation, folder.name)}
            </MessageBarBody>
            <MessageBarActions>
              <Button size="small" onClick={() => api.confirmWatchedDeletions(folder.id, true)}>
                {t("Delete from Bucket")}
              </Button>
              <Button size="small" appearance="primary" onClick={() => api.confirmWatchedDeletions(folder.id, false)}>
                {t("Keep Uploaded Files")}
              </Button>
            </MessageBarActions>
          </MessageBar>
        ))}
      {folders
        .filter((folder) => folder.awaitingConfirmation > 0)
        .map((folder) => (
          <MessageBar key={folder.id} intent="warning" className="watch-banner">
            <MessageBarBody>{t("{0} new files in {1}", folder.awaitingConfirmation, folder.name)}</MessageBarBody>
            <MessageBarActions>
              <Button size="small" appearance="primary" onClick={() => api.confirmWatchedBatch(folder.id, true)}>
                {t("Upload")}
              </Button>
              <Button size="small" onClick={() => api.confirmWatchedBatch(folder.id, false)}>
                {t("Skip")}
              </Button>
            </MessageBarActions>
          </MessageBar>
        ))}
    </>
  );
}

/** The chip on a folder's card: what's going on there right now. */
export function folderStatus(folder: WatchedFolderInfo, t: Translate): { text: string; tone: "normal" | "busy" | "warning" } {
  switch (folder.status) {
    case "notFound":
      return { text: t("Folder not found"), tone: "warning" };
    case "accessNeeded":
      return { text: t("Access needed"), tone: "warning" };
    case "error":
      return { text: t("Can’t watch this folder"), tone: "warning" };
    default:
      break;
  }
  // The banner above says what they're waiting for.
  if (folder.awaitingConfirmation > 0) return { text: t("Waiting for {0} files", folder.awaitingConfirmation), tone: "warning" };
  if (folder.failed > 0) return { text: t("{0} failed", folder.failed), tone: "warning" };
  if (folder.uploading > 0) return { text: t("Uploading {0}", folder.uploading), tone: "busy" };
  if (folder.waiting > 0) {
    return { text: folder.waiting === 1 ? t("Waiting for 1 file") : t("Waiting for {0} files", folder.waiting), tone: "busy" };
  }
  if (folder.status === "paused" || folder.status === "disabled") return { text: t("Paused"), tone: "normal" };
  return { text: t("Watching"), tone: "normal" };
}

/** A long path with its middle left out, so both the drive and the
 * folder's own name show. */
function middleTruncated(path: string, max = 56) {
  if (path.length <= max) return path;
  const keep = max - 1;
  return `${path.slice(0, Math.ceil(keep / 2))}…${path.slice(path.length - Math.floor(keep / 2))}`;
}

function FolderRow(props: {
  folder: WatchedFolderInfo;
  destinations: DestinationConfig[];
  onEdit: () => void;
  onReset: () => void;
  onRemove: () => void;
}) {
  const { t, locale } = useI18n();
  const { folder } = props;
  const destination = folder.destinationID
    ? props.destinations.find((candidate) => candidate.id === folder.destinationID)?.name ?? t("Removed destination")
    : t("Default destination");
  const status = folderStatus(folder, t);
  const details = [destination];
  if (folder.lastUploadAt) details.push(t("Last upload {0}", formatDateTime(folder.lastUploadAt, locale)));
  return (
    <CardRow
      icon={folder.preset === "screenshots" ? <ScreenshotRegular fontSize={24} /> : <FolderRegular fontSize={24} />}
      title={
        <span className="inline-row">
          {folder.name}
          <span className={`watch-chip watch-chip-${status.tone}`} title={folder.lastError ?? undefined}>
            {status.text}
          </span>
        </span>
      }
      subtitle={
        <>
          <span className="watch-path" title={folder.path}>
            {middleTruncated(folder.path)}
          </span>
          <br />
          {details.join(" · ")}
          {folder.lastError && (
            <>
              <br />
              <span className="text-warning">{folder.lastError}</span>
            </>
          )}
        </>
      }
    >
      <Switch
        checked={folder.enabled}
        aria-label={folder.name}
        onChange={(_, data) => api.setWatchedFolderEnabled(folder.id, data.checked)}
      />
      <Menu>
        <MenuTrigger disableButtonEnhancement>
          <Button appearance="subtle" icon={<MoreHorizontalRegular />} aria-label={t("More")} />
        </MenuTrigger>
        <MenuPopover>
          <MenuList>
            <MenuItem onClick={props.onEdit}>{t("Edit")}</MenuItem>
            <MenuItem onClick={() => api.showFolder(folder.path).catch((error) => message(errorMessage(error), { kind: "error" }))}>
              {t("Show in Explorer")}
            </MenuItem>
            <MenuDivider />
            <MenuItem disabled={folder.waiting === 0 && folder.awaitingConfirmation === 0} onClick={() => api.uploadWatchedPending(folder.id)}>
              {t("Upload Pending Now")}
            </MenuItem>
            <MenuItem disabled={folder.failed === 0} onClick={() => api.retryWatchedFailed(folder.id)}>
              {t("Retry Failed")}
            </MenuItem>
            <MenuItem onClick={props.onReset}>{t("Reset…")}</MenuItem>
            <MenuDivider />
            <MenuItem className="menu-destructive" onClick={props.onRemove}>
              {t("Remove")}
            </MenuItem>
          </MenuList>
        </MenuPopover>
      </Menu>
    </CardRow>
  );
}
