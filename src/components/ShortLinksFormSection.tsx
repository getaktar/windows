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
  Textarea,
} from "@fluentui/react-components";
import { useState } from "react";

import {
  api,
  errorMessage,
  type ShareXFailure,
  type ShareXImport,
  type ShareXSecretLocation,
  type ShortLinkBodyType,
  type ShortLinkTestResult,
} from "../lib/api";
import { useShortLinkProviders } from "../lib/hooks";
import { useI18n, type Translate } from "../lib/i18n";
import {
  applyShareXImport,
  canExpire,
  customMethods,
  customProviderId,
  deleteMethods,
  formDefinition,
  formProblem,
  formSettings,
  isCustom,
  usesEndpoint,
  usesHTTP,
  type ShortLinkFormState,
} from "../lib/shortLinks";
import { AlertDialog } from "./Dialogs";

type TestState = { kind: "testing" } | { kind: "passed"; result: ShortLinkTestResult } | { kind: "failed"; message: string };

/** "Short Links" in the destination form: Off or a shortener, its address,
 * domain and key, Test, a ShareX import, and when links are shortened. */
export function ShortLinksFormSection({
  state,
  onChange,
  destinationId,
  savedTokenProvider,
}: {
  state: ShortLinkFormState;
  onChange: (state: ShortLinkFormState) => void;
  /** The destination being edited, for testing with its saved token. */
  destinationId: string | null;
  /** The provider whose token is saved for this destination, if any. */
  savedTokenProvider: string | null;
}) {
  const { t } = useI18n();
  const providers = useShortLinkProviders();
  const [test, setTest] = useState<TestState | null>(null);
  /** A ShareX configuration waiting for the user's consent. */
  const [pendingImport, setPendingImport] = useState<ShareXImport | null>(null);
  const [importError, setImportError] = useState<string | null>(null);
  /** The last import was refused for http://, so the toggle that allows
   * it is shown. */
  const [importNeedsInsecureHTTP, setImportNeedsInsecureHTTP] = useState(false);

  const hasSavedToken = savedTokenProvider !== null && savedTokenProvider === state.providerId;
  const definition = formDefinition(state, providers, t);
  const problem = formProblem(state, providers, hasSavedToken, t);
  const custom = isCustom(state);
  const expires = canExpire(definition);
  const set = (change: Partial<ShortLinkFormState>) => onChange({ ...state, ...change });

  const runTest = async () => {
    const settings = formSettings(state, providers, t);
    if (!settings) return;
    setTest({ kind: "testing" });
    try {
      setTest({ kind: "passed", result: await api.testShortLinks(destinationId, settings, state.token.trim() || null) });
    } catch (error) {
      setTest({ kind: "failed", message: errorMessage(error) });
    }
  };

  /** Reads a .sxcu file and asks before anything changes. */
  const chooseShareXFile = async () => {
    try {
      const imported = await api.pickShareXConfiguration(state.allowInsecureHTTP);
      if (imported) setPendingImport(imported);
    } catch (error) {
      const failure = error as Partial<ShareXFailure>;
      if (failure?.insecure) setImportNeedsInsecureHTTP(true);
      setImportError(typeof failure?.message === "string" ? failure.message : errorMessage(error));
    }
  };

  const insecureToggle = (
    <>
      <Switch label={t("Allow insecure HTTP")} checked={state.allowInsecureHTTP} onChange={(_, data) => set({ allowInsecureHTTP: data.checked })} />
      <Text size={200} className="text-warning">
        {t("Links and your API key are sent unencrypted over http://. Use this only on a network you trust, such as your own computer.")}
      </Text>
    </>
  );

  return (
    <>
      <Text weight="semibold" className="form-heading">
        {t("Short Links")}
      </Text>
      <div className="form-group">
        <Field label={t("Shortener")}>
          <Select
            value={state.providerId ?? ""}
            onChange={(_, data) => {
              set({ providerId: data.value || null });
              setTest(null);
            }}
          >
            <option value="">{t("Off")}</option>
            {(providers ?? []).map((provider) => (
              <option key={provider.id} value={provider.id}>
                {provider.name}
              </option>
            ))}
            <option value={customProviderId}>{t("Custom HTTP…")}</option>
          </Select>
        </Field>
        {state.providerId !== null ? (
          <>
            {(definition ? usesEndpoint(definition) : custom) && (
              <Field label={t("Address")}>
                <Input value={state.endpoint} placeholder="https://s.example.com" spellCheck={false} onChange={(_, data) => set({ endpoint: data.value })} />
              </Field>
            )}
            {(usesHTTP(state) || importNeedsInsecureHTTP) && insecureToggle}
            {definition && (definition.needsDomain || definition.capabilities.customDomain) && (
              <Field label={t("Domain")}>
                <Input
                  value={state.domain}
                  placeholder={definition.needsDomain ? "short.example.com" : t("Optional")}
                  spellCheck={false}
                  onChange={(_, data) => set({ domain: data.value })}
                />
              </Field>
            )}
            <Field label={custom ? t("Token") : t("API Key")}>
              <Input
                type="password"
                value={state.token}
                autoComplete="off"
                placeholder={hasSavedToken ? t("Unchanged") : definition && !definition.auth ? t("Optional") : undefined}
                onChange={(_, data) => set({ token: data.value })}
              />
            </Field>
            {custom && <CustomFields state={state} set={set} />}
            <Field label={t("Only shorten links longer than")}>
              <div className="inline-row">
                <Input
                  type="number"
                  min={0}
                  style={{ width: 90 }}
                  value={String(state.onlyLongerThan)}
                  onChange={(_, data) => set({ onlyLongerThan: Math.max(0, Math.floor(Number(data.value)) || 0) })}
                />
                <Text className="secondary">{t("characters")}</Text>
              </div>
            </Field>
            <Switch
              label={t("Also shorten temporary links")}
              checked={state.shortenTemporaryLinks && expires}
              disabled={!expires}
              onChange={(_, data) => set({ shortenTemporaryLinks: data.checked })}
            />
            <div className="inline-row wrap-buttons">
              <Button size="small" disabled={test?.kind === "testing" || problem !== null || !definition} onClick={runTest}>
                {test?.kind === "testing" ? t("Testing…") : t("Test")}
              </Button>
              {test && <TestOutcome test={test} />}
            </div>
            {problem && (
              <Text size={200} className="text-error">
                {problem}
              </Text>
            )}
          </>
        ) : (
          importNeedsInsecureHTTP && insecureToggle
        )}
        <div>
          <Button size="small" onClick={chooseShareXFile}>
            {t("Import ShareX Configuration (.sxcu)…")}
          </Button>
        </div>
        <Text size={200} className="secondary">
          {t(
            "After each upload, the link is shortened with your own link shortener and the short link is copied. If it can’t be, the original link is copied and you’re told. Your links and click data stay in your infrastructure.",
          )}
        </Text>
        {definition && (
          <>
            {definition.kind === "hosted" && (
              <Text size={200} className="secondary">
                {t("This service sees every link you shorten and every click.")}
              </Text>
            )}
            {state.shortenTemporaryLinks && expires ? (
              <Text size={200} className="secondary">
                {t("Short link will expire together with the original temporary URL.")}
              </Text>
            ) : (
              !expires && (
                <Text size={200} className="secondary">
                  {t("This shortener can’t expire links, so temporary links aren’t shortened.")}
                </Text>
              )
            )}
            {custom && (
              <Text size={200} className="secondary">
                {t(
                  "Use {url} for the link and {token} for the token in the path, headers, query or body. The short link and ID are read from the JSON answer with dot paths, such as data.link or items.0.id; deleting needs both the delete path and the ID.",
                )}
              </Text>
            )}
          </>
        )}
      </div>
      <ShareXConsentDialog
        imported={pendingImport}
        onConfirm={() => {
          if (pendingImport) onChange(applyShareXImport(state, pendingImport));
          setImportNeedsInsecureHTTP(false);
          setTest(null);
          setPendingImport(null);
        }}
        onCancel={() => setPendingImport(null)}
      />
      <AlertDialog
        open={importError !== null}
        title={t("Couldn’t Import the Configuration")}
        message={importError ?? ""}
        onClose={() => setImportError(null)}
      />
    </>
  );
}

