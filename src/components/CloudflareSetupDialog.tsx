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
  Radio,
  RadioGroup,
  Select,
  Spinner,
  Text,
} from "@fluentui/react-components";
import { CheckmarkCircleFilled, CircleRegular, DismissCircleFilled, OpenRegular } from "@fluentui/react-icons";
import { useEffect, useRef, useState } from "react";

import { api, errorMessage, type CloudflareItem, type ConnectionResult, type DestinationConfig } from "../lib/api";
import { providerName } from "../lib/format";
import { useI18n } from "../lib/i18n";
import { ConnectionTestResult } from "./ConnectionTestResult";

type Step = "token" | "options" | "working" | "saved";

/** What the setup does once it runs, in order. */
type Stage = "bucket" | "publicLink" | "save";
type StageState = "running" | "done" | "failed";

const STAGES: Stage[] = ["bucket", "publicLink", "save"];

/** A bucket name Cloudflare accepts: 3 to 63 lowercase letters, digits
 * and hyphens, starting and ending with a letter or digit. Rust checks it
 * again. */
const isValidBucketName = (name: string) => /^[a-z0-9][a-z0-9-]{1,61}[a-z0-9]$/.test(name);

/** A host name inside `zone`: the zone itself or a subdomain of it. */
function isValidDomain(domain: string, zone: string) {
  const host = domain.toLowerCase();
  if (host !== zone && !host.endsWith(`.${zone}`)) return false;
  return /^([a-z0-9]([a-z0-9-]*[a-z0-9])?\.)+[a-z]{2,}$/.test(host);
}

/** "aktar", or "aktar-2" and on when that's taken. */
function suggestedBucketName(buckets: string[]) {
  if (!buckets.includes("aktar")) return "aktar";
  let number = 2;
  while (buckets.includes(`aktar-${number}`)) number++;
  return `aktar-${number}`;
}

/** "Set Up Cloudflare R2": opens Cloudflare's token page with the
 * permissions filled in, takes the token, and does the rest (bucket,
 * public link, keys) in Rust, then saves the destination and tests it like
 * an imported one. The token stays in this dialog only while it's open. */
