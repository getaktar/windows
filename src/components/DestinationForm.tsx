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
  Spinner,
  Switch,
  Text,
} from "@fluentui/react-components";
import { useEffect, useRef, useState } from "react";

import {
  api,
  errorMessage,
  expiryDurations,
  folderUploadModes,
  imageMetadataPolicies,
  temporaryLinkDurations,
  type ConnectionResult,
  type DestinationConfig,
  type ExpiryRulesCheck,
  type FolderUploadMode,
  type FormRules,
  type ImageMetadataPolicy,
  type OutputMode,
  type ProviderPreset,
  type PublicLinkCheck,
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
import { ConfirmDialog } from "./Dialogs";

const presets: ProviderPreset[] = ["cloudflareR2", "amazonS3", "minIO", "backblazeB2", "digitalOceanSpaces", "customS3"];

const defaultRegion = (preset: ProviderPreset) => (preset === "amazonS3" ? "us-east-1" : "auto");

/** MinIO deployments commonly run without DNS-based virtual-hosted buckets. */
const defaultForcePathStyle = (preset: ProviderPreset) => preset === "minIO";

const r2Endpoint = (accountID: string) => `https://${accountID}.r2.cloudflarestorage.com`;

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
    setTestResult(null);
    setTestError(null);
    setSaveError(null);
    // Settings only supply the starting "Delete after" of a new form.
  }, [open, existing]);

  const hasNewCredentials = accessKeyId.trim() !== "" && secretAccessKey !== "";
  const canTest = endpoint.trim() !== "" && bucket.trim() !== "" && (hasNewCredentials || existing !== null);
  const canSave =
    name.trim() !== "" &&
    bucket.trim() !== "" &&
    endpoint.trim() !== "" &&
    publicBaseURL.trim() !== "" &&
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
  });

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

  const save = async () => {
    setIsSaving(true);
    setSaveError(null);
    try {
      onSaved(await api.saveDestination(currentConfig(), credentials(), formRules));
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
            if (canSave) save();
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
                  hint={t("Variables: {year} {month} {day} {date} {time} {filename} {uuid} {random} {ext}")}
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
                <Field label={t("Image metadata")}>
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

              <div ref={testOutcome}>
                {testError && (
                  <Text size={200} className="text-error">
                    {testError}
                  </Text>
                )}
                {testResult && <TestResult result={testResult} />}
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

/** One line per step of the test, so a bucket that takes uploads but won't
 * serve them reads as a problem instead of a success. */
function TestResult({ result }: { result: ConnectionResult }) {
  const { t } = useI18n();
  const link = result.publicLink;
  const hint = publicLinkHint(link, t);
  return (
    <div className="test-result">
      {result.writable ? (
        <Text size={200} className="text-success">
          ✓ {t("Upload: works")}
        </Text>
      ) : (
        <Text size={200} className="text-error">
          ✕ {t("Upload: failed. This key can read the bucket but can’t write to it.")}
        </Text>
      )}
      {link?.kind === "reachable" && (
        <Text size={200} className="text-success">
          ✓ {t("Public link: works")}
        </Text>
      )}
      {link?.kind === "status" && (
        <Text size={200} className="text-error">
          ✕ {t("Public link: failed (HTTP {0})", String(link.code))}
        </Text>
      )}
      {link?.kind === "noResponse" && (
        <Text size={200} className="text-error">
          ✕ {t("Public link: no response")}
        </Text>
      )}
      {hint && (
        <Text size={200} className="secondary">
          {hint}
        </Text>
      )}
    </div>
  );
}

function publicLinkHint(check: PublicLinkCheck | null, t: Translate) {
  if (!check || check.kind === "reachable") return null;
  if (check.kind === "status" && (check.code === 401 || check.code === 403)) {
    return t(
      "Uploads work, but anyone who opens a link gets an error. Allow public reads on the bucket (on R2, turn on the r2.dev URL or connect a custom domain), or keep it private and share files with Copy Temporary Link in the Library.",
    );
  }
  if (check.kind === "status" && check.code === 404) {
    return t("The test file was uploaded, but it isn’t at the Public Base URL. Check that the URL points to this bucket.");
  }
  return t("The Public Base URL didn’t serve the test file. Check the domain and that it points to this bucket.");
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