function CustomFields({ state, set }: { state: ShortLinkFormState; set: (change: Partial<ShortLinkFormState>) => void }) {
  const { t } = useI18n();
  return (
    <>
      <Field label={t("Method")}>
        <Select value={state.customMethod} onChange={(_, data) => set({ customMethod: data.value })}>
          {customMethods.map((method) => (
            <option key={method} value={method}>
              {method}
            </option>
          ))}
        </Select>
      </Field>
      <Field label={t("Path or URL")}>
        <Input value={state.customPath} placeholder="/api/shorten" spellCheck={false} onChange={(_, data) => set({ customPath: data.value })} />
      </Field>
      <Field label={t("Headers")}>
        <Textarea
          className="mono"
          value={state.customHeaders}
          placeholder="Authorization: Bearer {token}"
          resize="vertical"
          spellCheck={false}
          onChange={(_, data) => set({ customHeaders: data.value })}
        />
      </Field>
      <Field label={t("Query")}>
        <Textarea
          className="mono"
          value={state.customQuery}
          placeholder={t("Optional")}
          resize="vertical"
          spellCheck={false}
          onChange={(_, data) => set({ customQuery: data.value })}
        />
      </Field>
      <Field label={t("Body")}>
        <Select
          value={state.customBodyType ?? ""}
          onChange={(_, data) => set({ customBodyType: (data.value || null) as ShortLinkBodyType | null })}
        >
          <option value="">{t("None")}</option>
          <option value="json">JSON</option>
          <option value="form">{t("Form")}</option>
        </Select>
      </Field>
      {state.customBodyType && (
        <Textarea
          className="mono"
          aria-label={t("Body")}
          value={state.customBody}
          placeholder={'{"url": "{url}"}'}
          resize="vertical"
          spellCheck={false}
          onChange={(_, data) => set({ customBody: data.value })}
        />
      )}
      <Field label={t("Short link in the answer")}>
        <Input value={state.customShortUrlPath} placeholder="data.shortUrl" spellCheck={false} onChange={(_, data) => set({ customShortUrlPath: data.value })} />
      </Field>
      <Field label={t("ID in the answer")}>
        <Input value={state.customIdPath} placeholder={t("Optional")} spellCheck={false} onChange={(_, data) => set({ customIdPath: data.value })} />
      </Field>
      <Field label={t("Delete path")}>
        <Input value={state.customDeletePath} placeholder="/api/links/{id}" spellCheck={false} onChange={(_, data) => set({ customDeletePath: data.value })} />
      </Field>
      {state.customDeletePath.trim() && (
        <Field label={t("Delete method")}>
          <Select value={state.customDeleteMethod} onChange={(_, data) => set({ customDeleteMethod: data.value })}>
            {deleteMethods.map((method) => (
              <option key={method} value={method}>
                {method}
              </option>
            ))}
          </Select>
        </Field>
      )}
    </>
  );
}

