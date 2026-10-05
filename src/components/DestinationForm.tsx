import {
  Button,
  Checkbox,
  Dialog,
  DialogActions,
  DialogBody,
  DialogContent,
  DialogSurface,
  DialogTitle,
  Field,
  Input,
  Select,
  Spinner,
  Switch,
  Text,
} from "@fluentui/react-components";
import { useEffect, useRef, useState } from "react";

import {
  api,
  errorMessage,
  expiryDurations,
  fileKinds,
  folderUploadModes,
  imageFormats,
  imageMetadataPolicies,
  imageQualities,
  imageSizes,
  temporaryLinkDurations,
  thumbnailModes,
  type ConnectionResult,
  type DestinationConfig,
  type ExpiryRulesCheck,
  type FileKind,
  type FileRouting,
  type FolderUploadMode,
  type FormRules,
  type ImageFormat,
  type ImageMetadataPolicy,
  type ImageProcessing,
  type OutputMode,
  type ProviderPreset,
  type ThumbnailMode,
  type WatchHook,
} from "../lib/api";
import {
  durationLabel,
  expiryRulesExplanation,
  formatDateTime,
  providerName,
  rulesStatusMessage,
  temporaryLinkLabel,
} from "../lib/format";
import { useSettings } from "../lib/hooks";
import { useI18n, type Translate } from "../lib/i18n";
import { defaultThumbnailPrefix, normalizedThumbnailPrefix, thumbnailPrefixProblem } from "../lib/thumbnails";
import { ConnectionTestResult } from "./ConnectionTestResult";
import { ConfirmDialog } from "./Dialogs";
import { HookEditor } from "./HookEditor";
import { ShortcutRecorder } from "./ShortcutRecorder";

const presets: ProviderPreset[] = ["cloudflareR2", "amazonS3", "minIO", "backblazeB2", "digitalOceanSpaces", "customS3"];

const defaultRegion = (preset: ProviderPreset) => (preset === "amazonS3" ? "us-east-1" : "auto");

/** MinIO deployments commonly run without DNS-based virtual-hosted buckets. */
const defaultForcePathStyle = (preset: ProviderPreset) => preset === "minIO";

const r2Endpoint = (accountID: string) => `https://${accountID}.r2.cloudflarestorage.com`;

/** An extension as "Use for" keeps it: without leading dots, lowercase,
 * 1 to 16 letters and digits. Null when it can't be one. */
function normalizedExtension(raw: string): string | null {
  const extension = raw.trim().replace(/^\.+/, "").toLowerCase();
  return /^[a-z0-9]{1,16}$/.test(extension) ? extension : null;
}

/** "dmg, .zip  tar": the extensions typed, and the ones that can't be one. */
function parseExtensions(text: string): { extensions: string[]; invalid: string[] } {
  const extensions: string[] = [];
  const invalid: string[] = [];
  for (const part of text.split(/[\s,]+/).filter(Boolean)) {
    const extension = normalizedExtension(part);
    if (extension === null) invalid.push(part);
    else if (!extensions.includes(extension)) extensions.push(extension);
  }
  return { extensions, invalid };
}

/** A Cloudflare zone ID: 32 hex digits, as the dashboard shows it. */
const isZoneId = (zone: string) => /^[0-9a-f]{32}$/i.test(zone.trim());

interface Props {
  open: boolean;
  /** The destination being edited, or null to add one. */
  existing: DestinationConfig | null;
  onSaved: (destination: DestinationConfig) => void;
  onCancel: () => void;
}

