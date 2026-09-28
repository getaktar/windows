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
import { useEffect, useState } from "react";

import { api, errorMessage, type DestinationConfig, type ProviderPreset } from "../lib/api";
import { providerName } from "../lib/format";
import { useI18n } from "../lib/i18n";

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
  const { t } = useI18n();
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
  const [testResult, setTestResult] = useState<{ ok: boolean; message: string } | null>(null);
  const [isTesting, setIsTesting] = useState(false);
  const [isSaving, setIsSaving] = useState(false);
  const [saveError, setSaveError] = useState<string | null>(null);

  useEffect(() => {
    if (!open) return;
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
    setTestResult(null);
    setSaveError(null);
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
  });

  const credentials = () => (hasNewCredentials ? { accessKeyId, secretAccessKey, sessionToken: null } : null);

  const test = async () => {
    setIsTesting(true);
    setTestResult(null);
    try {
      const result = await api.testConnection(currentConfig(), credentials());
      const parts: string[] = [];
      if (result.writable) parts.push(t("✓ Connection successful."));
      parts.push(result.writable ? t("Bucket reachable. Write access: Yes.") : t("Bucket reachable. Write access: No."));
      if (result.publicUrlReachable !== null) {
        parts.push(
          result.publicUrlReachable
            ? t("Public URL: Reachable.")
            : t("⚠ Public URL does not appear to be publicly accessible."),
        );
      }
      setTestResult({ ok: result.writable, message: parts.join(" ") });
    } catch (error) {
      setTestResult({ ok: false, message: errorMessage(error) });
    } finally {
      setIsTesting(false);
    }
  };

  const save = async () => {
    setIsSaving(true);
    setSaveError(null);
    try {
      onSaved(await api.saveDestination(currentConfig(), credentials()));
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
                      setTestResult(null);
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
                      }}
                    />
                  </Field>
                ) : (
                  <Field label={t("Endpoint")} required>
                    <Input value={endpoint} placeholder="s3.example.com" onChange={(_, data) => setEndpoint(data.value)} />
                  </Field>
                )}
                <Field label={t("Region")}>
                  <Input value={region} onChange={(_, data) => setRegion(data.value)} />
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
                  <Input value={bucket} placeholder="screenshots" spellCheck={false} onChange={(_, data) => setBucket(data.value)} />
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
                    onChange={(_, data) => setPublicBaseURL(data.value)}
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

              {testResult && (
                <Text size={200} className={testResult.ok ? "text-success" : "text-error"}>
                  {testResult.message}
                </Text>
              )}
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
      </DialogSurface>
    </Dialog>
  );
}
