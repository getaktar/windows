import {
  Button,
  Dialog,
  DialogActions,
  DialogBody,
  DialogContent,
  DialogSurface,
  DialogTitle,
  Field,
  Input,
  Select,
  Switch,
  Text,
} from "@fluentui/react-components";
import { CodeRegular, DeleteRegular, GlobeRegular } from "@fluentui/react-icons";
import { open } from "@tauri-apps/plugin-dialog";
import { useEffect, useState } from "react";

import {
  api,
  errorMessage,
  expiryDurations,
  expiryRulesActive,
  temporaryLinkDurations,
  type AfterUpload,
  type ClipboardPolicy,
  type DestinationConfig,
  type FilterKind,
  type ModifiedPolicy,
  type NotificationPolicy,
  type OnDeletePolicy,
  type Subfolders,
  type WatchedFolder,
  type WatchHook,
} from "../lib/api";
import { durationLabel, temporaryLinkLabel } from "../lib/format";
import { useSettings } from "../lib/hooks";
import { useI18n } from "../lib/i18n";
import { PromptDialog } from "./Dialogs";

const MB = 1024 * 1024;

/** The folder's settings, in the shape `WatchedFolder` keeps them, minus
 * what's typed as text until it's saved. */
interface Draft {
  folder: WatchedFolder;
  customPath: boolean;
  include: string;
  exclude: string;
  minMB: string;
  maxMB: string;
}

function draftOf(folder: WatchedFolder): Draft {
  return {
    folder,
    customPath: folder.pathTemplate !== null,
    include: folder.filter.include.join(", "),
    exclude: folder.filter.exclude.join(", "),
    minMB: folder.filter.minBytes === null ? "" : String(+(folder.filter.minBytes / MB).toFixed(2)),
    maxMB: folder.filter.maxBytes === null ? "" : String(+(folder.filter.maxBytes / MB).toFixed(2)),
  };
}

function patterns(text: string) {
  return text
    .split(/[,\n]/)
    .map((pattern) => pattern.trim())
    .filter(Boolean);
}

function bytes(text: string) {
  const value = Number.parseFloat(text.replace(",", "."));
  return Number.isFinite(value) && value > 0 ? Math.round(value * MB) : null;
}

/** A watched folder's settings: Folder, Upload, Files, changes, After
 * upload, and Automation. `folder` is null while it's closed. */