export function DestinationForm({ open, existing, onSaved, onCancel }: Props) {
  const { t, locale } = useI18n();
  const [preset, setPreset] = useState<ProviderPreset>("cloudflareR2");
  const [name, setName] = useState("");
  const [accountID, setAccountID] = useState("");
  const [endpoint, setEndpoint] = useState("");
  const [region, setRegion] = useState("auto");
  const [accessKeyId, setAccessKeyId] = useState("");
  const [secretAccessKey, setSecretAccessKey] = useState("");
  const [bucket, setBucket] = useState("");
  const [publicBaseURL, setPublicBaseURL] = useState("");
  const [objectPathTemplate, setObjectPathTemplate] = useState("{year}/{month}/{uuid}.{ext}");
  const [forcePathStyle, setForcePathStyle] = useState(false);
  const [settings] = useSettings();
  /** Upload Defaults; null follows Settings > Output. */
  const [outputMode, setOutputMode] = useState<OutputMode | null>(null);
  const [temporaryLink, setTemporaryLink] = useState<number | null>(null);
  const [expiryDays, setExpiryDays] = useState(0);
  const [imageMetadata, setImageMetadata] = useState<ImageMetadataPolicy>("removeLocation");
  const [folderUpload, setFolderUpload] = useState<FolderUploadMode>("zip");
  const [imageFormat, setImageFormat] = useState<ImageFormat>("original");
  const [imageQuality, setImageQuality] = useState<number | null>(null);
  const [imageMaxLongEdge, setImageMaxLongEdge] = useState<number | null>(null);
  const [thumbnailMode, setThumbnailMode] = useState<ThumbnailMode>("local");
  const [thumbnailPrefix, setThumbnailPrefix] = useState(defaultThumbnailPrefix);
  const [useForKinds, setUseForKinds] = useState<FileKind[]>([]);
  const [useForExtensions, setUseForExtensions] = useState("");
  /** This destination's own shortcut; for a new one, set once it's saved. */
  const [shortcut, setShortcut] = useState<string | null>(null);
  const [shortcutError, setShortcutError] = useState<string | null>(null);
  const [shortCache, setShortCache] = useState(false);
  const [cloudflareZoneId, setCloudflareZoneId] = useState("");
  /** Typed here; empty keeps the saved one. */
  const [cloudflareToken, setCloudflareToken] = useState("");
  const [hasSavedToken, setHasSavedToken] = useState(false);
  const [removeToken, setRemoveToken] = useState(false);
  const [tokenCheck, setTokenCheck] = useState<{ kind: "checking" } | { kind: "passed" } | { kind: "failed"; message: string } | null>(null);
  const [hooks, setHooks] = useState<WatchHook[]>([]);
  const [hookError, setHookError] = useState<string | null>(null);
  /** Saving stopped to ask what happens to the thumbnails already in this
   * bucket folder, which the destination would stop using. */
  const [oldThumbnailPrefix, setOldThumbnailPrefix] = useState<string | null>(null);
  const [testResult, setTestResult] = useState<ConnectionResult | null>(null);
  /** Why the test couldn't reach the bucket at all. */
  const [testError, setTestError] = useState<string | null>(null);
  const testOutcome = useRef<HTMLDivElement>(null);

  // The result is at the bottom of a long form: bring it into view.
  useEffect(() => {
    if (testResult || testError) testOutcome.current?.scrollIntoView({ block: "nearest", behavior: "smooth" });
  }, [testResult, testError]);
  const [isTesting, setIsTesting] = useState(false);
  const [isSaving, setIsSaving] = useState(false);
  const [saveError, setSaveError] = useState<string | null>(null);
  const [rulesCheck, setRulesCheck] = useState<ExpiryRulesCheck | null>(null);
  const [isCheckingRules, setIsCheckingRules] = useState(false);
  const [rulesError, setRulesError] = useState<string | null>(null);
  /** The rules result obtained in this form (checked, set up, or turned
   * off), which saving records instead of guessing from what was edited. */
  const [formRules, setFormRules] = useState<FormRules>({ kind: "notChecked" });
  const [isConfirmingTurnOff, setIsConfirmingTurnOff] = useState(false);
  /** tmp/{N}d/ folders that already hold files, while confirming set up. */
  const [prefixesInUse, setPrefixesInUse] = useState<string[] | null>(null);

  useEffect(() => {
    if (!open) return;
    setRulesCheck(null);
    setRulesError(null);
    setFormRules({ kind: "notChecked" });
    setIsConfirmingTurnOff(false);
    setPrefixesInUse(null);
    if (existing) api.expiryRulesStatus(existing.id).then(setRulesCheck).catch(() => {});
    const initialPreset = existing?.preset ?? "cloudflareR2";
    setPreset(initialPreset);
    setName(existing?.name ?? "");
    setAccountID(existing?.accountID ?? "");
    setEndpoint(existing?.endpoint ?? "");
    setRegion(existing?.region ?? defaultRegion(initialPreset));
    setAccessKeyId("");
    setSecretAccessKey("");
    setBucket(existing?.bucket ?? "");
    setPublicBaseURL(existing?.publicBaseURL ?? "");
    setObjectPathTemplate(existing?.objectPathTemplate ?? "{year}/{month}/{uuid}.{ext}");
    setForcePathStyle(existing?.forcePathStyle ?? defaultForcePathStyle(initialPreset));
    setOutputMode(existing?.outputMode ?? null);
    setTemporaryLink(existing?.temporaryLink ?? null);
    setExpiryDays(existing?.expiryDays ?? settings?.deleteAfterDays ?? 0);
    setImageMetadata(existing?.imageMetadata ?? "removeLocation");
    setFolderUpload(existing?.folderUpload ?? "zip");
    setImageFormat(existing?.imageProcessing?.format ?? "original");
    setImageQuality(existing?.imageProcessing?.quality ?? null);
    setImageMaxLongEdge(existing?.imageProcessing?.maxLongEdge ?? null);
    setThumbnailMode(existing?.thumbnails ?? "local");
    setThumbnailPrefix(normalizedThumbnailPrefix(existing?.thumbnailPrefix ?? "") ?? defaultThumbnailPrefix);
    setOldThumbnailPrefix(null);
    setUseForKinds(existing?.useFor?.kinds ?? []);
    setUseForExtensions((existing?.useFor?.extensions ?? []).join(", "));
    setShortcut(existing ? (settings?.destinationShortcuts?.[existing.id] ?? null) : null);
    setShortcutError(null);
    setShortCache(existing?.shortCache ?? false);
    setCloudflareZoneId(existing?.cloudflareZoneId ?? "");
    setCloudflareToken("");
    setRemoveToken(false);
    setTokenCheck(null);
    setHasSavedToken(false);
    if (existing) api.hasCloudflareToken(existing.id).then(setHasSavedToken).catch(() => {});
    setHooks(existing?.hooks ?? []);
    setHookError(null);
    setTestResult(null);
    setTestError(null);
    setSaveError(null);
    // Settings only supply the starting "Delete after" of a new form.
  }, [open, existing]);

  const hasNewCredentials = accessKeyId.trim() !== "" && secretAccessKey !== "";
  const canTest = endpoint.trim() !== "" && bucket.trim() !== "" && (hasNewCredentials || existing !== null);
  const prefixProblem = thumbnailMode === "bucket" ? thumbnailPrefixProblem(thumbnailPrefix, t) : null;
  const invalidExtensions = parseExtensions(useForExtensions).invalid;
  const extensionsProblem =
    invalidExtensions.length > 0
      ? t("Not an extension: {0}. Use letters and digits only, such as dmg or mp4.", invalidExtensions.map((part) => `“${part}”`).join(", "))
      : null;
  const zoneProblem = cloudflareZoneId.trim() !== "" && !isZoneId(cloudflareZoneId) ? t("The Cloudflare zone ID isn’t valid.") : null;
  const canSave =
    name.trim() !== "" &&
    extensionsProblem === null &&
    zoneProblem === null &&
    bucket.trim() !== "" &&
    endpoint.trim() !== "" &&
    publicBaseURL.trim() !== "" &&
    prefixProblem === null &&
    (existing !== null || hasNewCredentials);

  const currentConfig = (): DestinationConfig => ({
    id: existing?.id ?? "",
    name: name.trim(),
    preset,
    accountID: preset === "cloudflareR2" ? accountID.trim() : null,
    endpoint: endpoint.trim(),
    region: region.trim(),
    bucket: bucket.trim(),
    publicBaseURL: publicBaseURL.trim(),
    objectPathTemplate: objectPathTemplate.trim() || "{year}/{month}/{uuid}.{ext}",
    forcePathStyle,
    isDefault: existing?.isDefault ?? false,
    outputMode,
    expiryDays,
    temporaryLink,
    imageMetadata,
    folderUpload,
    imageProcessing: imageProcessingConfig(),
    thumbnails: thumbnailMode,
    thumbnailPrefix: thumbnailPrefixConfig(),
    useFor: useForConfig(),
    shortCache: shortCache || null,
    cloudflareZoneId: isZoneId(cloudflareZoneId) ? cloudflareZoneId.trim().toLowerCase() : null,
    hooks: hooks.length > 0 ? hooks : null,
  });

  /** Null when it claims nothing, so uploads only come here when picked. */
  const useForConfig = (): FileRouting | null => {
    const extensions = parseExtensions(useForExtensions).extensions;
    const kinds = fileKinds.filter((kind) => useForKinds.includes(kind));
    return kinds.length === 0 && extensions.length === 0 ? null : { kinds, extensions };
  };

  /** Sets the destination's shortcut right away; a failure says why and
   * keeps the old one. */
  const changeShortcut = async (accelerator: string | null) => {
    if (!existing) {
      // Set once the destination is saved and has its ID.
      setShortcut(accelerator);
      setShortcutError(null);
      api.setShortcutPaused(false).catch(() => {});
      return;
    }
    try {
      await api.setDestinationShortcut(existing.id, accelerator);
      setShortcut(accelerator);
      setShortcutError(null);
    } catch (error) {
      setShortcutError(errorMessage(error));
      api.setShortcutPaused(false).catch(() => {});
    }
  };

  const checkToken = async () => {
    setTokenCheck({ kind: "checking" });
    try {
      await api.checkCloudflareToken(existing?.id ?? null, cloudflareToken.trim() || null);
      setTokenCheck({ kind: "passed" });
    } catch (error) {
      setTokenCheck({ kind: "failed", message: errorMessage(error) });
    }
  };

  /** What saving does with the Cloudflare token: null keeps it. */
  const tokenChange = (): string | null => (cloudflareToken.trim() ? cloudflareToken.trim() : removeToken ? "" : null);

  /** Kept while thumbnails aren't in the bucket, so switching back finds
   * the same folder; null for the default one. */
  const thumbnailPrefixConfig = (): string | null => {
    const prefix = normalizedThumbnailPrefix(thumbnailPrefix);
    if (!prefix || thumbnailPrefixProblem(thumbnailPrefix, t) !== null) return existing?.thumbnailPrefix ?? null;
    return prefix === defaultThumbnailPrefix ? null : prefix;
  };

  /** None when it's all off, so photos go up untouched. */
  const imageProcessingConfig = (): ImageProcessing | null =>
    imageFormat === "original" && imageQuality === null && imageMaxLongEdge === null
      ? null
      : { format: imageFormat, quality: imageQuality, maxLongEdge: imageMaxLongEdge };

  const credentials = () => (hasNewCredentials ? { accessKeyId, secretAccessKey, sessionToken: null } : null);

  /** The bucket a rules result is about, so saving can tell whether the
   * connection was edited after it. */
  const currentConnection = () => {
    const config = currentConfig();
    return { endpoint: config.endpoint, bucket: config.bucket, region: config.region };
  };

  /** A rules result for the bucket the form pointed at before isn't true of
   * the one it points at now. */
  const connectionEdited = () => {
    setTestResult(null);
    setTestError(null);
    setRulesError(null);
    if (formRules.kind === "checked" || rulesCheck) {
      setFormRules({ kind: "notChecked" });
      setRulesCheck(null);
    }
  };

  const setUpRules = async () => {
    setIsCheckingRules(true);
    setRulesError(null);
    try {
      // Files already in those folders would start expiring with the
      // rules, so that's confirmed first.
      const inUse = await api.expiryPrefixesInUse(currentConfig(), credentials());
      if (inUse.length > 0) {
        setPrefixesInUse(inUse);
        setIsCheckingRules(false);
        return;
      }
    } catch {
      // Setting up reports the same problem, with more to go on.
    }
    await applyRules();
  };

  const test = async () => {
    setIsTesting(true);
    setTestResult(null);
    setTestError(null);
    try {
      setTestResult(await api.testConnection(currentConfig(), credentials()));
    } catch (error) {
      setTestError(errorMessage(error));
    } finally {
      setIsTesting(false);
    }
  };

  const applyRules = async () => {
    setPrefixesInUse(null);
    setIsCheckingRules(true);
    setRulesError(null);
    try {
      const connection = currentConnection();
      const check = await api.setUpExpiryRules(currentConfig(), credentials());
      setRulesCheck(check);
      setFormRules({ kind: "checked", check, connection });
    } catch (error) {
      setRulesError(errorMessage(error));
    } finally {
      setIsCheckingRules(false);
    }
  };

  /** "Turn Off" marks the destination not active; "Turn Off and Remove
   * Rules" also takes Aktar's rules out of the bucket first. */
  const turnOffRules = async (removingRules: boolean) => {
    setIsConfirmingTurnOff(false);
    setIsCheckingRules(true);
    setRulesError(null);
    try {
      const connection = currentConnection();
      if (removingRules) await api.removeExpiryRules(currentConfig(), credentials());
      setRulesCheck(null);
      setFormRules({ kind: "checked", check: null, connection });
    } catch (error) {
      setRulesError(errorMessage(error));
    } finally {
      setIsCheckingRules(false);
    }
  };

  /** `deleteOldThumbnails`: the answer about the bucket folder of
   * thumbnails saving would stop using; asked first when there's one. */
  const save = async (deleteOldThumbnails?: boolean) => {
    setOldThumbnailPrefix(null);
    setIsSaving(true);
    setSaveError(null);
    try {
      const config = currentConfig();
      if (deleteOldThumbnails === undefined && existing) {
        const old = await api.thumbnailCleanupPrefix(config).catch(() => null);
        if (old) {
          setOldThumbnailPrefix(old);
          return;
        }
      }
      const saved = await api.saveDestination(config, credentials(), formRules, deleteOldThumbnails ?? false, tokenChange());
      if (!existing && shortcut) await api.setDestinationShortcut(saved.id, shortcut).catch(() => {});
      onSaved(saved);
    } catch (error) {
      setSaveError(errorMessage(error));
    } finally {
      setIsSaving(false);
    }
  };

  return (
    <Dialog open={open} onOpenChange={(_, data) => !data.open && onCancel()}>
      <DialogSurface className="destination-form">
        <form
          onSubmit={(event) => {
            event.preventDefault();
            if (canSave) void save();
          }}
        >
          <DialogBody>
            <DialogTitle>{existing ? t("Edit Destination") : t("Add Destination")}</DialogTitle>
            <DialogContent className="form-content">
              <div className="form-group">
                <Field label={t("Provider")}>
                  <Select
                    value={preset}
                    onChange={(_, data) => {
                      const next = data.value as ProviderPreset;
                      setPreset(next);
                      setRegion(defaultRegion(next));
                      setForcePathStyle(defaultForcePathStyle(next));
                      connectionEdited();
                    }}
                  >
                    {presets.map((value) => (
                      <option key={value} value={value}>
                        {providerName(value, t)}
                      </option>
                    ))}
                  </Select>
                </Field>
                <Field label={t("Profile Name")} required>
                  <Input value={name} placeholder={t("Production Files")} onChange={(_, data) => setName(data.value)} />
                </Field>
              </div>

              <Text weight="semibold" className="form-heading">
                {t("Connection")}
              </Text>
              <div className="form-group">
                {preset === "cloudflareR2" ? (
                  <Field
                    label={t("Account ID")}
                    hint={endpoint ? endpoint : t("Endpoint is derived from the Account ID.")}
                  >
                    <Input
                      value={accountID}
                      onChange={(_, data) => {
                        setAccountID(data.value);
                        setEndpoint(data.value.trim() ? r2Endpoint(data.value.trim()) : "");
                        connectionEdited();
                      }}
                    />
                  </Field>
                ) : (
                  <Field label={t("Endpoint")} required>
                    <Input value={endpoint} placeholder="s3.example.com" 
                      onChange={(_, data) => {
                        setEndpoint(data.value);
                        connectionEdited();
                      }}
                    />
                  </Field>
                )}
                <Field label={t("Region")}>
                  <Input
                    value={region}
                    onChange={(_, data) => {
                      setRegion(data.value);
                      connectionEdited();
                    }}
                  />
                </Field>
                {preset !== "cloudflareR2" && (
                  <Field
                    hint={t("Turn on for servers without a subdomain for each bucket, such as MinIO or a server reached by IP address.")}
                  >
                    <Switch
                      label={t("Use path-style addressing")}
                      checked={forcePathStyle}
                      onChange={(_, data) => {
                        setForcePathStyle(data.checked);
                        setTestResult(null);
                        setTestError(null);
                      }}
                    />
                  </Field>
                )}
              </div>

              <Text weight="semibold" className="form-heading">
                {t("Credentials")}
              </Text>
              <div className="form-group">
                <Field label={t("Access Key ID")} required={!existing}>
                  <Input
                    value={accessKeyId}
                    placeholder={existing ? t("Unchanged") : undefined}
                    autoComplete="off"
                    spellCheck={false}
                    onChange={(_, data) => setAccessKeyId(data.value)}
                  />
                </Field>
                <Field label={t("Secret Access Key")} required={!existing}>
                  <Input
                    type="password"
                    value={secretAccessKey}
                    placeholder={existing ? t("Unchanged") : undefined}
                    autoComplete="off"
                    onChange={(_, data) => setSecretAccessKey(data.value)}
                  />
                </Field>
              </div>

              <Text weight="semibold" className="form-heading">
                {t("Bucket")}
              </Text>
              <div className="form-group">
                <Field label={t("Bucket")} required>
                  <Input value={bucket} placeholder="screenshots" 
                    spellCheck={false}
                    onChange={(_, data) => {
                      setBucket(data.value);
                      connectionEdited();
                    }}
                  />
                </Field>
                <Field
                  label={t("Public Base URL")}
                  required
                  hint={t("The domain files are served from, for example a custom domain or CDN in front of the bucket.")}
                >
                  <Input
                    value={publicBaseURL}
                    placeholder="img.example.com"
                    spellCheck={false}
                    onChange={(_, data) => {
                      setPublicBaseURL(data.value);
                      setTestResult(null);
                      setTestError(null);
                    }}
                  />
                </Field>
              </div>

              <Text weight="semibold" className="form-heading">
                {t("Object Path")}
              </Text>
              <div className="form-group">
                <Field
                  label={t("Object Path")}
                  hint={
                    <>
                      {t("Variables: {year} {month} {day} {date} {time} {filename} {uuid} {random} {ext} {md5} {sha256}")}
                      <br />
                      {"{md5}"}: {t("MD5 of the file’s contents")} · {"{sha256}"}: {t("SHA-256 of the file’s contents")}
                      <br />
                      {"{folder}"}: {t("the watched folder’s name")} · {"{subpath}"}: {t("the file’s folder inside it")}
                    </>
                  }
                >
                  <Input
                    value={objectPathTemplate}
                    spellCheck={false}
                    onChange={(_, data) => setObjectPathTemplate(data.value)}
                  />
                </Field>
              </div>

              <Text weight="semibold" className="form-heading">
                {t("Auto-delete")}
              </Text>
              <div className="form-group">
                <Text size={200} className="secondary">
                  {t("Uploads with “Delete after” turned on are removed by lifecycle rules in the bucket.")}
                </Text>
                <ExpiryRulesState check={rulesCheck} />
                {rulesError && (
                  <Text size={200} className="text-error">
                    {rulesError}
                  </Text>
                )}
                <div className="inline-row">
                  <Button
                    size="small"
                    disabled={isCheckingRules}
                    icon={isCheckingRules ? <Spinner size="tiny" /> : undefined}
                    onClick={setUpRules}
                  >
                    {isCheckingRules ? t("Checking…") : rulesCheck ? t("Check Again") : t("Set Up")}
                  </Button>
                  {rulesCheck?.status.kind === "active" && (
                    <Button size="small" disabled={isCheckingRules} onClick={() => setIsConfirmingTurnOff(true)}>
                      {t("Turn Off…")}
                    </Button>
                  )}
                  {rulesCheck && (
                    <Text size={200} className="secondary">
                      {t("Last checked {0}", formatDateTime(rulesCheck.checkedAt, locale))}
                    </Text>
                  )}
                </div>
              </div>

              <Text weight="semibold" className="form-heading">
                {t("Upload Defaults")}
              </Text>
              <div className="form-group">
                <Field label={t("Copy as")}>
                  <Select
                    value={outputMode ?? ""}
                    onChange={(_, data) => setOutputMode((data.value || null) as OutputMode | null)}
                  >
                    <option value="">{t("Same as Settings")}</option>
                    <option value="url">URL</option>
                    <option value="markdown">Markdown</option>
                    <option value="html">HTML</option>
                    <option value="custom">{t("Custom")}</option>
                  </Select>
                </Field>
                <Field label={t("Link")}>
                  <Select
                    value={String(temporaryLink ?? "")}
                    onChange={(_, data) => setTemporaryLink(data.value ? Number(data.value) : null)}
                  >
                    <option value="">{temporaryLinkLabel(null, t)}</option>
                    {temporaryLinkDurations.map((seconds) => (
                      <option key={seconds} value={seconds}>
                        {temporaryLinkLabel(seconds, t)}
                      </option>
                    ))}
                  </Select>
                </Field>
                <Field label={t("Delete after")}>
                  <Select
                    value={String(expiryDays)}
                    disabled={rulesCheck?.status.kind !== "active"}
                    onChange={(_, data) => setExpiryDays(Number(data.value))}
                  >
                    <option value="0">{t("Off")}</option>
                    {expiryDurations.map((days) => (
                      <option key={days} value={days}>
                        {durationLabel(days, t)}
                      </option>
                    ))}
                  </Select>
                </Field>
                <Field
                  label={t("Image metadata")}
                  hint={imageMetadata === "keepAll" ? undefined : t("Videos (MOV, MP4) are uploaded with their location.")}
                >
                  <Select value={imageMetadata} onChange={(_, data) => setImageMetadata(data.value as ImageMetadataPolicy)}>
                    {imageMetadataPolicies.map((policy) => (
                      <option key={policy} value={policy}>
                        {imageMetadataLabel(policy, t)}
                      </option>
                    ))}
                  </Select>
                </Field>
                <Field label={t("Folders")}>
                  <Select value={folderUpload} onChange={(_, data) => setFolderUpload(data.value as FolderUploadMode)}>
                    {folderUploadModes.map((mode) => (
                      <option key={mode} value={mode}>
                        {mode === "zip" ? t("Upload as ZIP") : t("Keep folder structure")}
                      </option>
                    ))}
                  </Select>
                </Field>
                <Text size={200} className="secondary">
                  {t(
                    "Applied whenever this destination is picked. Add one destination per kind of file, such as Builds, Logs or Screenshots, each with its own path and defaults. A temporary link stops working after the time you pick and works for private buckets too. Image metadata applies to photos: Remove location drops the GPS position, Remove all also drops the camera, date and other details. A folder is uploaded as one ZIP file, or file by file with its subfolders.",
                  )}
                </Text>
              </div>

              <Text weight="semibold" className="form-heading">
                {t("Use For")}
              </Text>
              <div className="form-group">
                <div className="inline-row wrap">
                  {fileKinds.map((kind) => (
                    <Checkbox
                      key={kind}
                      label={fileKindLabel(kind, t)}
                      checked={useForKinds.includes(kind)}
                      onChange={(_, data) =>
                        setUseForKinds((kinds) => (data.checked ? [...kinds, kind] : kinds.filter((other) => other !== kind)))
                      }
                    />
                  ))}
                </div>
                <Field label={t("Extensions")} validationMessage={extensionsProblem ?? undefined}>
                  <Input value={useForExtensions} placeholder="dmg, zip" spellCheck={false} onChange={(_, data) => setUseForExtensions(data.value)} />
                </Field>
                <Text size={200} className="secondary">
                  {t(
                    "When an upload doesn’t name a destination (the clipboard shortcut, the panel, File Explorer, the local API), files of these types come here instead of the default destination. An extension listed here wins over a type.",
                  )}
                </Text>
              </div>

              <Text weight="semibold" className="form-heading">
                {t("Keyboard Shortcut")}
              </Text>
              <div className="form-group">
                <div className="inline-row">
                  <Text className="grow">{t("Upload clipboard here")}</Text>
                  <ShortcutRecorder value={shortcut} error={shortcutError} onChange={changeShortcut} />
                </div>
                <Text size={200} className="secondary">
                  {t("Uploads what’s on the clipboard to this destination, whatever “Use for” says. Kept on this PC only.")}
                </Text>
              </div>

              <Text weight="semibold" className="form-heading">
                {t("Image Processing")}
              </Text>
              <div className="form-group">
                <Field label={t("Format")}>
                  <Select value={imageFormat} onChange={(_, data) => setImageFormat(data.value as ImageFormat)}>
                    {imageFormats.map((format) => (
                      <option key={format} value={format}>
                        {format === "original" ? t("Keep Original") : format === "webp" ? "WebP" : "AVIF"}
                      </option>
                    ))}
                  </Select>
                </Field>
                <Field label={t("Compression")}>
                  <Select
                    value={String(imageQuality ?? "")}
                    onChange={(_, data) => setImageQuality(data.value ? Number(data.value) : null)}
                  >
                    <option value="">{t("Off")}</option>
                    {imageQualities.map((quality) => (
                      <option key={quality} value={quality}>
                        {compressionLabel(quality, t)}
                      </option>
                    ))}
                  </Select>
                </Field>
                <Field label={t("Resize")}>
                  <Select
                    value={String(imageMaxLongEdge ?? "")}
                    onChange={(_, data) => setImageMaxLongEdge(data.value ? Number(data.value) : null)}
                  >
                    <option value="">{t("Off")}</option>
                    {imageSizes.map((size) => (
                      <option key={size} value={size}>
                        {t("Longest side {0} px", size)}
                      </option>
                    ))}
                  </Select>
                </Field>
                <Text size={200} className="secondary">
                  {t(
                    "Applies to JPEG, PNG, HEIC, WebP, TIFF and BMP photos and screenshots. GIFs, SVGs and files inside ZIPs are left as they are.",
                  )}
                </Text>
              </div>

              <Text weight="semibold" className="form-heading">
                {t("Thumbnails")}
              </Text>
              <div className="form-group">
                <Field label={t("Thumbnails")}>
                  <Select value={thumbnailMode} onChange={(_, data) => setThumbnailMode(data.value as ThumbnailMode)}>
                    {thumbnailModes.map((mode) => (
                      <option key={mode} value={mode}>
                        {thumbnailModeLabel(mode, t)}
                      </option>
                    ))}
                  </Select>
                </Field>
                {thumbnailMode === "bucket" && (
                  <Field label={t("Folder")} validationMessage={prefixProblem ?? undefined}>
                    <Input
                      value={thumbnailPrefix}
                      placeholder={defaultThumbnailPrefix}
                      onChange={(_, data) => setThumbnailPrefix(data.value)}
                    />
                  </Field>
                )}
                <Text size={200} className="secondary">
                  {thumbnailMode === "off"
                    ? t("No thumbnails are made or downloaded for this destination. History and the bucket view show file icons.")
                    : thumbnailMode === "local"
                      ? t(
                          "Thumbnails of photos, videos, PDFs and documents are made on this PC and kept only here. Files already in the bucket get one when they’re shown, if they’re under 25 MB.",
                        )
                      : t(
                          "Thumbnails are also saved in your bucket, in this folder, so your other devices can show them. Each one is deleted, moved or expires along with its file. Use a folder only for thumbnails: it’s hidden in the bucket view.",
                        )}
                </Text>
              </div>

              <Text weight="semibold" className="form-heading">
                {t("Replacing Files")}
              </Text>
              <div className="form-group">
                <Switch label={t("Short cache time")} checked={shortCache} onChange={(_, data) => setShortCache(data.checked)} />
                <Field label={t("Cloudflare Zone ID")} validationMessage={zoneProblem ?? undefined}>
                  <Input
                    value={cloudflareZoneId}
                    placeholder={t("Optional")}
                    spellCheck={false}
                    onChange={(_, data) => setCloudflareZoneId(data.value)}
                  />
                </Field>
                <Field label={t("Cloudflare API Token")}>
                  <div className="inline-row">
                    <Input
                      className="grow"
                      type="password"
                      value={cloudflareToken}
                      placeholder={hasSavedToken && !removeToken ? t("Unchanged") : t("Optional")}
                      autoComplete="off"
                      onChange={(_, data) => {
                        setCloudflareToken(data.value);
                        setTokenCheck(null);
                      }}
                    />
                    <Button
                      size="small"
                      disabled={tokenCheck?.kind === "checking" || (cloudflareToken.trim() === "" && (!hasSavedToken || removeToken))}
                      onClick={checkToken}
                    >
                      {t("Check")}
                    </Button>
                    {hasSavedToken && !removeToken && cloudflareToken === "" && (
                      <Button size="small" appearance="subtle" onClick={() => setRemoveToken(true)}>
                        {t("Remove")}
                      </Button>
                    )}
                  </div>
                </Field>
                {tokenCheck?.kind === "checking" && (
                  <Text size={200} className="secondary">
                    {t("Checking…")}
                  </Text>
                )}
                {tokenCheck?.kind === "passed" && (
                  <Text size={200} className="text-success">
                    {t("Cloudflare accepts this token")}
                  </Text>
                )}
                {tokenCheck?.kind === "failed" && (
                  <Text size={200} className="text-error">
                    {tokenCheck.message}
                  </Text>
                )}
                <Text size={200} className="secondary">
                  {t(
                    "Replace File writes a new file at the same key, so its link keeps working. Short cache time sends every upload here with a one-minute cache time, so a replaced file shows up everywhere within about a minute. With a Cloudflare zone ID and an API token that can purge its cache (Zone > Cache Purge), Aktar clears the old version from Cloudflare right away.",
                  )}
                </Text>
              </div>

              <Text weight="semibold" className="form-heading">
                {t("After Upload")}
              </Text>
              <div className="form-group">
                <HookEditor
                  hooks={hooks}
                  description={t(
                    "Runs after each upload to this destination and each replace, except a watched folder’s files, which run their folder’s own Automation. A webhook gets the upload as JSON. A script gets the same JSON on standard input, with the link, the key, the file and the destination as its arguments.",
                  )}
                  onChange={setHooks}
                  test={(hook) => api.testDestinationHook(currentConfig(), hook)}
                  onError={setHookError}
                />
                {hookError && (
                  <Text size={200} className="text-error">
                    {hookError}
                  </Text>
                )}
              </div>

              <div ref={testOutcome}>
                <ConnectionTestResult result={testResult} error={testError} />
              </div>
              {saveError && (
                <Text size={200} className="text-error">
                  {saveError}
                </Text>
              )}
            </DialogContent>
            <DialogActions position="start">
              <Button
                appearance="secondary"
                disabled={isTesting || !canTest}
                icon={isTesting ? <Spinner size="tiny" /> : undefined}
                onClick={test}
              >
                {isTesting ? t("Testing…") : t("Test Connection")}
              </Button>
            </DialogActions>
            <DialogActions position="end">
              <Button appearance="secondary" onClick={onCancel}>
                {t("Cancel")}
              </Button>
              <Button appearance="primary" type="submit" disabled={!canSave || isSaving}>
                {t("Save")}
              </Button>
            </DialogActions>
          </DialogBody>
        </form>
        <ConfirmDialog
          open={isConfirmingTurnOff}
          title={t("Turn off auto-delete for this destination?")}
          message={t(
            "“Delete after” won't be offered for this destination. Files already under tmp/ are still deleted on schedule while the bucket keeps Aktar's rules; removing the rules keeps those files for good.",
          )}
          alternative={{ label: t("Turn Off"), onSelect: () => turnOffRules(false) }}
          confirmLabel={t("Turn Off and Remove Rules")}
          destructive
          onConfirm={() => turnOffRules(true)}
          onCancel={() => setIsConfirmingTurnOff(false)}
        />
        <ConfirmDialog
          open={oldThumbnailPrefix !== null}
          title={t("Delete the thumbnails already in the bucket?")}
          message={t(
            "Thumbnails saved in {0} stay in the bucket unless you delete them. Your files aren’t affected.",
            oldThumbnailPrefix ?? "",
          )}
          alternative={{ label: t("Keep Them"), onSelect: () => void save(false) }}
          confirmLabel={t("Delete Thumbnails")}
          destructive
          onConfirm={() => void save(true)}
          onCancel={() => setOldThumbnailPrefix(null)}
        />
        <ConfirmDialog
          open={prefixesInUse !== null}
          title={t("Files already in these folders will be deleted")}
          message={t(
            "{0} already hold files. Once the rules are set up, the bucket deletes them too when they're older than the folder's number of days.",
            (prefixesInUse ?? []).join(", "),
          )}
          confirmLabel={t("Set Up Anyway")}
          destructive
          onConfirm={applyRules}
          onCancel={() => setPrefixesInUse(null)}
        />
      </DialogSurface>
    </Dialog>
  );
}

