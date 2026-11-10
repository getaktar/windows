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
  Text,
} from "@fluentui/react-components";
import { useEffect, useRef, useState } from "react";

import { api, errorMessage, type ConnectionResult, type DestinationConfig } from "../lib/api";
import { providerName } from "../lib/format";
import { useDestinations } from "../lib/hooks";
import { useI18n } from "../lib/i18n";
import { sharex, type ShareXImported, type ShareXReview } from "../lib/sharex";
import { ConnectionTestResult } from "./ConnectionTestResult";

function withBold(text: string) {
  return text.split("**").map((part, index) => (index % 2 === 1 ? <strong key={index}>{part}</strong> : part));
}

/** Where a shortener goes: "" is not imported. */
type ShortenerTarget = string;

interface DestinationDraft {
  selected: boolean;
  secret: string;
  endpoint: string;
}

interface TestState {
  testing: boolean;
  result: ConnectionResult | null;
  error: string | null;
}

/** "Import from ShareX": reads ShareX's settings (found in its folder, or
 * a picked UploadersConfig.json or .sxb backup), shows what was found, and
 * imports what's ticked. Each new destination's connection is tested right
 * after, as after Import from Another Device. */
export function ShareXImportDialog({ open, pick, onClose }: { open: boolean; pick: boolean; onClose: () => void }) {
  const { t } = useI18n();
  const [destinations] = useDestinations();
  const [review, setReview] = useState<ShareXReview | null>(null);
  const [loading, setLoading] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const [drafts, setDrafts] = useState<Record<string, DestinationDraft>>({});
  const [targets, setTargets] = useState<Record<string, ShortenerTarget>>({});
  const [history, setHistory] = useState(true);
  const [importing, setImporting] = useState(false);
  const [imported, setImported] = useState<ShareXImported | null>(null);
  const [tests, setTests] = useState<Record<string, TestState>>({});
  const session = useRef(0);

  /** `picking`: from a file dialog, where nothing found is a cancel. */
  const start = (read: () => Promise<ShareXReview | null>, picking: boolean) => {
    const current = ++session.current;
    setLoading(true);
    setError(null);
    setReview(null);
    setImported(null);
    setTests({});
    read()
      .then((found) => {
        if (current !== session.current) return;
        if (!found) {
          if (pick) onClose();
          else if (!picking) setError(t("ShareX’s settings weren’t found on this PC."));
          return;
        }
        setReview(found);
        const nextDrafts: Record<string, DestinationDraft> = {};
        for (const destination of found.destinations) {
          nextDrafts[destination.id] = { selected: !destination.alreadyInAktar, secret: "", endpoint: "" };
        }
        setDrafts(nextDrafts);
        const firstNew = found.destinations.find((destination) => !destination.alreadyInAktar)?.id;
        const fallback = firstNew ?? destinations.find((destination) => destination.isDefault)?.id ?? "";
        const nextTargets: Record<string, ShortenerTarget> = {};
        for (const shortener of found.shorteners) nextTargets[shortener.id] = shortener.usesHTTP ? "" : fallback;
        setTargets(nextTargets);
        setHistory(found.historyCount > 0);
      })
      .catch((failure) => current === session.current && setError(errorMessage(failure)))
      .finally(() => current === session.current && setLoading(false));
  };

  useEffect(() => {
    if (!open) {
      session.current++;
      return;
    }
    start(pick ? sharex.pickFile : sharex.scan, pick);
  }, [open, pick]);

  const draft = (id: string) => drafts[id] ?? { selected: false, secret: "", endpoint: "" };
  const setDraft = (id: string, change: Partial<DestinationDraft>) => setDrafts((current) => ({ ...current, [id]: { ...draft(id), ...change } }));

  const selected = review?.destinations.filter((destination) => !destination.alreadyInAktar && draft(destination.id).selected) ?? [];
  const missing = selected.some(
    (destination) => (destination.needsSecret && !draft(destination.id).secret.trim()) || (destination.needsEndpoint && !draft(destination.id).endpoint.trim()),
  );
  const shortenerCount = Object.values(targets).filter(Boolean).length;
  const somethingToImport = selected.length > 0 || shortenerCount > 0 || (history && (review?.historyCount ?? 0) > 0);

  /** Where a shortener can go: the destinations ticked for import, then
   * the ones already in Aktar. */
  const targetOptions = [
    ...selected.map((destination) => ({ value: destination.id, label: destination.name })),
    ...destinations.map((destination) => ({ value: destination.id, label: destination.name })),
  ];

  const test = async (destination: DestinationConfig) => {
    setTests((current) => ({ ...current, [destination.id]: { testing: true, result: null, error: null } }));
    try {
      const result = await api.testConnection(destination, null);
      setTests((current) => ({ ...current, [destination.id]: { testing: false, result, error: null } }));
    } catch (failure) {
      setTests((current) => ({ ...current, [destination.id]: { testing: false, result: null, error: errorMessage(failure) } }));
    }
  };

  const submit = async () => {
    if (!review || importing || missing || !somethingToImport) return;
    setImporting(true);
    setError(null);
    try {
      const result = await sharex.importReviewed({
        destinations: selected.map((destination) => ({
          id: destination.id,
          secretAccessKey: draft(destination.id).secret.trim() || null,
          endpoint: draft(destination.id).endpoint.trim() || null,
        })),
        shorteners: Object.entries(targets)
          .filter(([, destination]) => destination)
          .map(([id, destination]) => ({ id, destination })),
        history: history && review.historyCount > 0,
      });
      setImported(result);
      result.destinations.forEach(test);
    } catch (failure) {
      setError(errorMessage(failure));
    } finally {
      setImporting(false);
    }
  };

  const nothingFound = review && review.destinations.length === 0 && review.shorteners.length === 0 && review.historyCount === 0;

  return (
    <Dialog open={open && !(pick && !review && !error && !loading)} onOpenChange={(_, data) => !data.open && onClose()}>
      <DialogSurface className="import-surface">
        <form
          onSubmit={(event) => {
            event.preventDefault();
            if (imported) onClose();
            else submit();
          }}
        >
          <DialogBody>
            <DialogTitle>{t("Import from ShareX")}</DialogTitle>
            <DialogContent className="dialog-stack">
              {loading && <Spinner size="small" label={t("Reading ShareX’s settings…")} />}
              {error && (
                <Text size={200} className="text-error">
                  {error}
                </Text>
              )}
              {review && !imported && (
                <>
                  <Text size={200} className="secondary break">
                    {t("Read from {0}", review.source)}
                  </Text>
                  {nothingFound && <Text>{t("ShareX has no S3 or Backblaze B2 uploaders set up.")}</Text>}

                  {review.destinations.map((destination) => {
                    const current = draft(destination.id);
                    return (
                      <div key={destination.id} className="form-group">
                        <Checkbox
                          label={<Text weight="semibold">{destination.name}</Text>}
                          checked={!destination.alreadyInAktar && current.selected}
                          disabled={destination.alreadyInAktar}
                          onChange={(_, data) => setDraft(destination.id, { selected: data.checked === true })}
                        />
                        <div className="consent-grid">
                          <Text className="secondary">{t("Provider")}</Text>
                          <Text>{providerName(destination.preset, t)}</Text>
                          <Text className="secondary">{t("Bucket")}</Text>
                          <Text className="mono">{destination.bucket}</Text>
                          {destination.endpoint && (
                            <>
                              <Text className="secondary">{t("Endpoint")}</Text>
                              <Text className="mono selectable break">{destination.endpoint}</Text>
                            </>
                          )}
                          <Text className="secondary">{t("Public Base URL")}</Text>
                          <Text className="mono selectable break">{destination.publicBaseURL || "-"}</Text>
                          <Text className="secondary">{t("Object Path")}</Text>
                          <Text className="mono break">{destination.template}</Text>
                        </div>
                        {destination.alreadyInAktar && (
                          <Text size={200} className="secondary">
                            {t("Already in Aktar")}
                          </Text>
                        )}
                        {destination.notes.map((note) => (
                          <Text key={note} size={200} className="text-warning">
                            {note}
                          </Text>
                        ))}
                        {!destination.alreadyInAktar && current.selected && destination.needsSecret && (
                          <Field label={destination.kind === "backblazeB2" ? t("Application Key") : t("Secret Access Key")} required>
                            <Input
                              type="password"
                              value={current.secret}
                              autoComplete="off"
                              onChange={(_, data) => setDraft(destination.id, { secret: data.value })}
                            />
                          </Field>
                        )}
                        {!destination.alreadyInAktar && current.selected && destination.needsEndpoint && (
                          <Field label={t("Endpoint")} required>
                            <Input
                              value={current.endpoint}
                              placeholder="https://s3.us-west-004.backblazeb2.com"
                              spellCheck={false}
                              onChange={(_, data) => setDraft(destination.id, { endpoint: data.value })}
                            />
                          </Field>
                        )}
                      </div>
                    );
                  })}

                  {review.shorteners.length > 0 && (
                    <Text weight="semibold" className="form-heading">
                      {t("Short Links")}
                    </Text>
                  )}
                  {review.shorteners.map((shortener) => (
                    <div key={shortener.id} className="form-group">
                      <Text weight="semibold">{shortener.name}</Text>
                      <Text size={200}>
                        {withBold(
                          shortener.hasToken
                            ? t("This configuration will send your API token to **{0}**.", shortener.host)
                            : t("This configuration will send your links to **{0}**.", shortener.host),
                        )}
                      </Text>
                      <Text size={200} className="mono selectable break">
                        {shortener.method} {shortener.endpoint}
                      </Text>
                      {shortener.usesHTTP && (
                        <Text size={200} className="text-warning">
                          {t("Links and your API key are sent unencrypted over http://. Use this only on a network you trust, such as your own computer.")}
                        </Text>
                      )}
                      <Field label={t("Destination")}>
                        <Select value={targets[shortener.id] ?? ""} onChange={(_, data) => setTargets((current) => ({ ...current, [shortener.id]: data.value }))}>
                          <option value="">{t("Don’t import")}</option>
                          {targetOptions.map((option) => (
                            <option key={option.value} value={option.value}>
                              {option.label}
                            </option>
                          ))}
                        </Select>
                      </Field>
                    </div>
                  ))}

                  {review.historyCount > 0 && (
                    <Checkbox
                      label={t("Import {0} uploads from ShareX’s history", review.historyCount)}
                      checked={history}
                      onChange={(_, data) => setHistory(data.checked === true)}
                    />
                  )}
                  {review.historySkipped > 0 && (
                    <Text size={200} className="secondary">
                      {t("{0} history entries on other hosts (Imgur, FTP and others) aren’t imported.", review.historySkipped)}
                    </Text>
                  )}

                  {review.skipped.length > 0 && (
                    <>
                      <Text weight="semibold" className="form-heading">
                        {t("Not Imported")}
                      </Text>
                      {review.skipped.map((item, index) => (
                        <Text key={`${item.name}-${index}`} size={200} className="secondary">
                          {item.name}: {item.reason}
                        </Text>
                      ))}
                    </>
                  )}
                  <Text size={200} className="secondary">
                    {t("Secret keys go to Credential Manager. Nothing runs after uploads that you didn’t set up in Aktar.")}
                  </Text>
                </>
              )}

              {imported && (
                <>
                  {imported.destinations.map((destination) => (
                    <div key={destination.id} className="form-group">
                      <Text weight="semibold">{destination.name}</Text>
                      {tests[destination.id]?.testing ? (
                        <Spinner size="tiny" label={t("Testing…")} />
                      ) : (
                        <ConnectionTestResult result={tests[destination.id]?.result ?? null} error={tests[destination.id]?.error ?? null} />
                      )}
                    </div>
                  ))}
                  {imported.shorteners > 0 && <Text>{t("Link shorteners set up: {0}", imported.shorteners)}</Text>}
                  {imported.historyImported > 0 && <Text>{t("Uploads added to history: {0}", imported.historyImported)}</Text>}
                  {imported.historyDuplicates > 0 && (
                    <Text size={200} className="secondary">
                      {t("{0} uploads were in history already.", imported.historyDuplicates)}
                    </Text>
                  )}
                  {imported.failures.map((failure, index) => (
                    <Text key={`${failure.name}-${index}`} size={200} className="text-error">
                      {failure.name}: {failure.message}
                    </Text>
                  ))}
                  {imported.destinations.length === 0 && imported.shorteners === 0 && imported.historyImported === 0 && imported.failures.length === 0 && (
                    <Text>{t("Nothing new to import.")}</Text>
                  )}
                </>
              )}
            </DialogContent>
            <DialogActions>
              {imported ? (
                <Button appearance="primary" type="submit">
                  {t("Done")}
                </Button>
              ) : (
                <>
                  {!pick && (
                    <Button appearance="subtle" onClick={() => start(sharex.pickFile, true)} disabled={loading || importing}>
                      {t("Choose File…")}
                    </Button>
                  )}
                  <Button appearance="secondary" onClick={onClose}>
                    {t("Cancel")}
                  </Button>
                  <Button appearance="primary" type="submit" disabled={!review || importing || missing || !somethingToImport}>
                    {importing ? t("Importing…") : t("Import")}
                  </Button>
                </>
              )}
            </DialogActions>
          </DialogBody>
        </form>
      </DialogSurface>
    </Dialog>
  );
}