export function WatchedFolderForm({
  folder,
  destinations,
  onClose,
}: {
  folder: WatchedFolder | null;
  destinations: DestinationConfig[];
  onClose: () => void;
}) {
  const { t } = useI18n();
  const [settings] = useSettings();
  const [draft, setDraft] = useState<Draft | null>(null);
  const [error, setError] = useState<string | null>(null);
  const [saving, setSaving] = useState(false);
  const [addingWebhook, setAddingWebhook] = useState(false);
  const [testResults, setTestResults] = useState<Record<string, string>>({});

  useEffect(() => {
    setDraft(folder ? draftOf(folder) : null);
    setError(null);
    setTestResults({});
  }, [folder]);

  if (!draft) return null;
  const current = draft.folder;
  const update = (change: Partial<WatchedFolder>) => setDraft({ ...draft, folder: { ...current, ...change } });
  const updateFilter = (change: Partial<WatchedFolder["filter"]>) => update({ filter: { ...current.filter, ...change } });
  const updateHook = (id: string, change: Partial<WatchHook> | null) =>
    update({
      hooks: change === null ? current.hooks.filter((hook) => hook.id !== id) : current.hooks.map((hook) => (hook.id === id ? { ...hook, ...change } : hook)),
    });

  const defaultDestination = destinations.find((destination) => destination.isDefault) ?? destinations[0];
  const destination = current.destinationID ? destinations.find((candidate) => candidate.id === current.destinationID) : defaultDestination;
  const rulesActive = expiryRulesActive(settings, destination?.id);
  // Aktar moving the original away isn't the user deleting it.
  const originalStays = current.afterUpload === "keep" || current.afterUpload === "tag";

  /** What's saved: the text fields turned back into values. */
  const finished = (): WatchedFolder => ({
    ...current,
    name: current.name.trim() || current.path,
    pathTemplate: draft.customPath ? current.pathTemplate?.trim() || null : null,
    filter: { ...current.filter, include: patterns(draft.include), exclude: patterns(draft.exclude), minBytes: bytes(draft.minMB), maxBytes: bytes(draft.maxMB) },
  });

  const save = async () => {
    setSaving(true);
    setError(null);
    try {
      await api.saveWatchedFolder(finished());
      onClose();
    } catch (failure) {
      setError(errorMessage(failure));
    } finally {
      setSaving(false);
    }
  };

  const changeFolder = async () => {
    const selection = await open({ directory: true, multiple: false, defaultPath: current.path });
    if (typeof selection === "string") update({ path: selection });
  };

  const addScript = async () => {
    const selection = await open({
      multiple: false,
      directory: false,
      filters: [{ name: t("Scripts"), extensions: ["ps1", "bat", "cmd", "exe"] }],
    });
    if (typeof selection === "string") {
      update({ hooks: [...current.hooks, { id: crypto.randomUUID().toUpperCase(), kind: "script", target: selection, enabled: true }] });
    }
  };

  const test = async (hook: WatchHook) => {
    setTestResults({ ...testResults, [hook.id]: t("Sending…") });
    try {
      await api.testWatchHook(finished(), hook);
      setTestResults((results) => ({ ...results, [hook.id]: t("It worked.") }));
    } catch (failure) {
      setTestResults((results) => ({ ...results, [hook.id]: errorMessage(failure) }));
    }
  };

  return (
    <>
      <Dialog open onOpenChange={(_, data) => !data.open && onClose()}>
        <DialogSurface className="destination-form">
          <form
            onSubmit={(event) => {
              event.preventDefault();
              save();
            }}
          >
            <DialogBody>
              <DialogTitle>{t("Edit Watched Folder")}</DialogTitle>
              <DialogContent className="form-content">
                <Text weight="semibold" className="form-heading">
                  {t("Folder")}
                </Text>
                <div className="form-group">
                  <Field label={t("Name")}>
                    <Input value={current.name} onChange={(_, data) => update({ name: data.value })} />
                  </Field>
                  <Field label={t("Folder")}>
                    <div className="inline-row">
                      <Text className="ellipsis watch-path" title={current.path}>
                        {current.path}
                      </Text>
                      <Button size="small" onClick={changeFolder}>
                        {t("Change…")}
                      </Button>
                    </div>
                  </Field>
                </div>

                <Text weight="semibold" className="form-heading">
                  {t("Upload")}
                </Text>
                <div className="form-group">
                  <Field label={t("Destination")}>
                    <Select
                      value={current.destinationID ?? ""}
                      onChange={(_, data) => update({ destinationID: data.value || null })}
                    >
                      <option value="">{t("Default destination ({0})", defaultDestination?.name ?? "-")}</option>
                      {destinations.map((candidate) => (
                        <option key={candidate.id} value={candidate.id}>
                          {candidate.name}
                        </option>
                      ))}
                    </Select>
                  </Field>
                  <Field
                    label={t("Path")}
                    hint={
                      draft.customPath ? (
                        <>
                          {t("Variables: {year} {month} {day} {date} {time} {filename} {uuid} {random} {ext} {md5} {sha256}")}
                          <br />
                          {"{folder}"}: {t("the watched folder’s name")} · {"{subpath}"}: {t("the file’s folder inside it")}
                        </>
                      ) : (
                        destination?.objectPathTemplate
                      )
                    }
                  >
                    <Select
                      value={draft.customPath ? "custom" : "destination"}
                      onChange={(_, data) => {
                        const custom = data.value === "custom";
                        setDraft({
                          ...draft,
                          customPath: custom,
                          folder: { ...current, pathTemplate: custom ? current.pathTemplate ?? "{folder}/{subpath}/{filename}.{ext}" : null },
                        });
                      }}
                    >
                      <option value="destination">{t("Use destination’s path")}</option>
                      <option value="custom">{t("Custom")}</option>
                    </Select>
                    {draft.customPath && (
                      <Input
                        value={current.pathTemplate ?? ""}
                        spellCheck={false}
                        onChange={(_, data) => update({ pathTemplate: data.value })}
                      />
                    )}
                  </Field>
                  <Field label={t("Link")}>
                    <Select
                      value={current.temporaryLink === null ? "" : String(current.temporaryLink)}
                      onChange={(_, data) =>
                        update({ temporaryLink: data.value === "" ? null : data.value === "public" ? "public" : Number(data.value) })
                      }
                    >
                      <option value="">{t("Destination default")}</option>
                      <option value="public">{t("Public")}</option>
                      {temporaryLinkDurations.map((seconds) => (
                        <option key={seconds} value={String(seconds)}>
                          {temporaryLinkLabel(seconds, t)}
                        </option>
                      ))}
                    </Select>
                  </Field>
                  <Field
                    label={t("Delete after")}
                    hint={rulesActive ? undefined : t("Only once auto-delete is set up for the destination.")}
                  >
                    <Select
                      value={current.expiryDays === null ? "" : String(current.expiryDays)}
                      onChange={(_, data) => update({ expiryDays: data.value === "" ? null : Number(data.value) })}
                    >
                      <option value="">{t("Destination default")}</option>
                      <option value="0">{t("Never")}</option>
                      {expiryDurations.map((days) => (
                        <option key={days} value={String(days)}>
                          {durationLabel(days, t)}
                        </option>
                      ))}
                    </Select>
                  </Field>
                </div>

                <Text weight="semibold" className="form-heading">
                  {t("Files")}
                </Text>
                <div className="form-group">
                  <Field label={t("Subfolders")}>
                    <Select value={current.subfolders} onChange={(_, data) => update({ subfolders: data.value as Subfolders })}>
                      <option value="ignore">{t("Ignore subfolders")}</option>
                      <option value="keepStructure">{t("Include, keep folder structure")}</option>
                      <option value="flatten">{t("Include, upload flat")}</option>
                    </Select>
                  </Field>
                  <Field label={t("Files")}>
                    <Select value={current.filter.kind} onChange={(_, data) => updateFilter({ kind: data.value as FilterKind })}>
                      <option value="all">{t("All files")}</option>
                      <option value="images">{t("Images")}</option>
                      <option value="videos">{t("Videos")}</option>
                      <option value="screenshots">{t("Screenshots only")}</option>
                      <option value="custom">{t("Custom")}</option>
                    </Select>
                  </Field>
                  {current.filter.kind === "custom" && (
                    <Field label={t("Include patterns")} hint={t("Separated by commas, for example *.png, *.jpg")}>
                      <Input value={draft.include} spellCheck={false} onChange={(_, data) => setDraft({ ...draft, include: data.value })} />
                    </Field>
                  )}
                  <Field label={t("Exclude patterns")} hint={t("Temporary and partly downloaded files are always left out.")}>
                    <Input
                      value={draft.exclude}
                      placeholder="*.psd"
                      spellCheck={false}
                      onChange={(_, data) => setDraft({ ...draft, exclude: data.value })}
                    />
                  </Field>
                  <div className="inline-row">
                    <Field label={t("Minimum size")}>
                      <Input
                        value={draft.minMB}
                        inputMode="decimal"
                        contentAfter={<Text className="secondary">MB</Text>}
                        onChange={(_, data) => setDraft({ ...draft, minMB: data.value })}
                      />
                    </Field>
                    <Field label={t("Maximum size")}>
                      <Input
                        value={draft.maxMB}
                        inputMode="decimal"
                        contentAfter={<Text className="secondary">MB</Text>}
                        onChange={(_, data) => setDraft({ ...draft, maxMB: data.value })}
                      />
                    </Field>
                  </div>
                  <Switch
                    checked={current.includeCloudOnly}
                    label={t("Upload online-only files (downloads them first)")}
                    onChange={(_, data) => update({ includeCloudOnly: data.checked })}
                  />
                </div>

                <Text weight="semibold" className="form-heading">
                  {t("When a file changes")}
                </Text>
                <div className="form-group">
                  <Select value={current.modified} onChange={(_, data) => update({ modified: data.value as ModifiedPolicy })}>
                    <option value="ignore">{t("Ignore changes")}</option>
                    <option value="uploadAgain">{t("Upload again with a new link")}</option>
                    <option value="overwrite">{t("Replace the uploaded file (keeps the link)")}</option>
                  </Select>
                </div>

                <Text weight="semibold" className="form-heading">
                  {t("When a file is deleted")}
                </Text>
                <div className="form-group">
                  <Field
                    hint={
                      originalStays
                        ? current.onDelete === "deleteRemote"
                          ? t("Deleting a file from this folder also deletes its upload. Links to it stop working.")
                          : undefined
                        : t("Only available when the original file stays in the folder.")
                    }
                  >
                    <Select
                      value={originalStays ? current.onDelete : "keep"}
                      disabled={!originalStays}
                      aria-label={t("When a file is deleted")}
                      onChange={(_, data) => update({ onDelete: data.value as OnDeletePolicy })}
                    >
                      <option value="keep">{t("Keep the uploaded file")}</option>
                      <option value="deleteRemote">{t("Delete it from the bucket too")}</option>
                    </Select>
                  </Field>
                  {originalStays && current.onDelete === "deleteRemote" && (
                    <Field hint={t("Aktar asks before it deletes anything from the bucket.")}>
                      <Switch
                        checked={current.confirmDelete}
                        label={t("Ask before deleting")}
                        onChange={(_, data) => update({ confirmDelete: data.checked })}
                      />
                    </Field>
                  )}
                </div>

                <Text weight="semibold" className="form-heading">
                  {t("After upload")}
                </Text>
                <div className="form-group">
                  <Field label={t("Original file")}>
                    <Select value={current.afterUpload} onChange={(_, data) => update({ afterUpload: data.value as AfterUpload })}>
                      <option value="keep">{t("Keep it")}</option>
                      <option value="trash">{t("Move to Recycle Bin")}</option>
                      <option value="moveToUploaded">{t("Move to “Uploaded” subfolder")}</option>
                    </Select>
                  </Field>
                  <Field label={t("Clipboard")}>
                    <Select value={current.clipboard} onChange={(_, data) => update({ clipboard: data.value as ClipboardPolicy })}>
                      <option value="copyLink">{t("Copy the link")}</option>
                      <option value="none">{t("Don’t touch the clipboard")}</option>
                    </Select>
                  </Field>
                  <Field label={t("Notifications")}>
                    <Select
                      value={current.notifications}
                      onChange={(_, data) => update({ notifications: data.value as NotificationPolicy })}
                    >
                      <option value="each">{t("One per file")}</option>
                      <option value="grouped">{t("One per batch")}</option>
                      <option value="failuresOnly">{t("Only failures")}</option>
                    </Select>
                  </Field>
                </div>

                <Text weight="semibold" className="form-heading">
                  {t("Automation")}
                </Text>
                <div className="form-group">
                  <Text size={200} className="secondary">
                    {t("After each upload from this folder, webhooks get its details as JSON, and scripts get them on standard input.")}
                  </Text>
                  {current.hooks.map((hook) => (
                    <div key={hook.id} className="watch-hook">
                      {hook.kind === "webhook" ? <GlobeRegular /> : <CodeRegular />}
                      <div className="watch-hook-text">
                        <Text className="ellipsis" title={hook.target}>
                          {hook.target}
                        </Text>
                        {testResults[hook.id] && (
                          <Text size={200} className="secondary ellipsis" title={testResults[hook.id]}>
                            {testResults[hook.id]}
                          </Text>
                        )}
                      </div>
                      <Switch
                        checked={hook.enabled}
                        aria-label={hook.target}
                        onChange={(_, data) => updateHook(hook.id, { enabled: data.checked })}
                      />
                      <Button size="small" onClick={() => test(hook)}>
                        {t("Test")}
                      </Button>
                      <Button
                        size="small"
                        appearance="subtle"
                        icon={<DeleteRegular />}
                        aria-label={t("Remove")}
                        title={t("Remove")}
                        onClick={() => updateHook(hook.id, null)}
                      />
                    </div>
                  ))}
                  <div className="inline-row">
                    <Button size="small" onClick={() => setAddingWebhook(true)}>
                      {t("Add Webhook…")}
                    </Button>
                    <Button size="small" onClick={addScript}>
                      {t("Add Script…")}
                    </Button>
                  </div>
                </div>
                {error && (
                  <Text size={200} className="text-error">
                    {error}
                  </Text>
                )}
              </DialogContent>
              <DialogActions>
                <Button appearance="secondary" onClick={onClose}>
                  {t("Cancel")}
                </Button>
                <Button appearance="primary" type="submit" disabled={saving}>
                  {t("Save")}
                </Button>
              </DialogActions>
            </DialogBody>
          </form>
        </DialogSurface>
      </Dialog>
      <PromptDialog
        open={addingWebhook}
        title={t("Add Webhook")}
        message={t("Aktar sends a POST request with the upload’s details as JSON after each upload.")}
        label="URL"
        initialValue="https://"
        confirmLabel={t("Add")}
        onCancel={() => setAddingWebhook(false)}
        onConfirm={(url) => {
          setAddingWebhook(false);
          update({ hooks: [...current.hooks, { id: crypto.randomUUID().toUpperCase(), kind: "webhook", target: url.trim(), enabled: true }] });
        }}
      />
    </>
  );
}