export function fileKindLabel(kind: FileKind, t: Translate) {
  switch (kind) {
    case "image":
      return t("Images");
    case "video":
      return t("Videos");
    case "audio":
      return t("Audio");
    case "document":
      return t("Documents");
    case "archive":
      return t("Archives");
  }
}

function thumbnailModeLabel(mode: ThumbnailMode, t: Translate) {
  switch (mode) {
    case "off":
      return t("Off");
    case "local":
      return t("On This PC");
    case "bucket":
      return t("In the Bucket");
  }
}

function compressionLabel(quality: number, t: Translate) {
  switch (quality) {
    case 90:
      return t("Light (90%)");
    case 80:
      return t("Medium (80%)");
    default:
      return t("Strong (65%)");
  }
}

function imageMetadataLabel(policy: ImageMetadataPolicy, t: Translate) {
  switch (policy) {
    case "removeLocation":
      return t("Remove location");
    case "removeAll":
      return t("Remove all");
    case "keepAll":
      return t("Keep all");
  }
}

/** Whether the bucket deletes expiring uploads itself, and if the key
 * can't set that up, how to do it by hand. */
function ExpiryRulesState({ check }: { check: ExpiryRulesCheck | null }) {
  const { t } = useI18n();
  if (!check) {
    return (
      <Text size={200} className="secondary">
        {t("Not set up yet.")}
      </Text>
    );
  }
  if (check.status.kind === "active") {
    return (
      <Text size={200} className="text-success">
        {t("Lifecycle rules are set up.")}
      </Text>
    );
  }
  return (
    <div className="expiry-rules-help">
      <Text size={200} className="text-warning" title={check.status.message}>
        {rulesStatusMessage(check.status.kind, t)}
      </Text>
      {check.status.kind === "denied" && (
        <Text size={200} className="secondary">
          {expiryRulesExplanation(t)}
        </Text>
      )}
    </div>
  );
}