export function CloudflareSetupDialog({
  open,
  onClose,
  onDone,
  onEdit,
}: {
  open: boolean;
  /** Closed before anything was saved. */
  onClose: () => void;
  /** "Done" on the last step, or closing it. */
  onDone: (destination: DestinationConfig) => void;
  /** "Edit" on the last step: the destination form for what was saved. */
  onEdit: (destination: DestinationConfig) => void;
}) {
  const { t } = useI18n();
  const [step, setStep] = useState<Step>("token");
  const [token, setToken] = useState("");
  const [tokenId, setTokenId] = useState("");
  const [tokenError, setTokenError] = useState<string | null>(null);
  const [isChecking, setIsChecking] = useState(false);

  const [accounts, setAccounts] = useState<CloudflareItem[]>([]);
  const [accountId, setAccountId] = useState("");
  const [buckets, setBuckets] = useState<string[]>([]);
  const [zones, setZones] = useState<CloudflareItem[]>([]);
  const [isLoadingAccount, setIsLoadingAccount] = useState(false);
  const [accountError, setAccountError] = useState<string | null>(null);

  /** The existing bucket picked, or null for a new one. */
  const [bucketChoice, setBucketChoice] = useState<string | null>(null);
  const [newBucket, setNewBucket] = useState("aktar");
  const [publicLink, setPublicLink] = useState<"devURL" | "domain">("devURL");
  const [zoneId, setZoneId] = useState("");
  const [subdomain, setSubdomain] = useState("files");
  const [name, setName] = useState("Cloudflare R2");

  const [progress, setProgress] = useState<Partial<Record<Stage, StageState>>>({});
  const [setupError, setSetupError] = useState<string | null>(null);
  const [saved, setSaved] = useState<DestinationConfig | null>(null);
  const [isTesting, setIsTesting] = useState(false);
  const [testResult, setTestResult] = useState<ConnectionResult | null>(null);
  const [testError, setTestError] = useState<string | null>(null);
  /** Bumped when the dialog opens or closes, so work still running from
   * before doesn't show up in the next setup. */
  const session = useRef(0);
  /** Bumped when another account is picked, so a slower answer about the
   * one before is dropped. */
  const accountRequest = useRef(0);
  const doneButton = useRef<HTMLButtonElement>(null);

  useEffect(() => {
    session.current++;
    if (!open) return;
    setStep("token");
    setToken("");
    setTokenId("");
    setTokenError(null);
    setIsChecking(false);
    setAccounts([]);
    setAccountId("");
    setBuckets([]);
    setZones([]);
    setIsLoadingAccount(false);
    setAccountError(null);
    setBucketChoice(null);
    setNewBucket("aktar");
    setPublicLink("devURL");
    setZoneId("");
    setSubdomain("files");
    setName("Cloudflare R2");
    setProgress({});
    setSetupError(null);
    setSaved(null);
    setIsTesting(false);
    setTestResult(null);
    setTestError(null);
  }, [open]);

  useEffect(() => {
    // The button that started it is gone; without focus, Esc and Enter
    // would go nowhere.
    if (step === "saved") window.setTimeout(() => doneButton.current?.focus(), 0);
  }, [step]);

  // MARK: Token

  const trimmedToken = token.trim();

  const checkToken = async () => {
    if (!trimmedToken || isChecking) return;
    const current = session.current;
    setIsChecking(true);
    setTokenError(null);
    try {
      const checked = await api.cloudflareCheckToken(trimmedToken);
      if (current !== session.current) return;
      setTokenId(checked.tokenId);
      setAccounts(checked.accounts);
      setStep("options");
      const first = checked.accounts[0];
      selectAccount(checked.accounts.some((account) => account.id === accountId) ? accountId : first.id);
    } catch (error) {
      if (current === session.current) setTokenError(errorMessage(error));
    } finally {
      if (current === session.current) setIsChecking(false);
    }
  };

  // MARK: Options

  const selectAccount = async (id: string) => {
    const current = ++accountRequest.current;
    setAccountId(id);
    setIsLoadingAccount(true);
    setAccountError(null);
    try {
      const found = await api.cloudflareAccount(trimmedToken, id);
      if (current !== accountRequest.current) return;
      setBuckets(found.buckets);
      setZones(found.zones);
      setZoneId(found.zones[0]?.id ?? "");
      setBucketChoice((choice) => (choice !== null && found.buckets.includes(choice) ? choice : null));
      setNewBucket(suggestedBucketName(found.buckets));
    } catch (error) {
      if (current !== accountRequest.current) return;
      setBuckets([]);
      setZones([]);
      setAccountError(errorMessage(error));
    } finally {
      if (current === accountRequest.current) setIsLoadingAccount(false);
    }
  };

  const bucketName = bucketChoice ?? newBucket.trim();

  /** Only for a new bucket: an existing one was picked from the list. */
  const bucketProblem = (() => {
    if (bucketChoice !== null || !bucketName) return null;
    if (buckets.includes(bucketName)) return t("A bucket with this name already exists. Pick it from the list above to use it.");
    if (!isValidBucketName(bucketName)) {
      return t("Use 3 to 63 lowercase letters, numbers and hyphens, starting and ending with a letter or number.");
    }
    return null;
  })();

  const selectedZone = zones.find((zone) => zone.id === zoneId) ?? null;

  /** The full host name for the public links, when it's valid. */
  const customDomain = (() => {
    if (!selectedZone) return null;
    const label = subdomain.trim().toLowerCase();
    const domain = label ? `${label}.${selectedZone.name}` : selectedZone.name;
    return isValidDomain(domain, selectedZone.name) ? domain : null;
  })();

  const canSetUp =
    accountId !== "" &&
    !isLoadingAccount &&
    accountError === null &&
    name.trim() !== "" &&
    bucketName !== "" &&
    bucketProblem === null &&
    (publicLink === "devURL" || customDomain !== null);

  // MARK: Setup

  const runSetup = async () => {
    if (!canSetUp) return;
    const current = session.current;
    const isCurrent = () => current === session.current;
    const mark = (stage: Stage, state: StageState) => {
      if (isCurrent()) setProgress((progress) => ({ ...progress, [stage]: state }));
    };
    setStep("working");
    setSetupError(null);
    setProgress({});
    const bucket = bucketName;
    const domain = publicLink === "domain" ? customDomain : null;
    const zone = domain ? selectedZone : null;
    let stage: Stage = "bucket";
    try {
      mark("bucket", "running");
      if (bucketChoice === null) {
        await api.cloudflareCreateBucket(trimmedToken, accountId, bucket);
        // Back after a later step fails uses it instead of making it again.
        if (isCurrent()) {
          setBuckets((buckets) => [...buckets, bucket].sort());
          setBucketChoice(bucket);
        }
      }
      mark("bucket", "done");

      stage = "publicLink";
      mark("publicLink", "running");
      const baseURL = await api.cloudflareEnablePublicLinks(trimmedToken, accountId, bucket, zone, domain);
      mark("publicLink", "done");

      stage = "save";
      mark("save", "running");
      const destination = await api.cloudflareSaveDestination(trimmedToken, tokenId, accountId, bucket, baseURL, name.trim(), domain !== null);
      if (!isCurrent()) return;
      setSaved(destination);
      setStep("saved");
      // The token was just made and its permissions may take a moment to
      // reach R2, so the first test waits briefly.
      setIsTesting(true);
      window.setTimeout(() => testConnection(destination, current), 2000);
    } catch (error) {
      mark(stage, "failed");
      if (isCurrent()) setSetupError(errorMessage(error));
    }
  };

  const testConnection = async (destination: DestinationConfig, current: number) => {
    if (current !== session.current) return;
    setIsTesting(true);
    setTestResult(null);
    setTestError(null);
    try {
      const result = await api.testConnection(destination, null);
      if (current === session.current) setTestResult(result);
    } catch (error) {
      if (current === session.current) setTestError(errorMessage(error));
    } finally {
      if (current === session.current) setIsTesting(false);
    }
  };

  const close = () => {
    if (saved) onDone(saved);
    else onClose();
  };

  const stageTitle = (stage: Stage) => {
    switch (stage) {
      case "bucket":
        return t("Create the bucket");
      case "publicLink":
        return t("Turn on public links");
      case "save":
        return t("Save and test the destination");
    }
  };

  const stageIcon = (state: StageState | undefined) => {
    switch (state) {
      case "running":
        return <Spinner size="extra-tiny" />;
      case "done":
        return <CheckmarkCircleFilled className="text-success" fontSize={16} />;
      case "failed":
        return <DismissCircleFilled className="text-error" fontSize={16} />;
      default:
        return <CircleRegular className="tertiary" fontSize={16} />;
    }
  };

  const busy = step === "token" ? isChecking : step === "options" ? isLoadingAccount : step === "working" && setupError === null;

  return (
    <Dialog open={open} onOpenChange={(_, data) => !data.open && close()}>
      <DialogSurface className="cloudflare-surface">
        <form
          onSubmit={(event) => {
            event.preventDefault();
            if (step === "token") checkToken();
            if (step === "options") runSetup();
            if (step === "saved") close();
          }}
        >
          <DialogBody>
            <DialogTitle>{t("Set Up Cloudflare R2")}</DialogTitle>
            {step === "token" && (
              <DialogContent className="dialog-stack">
                <Text>
                  {t(
                    "Aktar creates the bucket, turns on public links and saves the destination for you. It only needs an API token from your Cloudflare account.",
                  )}
                </Text>
                <div className="cloudflare-section">
                  <Text weight="semibold">{t("1. Create a token")}</Text>
                  <div>
                    <Button icon={<OpenRegular />} onClick={() => api.openCloudflareTokenPage().catch(() => {})}>
                      {t("Open Cloudflare")}
                    </Button>
                  </div>
                  <Text size={200} className="secondary">
                    {t(
                      "The permissions are already filled in (R2 Storage: Edit, Zone: Read). Scroll down, select Continue to summary, then Create Token, and copy the token.",
                    )}
                  </Text>
                </div>
                <div className="cloudflare-section">
                  <Text weight="semibold">{t("2. Paste it")}</Text>
                  <Field
                    label={t("API Token")}
                    validationMessage={tokenError ?? undefined}
                    hint={t("The token isn’t stored. Aktar keeps only the storage keys made from it, in Credential Manager.")}
                  >
                    <Input
                      type="password"
                      value={token}
                      placeholder={t("Paste the token here")}
                      autoComplete="off"
                      spellCheck={false}
                      onChange={(_, data) => {
                        setToken(data.value);
                        setTokenError(null);
                      }}
                    />
                  </Field>
                </div>
              </DialogContent>
            )}
            {step === "options" && (
              <DialogContent className="dialog-stack">
                {accounts.length > 1 && (
                  <Field label={t("Account")}>
                    <Select value={accountId} onChange={(_, data) => selectAccount(data.value)}>
                      {accounts.map((account) => (
                        <option key={account.id} value={account.id}>
                          {account.name}
                        </option>
                      ))}
                    </Select>
                  </Field>
                )}
                {accountError && (
                  <Text size={200} className="text-error">
                    {accountError}
                  </Text>
                )}
                <Field label={t("Bucket")}>
                  <Select
                    value={bucketChoice ?? ""}
                    onChange={(_, data) => setBucketChoice(data.value === "" ? null : data.value)}
                  >
                    <option value="">{t("New Bucket")}</option>
                    {buckets.map((bucket) => (
                      <option key={bucket} value={bucket}>
                        {bucket}
                      </option>
                    ))}
                  </Select>
                </Field>
                {bucketChoice === null && (
                  <Field
                    label={t("Name")}
                    validationState={bucketProblem ? "warning" : "none"}
                    validationMessage={bucketProblem ?? undefined}
                  >
                    <Input value={newBucket} placeholder="aktar" spellCheck={false} onChange={(_, data) => setNewBucket(data.value)} />
                  </Field>
                )}
                <Field
                  label={t("Public Links")}
                  hint={
                    publicLink === "devURL"
                      ? t("Works right away. Cloudflare rate-limits r2.dev addresses, so a domain of your own is better for links you share widely.")
                      : t("Cloudflare adds the DNS record and certificate. It can take a few minutes before links work.")
                  }
                >
                  <RadioGroup
                    value={publicLink}
                    onChange={(_, data) => setPublicLink(data.value === "domain" ? "domain" : "devURL")}
                  >
                    <Radio value="devURL" label={t("r2.dev Address")} />
                    <Radio value="domain" label={t("My Domain")} />
                  </RadioGroup>
                </Field>
                {publicLink === "domain" &&
                  (zones.length === 0 ? (
                    <Text size={200} className="secondary">
                      {t("No domain on this Cloudflare account. Add one to Cloudflare first, or use the r2.dev address.")}
                    </Text>
                  ) : (
                    <>
                      <Field label={t("Domain")}>
                        <Select value={zoneId} onChange={(_, data) => setZoneId(data.value)}>
                          {zones.map((zone) => (
                            <option key={zone.id} value={zone.id}>
                              {zone.name}
                            </option>
                          ))}
                        </Select>
                      </Field>
                      <Field
                        label={t("Subdomain")}
                        hint={customDomain ? t("Links will look like https://{0}/2026/10/photo.jpg", customDomain) : undefined}
                      >
                        <Input value={subdomain} placeholder="files" spellCheck={false} onChange={(_, data) => setSubdomain(data.value)} />
                      </Field>
                    </>
                  ))}
                <Field label={t("Destination Name")}>
                  <Input value={name} onChange={(_, data) => setName(data.value)} />
                </Field>
              </DialogContent>
            )}
            {step === "working" && (
              <DialogContent className="dialog-stack">
                {STAGES.map((stage) => (
                  <div key={stage} className="cloudflare-stage">
                    {stageIcon(progress[stage])}
                    <Text>{stageTitle(stage)}</Text>
                  </div>
                ))}
                {setupError && (
                  <Text size={200} className="text-error break">
                    {setupError}
                  </Text>
                )}
              </DialogContent>
            )}
            {step === "saved" && saved && (
              <DialogContent className="dialog-stack">
                <Text weight="semibold">{t("“{0}” was added.", saved.name)}</Text>
                <Text size={200} className="secondary break">
                  {`${providerName(saved.preset, t)} · ${saved.bucket} · ${saved.publicBaseURL}`}
                </Text>
                {isTesting ? (
                  <Spinner size="tiny" labelPosition="after" label={t("Testing…")} className="import-testing" />
                ) : (
                  <>
                    <ConnectionTestResult result={testResult} error={testError} />
                    {!saved.publicBaseURL.endsWith(".r2.dev") && (
                      <Text size={200} className="secondary">
                        {t(
                          "If the public link check failed, the domain is probably still being set up. Try Test Connection again in a few minutes from Edit.",
                        )}
                      </Text>
                    )}
                  </>
                )}
              </DialogContent>
            )}
            {step === "saved" ? (
              <DialogActions>
                <Button appearance="secondary" onClick={() => saved && onEdit(saved)}>
                  {t("Edit")}
                </Button>
                <Button ref={doneButton} appearance="primary" type="submit">
                  {t("Done")}
                </Button>
              </DialogActions>
            ) : (
              <DialogActions>
                {(step === "options" || (step === "working" && setupError !== null)) && (
                  <Button appearance="secondary" onClick={() => setStep(step === "options" ? "token" : "options")}>
                    {t("Back")}
                  </Button>
                )}
                <Button appearance="secondary" onClick={close}>
                  {t("Cancel")}
                </Button>
                <Button
                  appearance="primary"
                  type="submit"
                  disabled={step === "token" ? !trimmedToken || isChecking : step === "options" ? !canSetUp : true}
                  icon={busy ? <Spinner size="tiny" /> : undefined}
                >
                  {step === "token" ? t("Continue") : t("Set Up")}
                </Button>
              </DialogActions>
            )}
          </DialogBody>
        </form>
      </DialogSurface>
    </Dialog>
  );
}