function TestOutcome({ test }: { test: TestState }) {
  const { t } = useI18n();
  if (test.kind === "testing") return <Spinner size="extra-tiny" />;
  if (test.kind === "failed") {
    return (
      <Text size={200} className="text-error">
        {test.message}
      </Text>
    );
  }
  const { created, cleanup } = test.result;
  if (!created) {
    return (
      <Text size={200} className="text-success">
        {t("The shortener accepts this key")}
      </Text>
    );
  }
  return (
    <span className="test-result">
      <Text size={200} className="text-success selectable">
        {t("Created {0}", created.shortUrl)}
      </Text>
      {cleanup.kind === "deleted" && (
        <Text size={200} className="secondary">
          {t("Deleted it again with the delete request.")}
        </Text>
      )}
      {cleanup.kind === "failed" && (
        <Text size={200} className="text-warning">
          {t("Deleting it again failed: {0}", cleanup.message)}
        </Text>
      )}
    </span>
  );
}

/** "Header X-Api-Key", "Query parameter signature", "Body field api_key". */
function describe(location: ShareXSecretLocation, t: Translate) {
  switch (location.kind) {
    case "header":
      return t("Header {0}", location.name);
    case "query":
      return t("Query parameter {0}", location.name);
    case "body":
      return t("Body field {0}", location.name);
  }
}

/** "**host**" in a translated sentence, in bold. */
function withBold(text: string) {
  return text.split("**").map((part, index) => (index % 2 === 1 ? <strong key={index}>{part}</strong> : part));
}

/** Importing a ShareX configuration is a consent screen: where the token
 * and the links go, and in which headers or parameters, before anything is
 * filled in. */
function ShareXConsentDialog({ imported, onConfirm, onCancel }: { imported: ShareXImport | null; onConfirm: () => void; onCancel: () => void }) {
  const { t } = useI18n();
  return (
    <Dialog open={imported !== null} onOpenChange={(_, data) => !data.open && onCancel()}>
      <DialogSurface>
        {imported && (
          <DialogBody>
            <DialogTitle>{t("Import ShareX Configuration")}</DialogTitle>
            <DialogContent className="form-group">
              {imported.name && <Text className="secondary">{imported.name}</Text>}
              <Text>
                {withBold(
                  imported.token
                    ? t("This configuration will send your API token to **{0}**.", imported.host)
                    : t("This configuration will send your links to **{0}**.", imported.host),
                )}
              </Text>
              <div className="consent-grid">
                <Text className="secondary">{t("Method")}</Text>
                <Text className="mono">{imported.method}</Text>
                <Text className="secondary">{t("Endpoint")}</Text>
                <Text className="mono selectable break">{imported.endpoint}</Text>
                {imported.secretLocations.length > 0 && (
                  <>
                    <Text className="secondary">{t("Token sent in")}</Text>
                    <span>
                      {imported.secretLocations.map((location) => (
                        <Text key={`${location.kind}:${location.name}`} block>
                          {describe(location, t)}
                        </Text>
                      ))}
                    </span>
                  </>
                )}
              </div>
              {imported.token && (
                <Text size={200} className="secondary">
                  {t("The token is kept in Credential Manager with this destination’s keys, not in its settings.")}
                </Text>
              )}
              {imported.usesHTTP && (
                <Text size={200} className="text-warning">
                  {t("Links and your API key are sent unencrypted over http://. Use this only on a network you trust, such as your own computer.")}
                </Text>
              )}
              {imported.deletionSkipped && (
                <Text size={200} className="secondary">
                  {t("Its deletion URL isn’t a simple request, so short links made with it can’t be deleted from Aktar.")}
                </Text>
              )}
            </DialogContent>
            <DialogActions>
              <Button appearance="secondary" onClick={onCancel}>
                {t("Cancel")}
              </Button>
              <Button appearance="primary" onClick={onConfirm}>
                {t("Import")}
              </Button>
            </DialogActions>
          </DialogBody>
        )}
      </DialogSurface>
    </Dialog>
  );
}
