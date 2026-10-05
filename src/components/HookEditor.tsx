import { Button, Switch, Text } from "@fluentui/react-components";
import { CodeRegular, DeleteRegular, GlobeRegular } from "@fluentui/react-icons";
import { useState } from "react";

import { api, errorMessage, type WatchHook } from "../lib/api";
import { useI18n } from "../lib/i18n";
import { PromptDialog } from "./Dialogs";

/** Webhooks and scripts run after an upload: a watched folder's Automation
 * and a destination's After Upload. `test` sends a made-up upload. */
export function HookEditor({
  hooks,
  description,
  onChange,
  test,
  onError,
}: {
  hooks: WatchHook[];
  description: string;
  onChange: (hooks: WatchHook[]) => void;
  test: (hook: WatchHook) => Promise<void>;
  onError: (message: string) => void;
}) {
  const { t } = useI18n();
  const [addingWebhook, setAddingWebhook] = useState(false);
  const [testResults, setTestResults] = useState<Record<string, string>>({});

  const update = (id: string, change: Partial<WatchHook> | null) =>
    onChange(change === null ? hooks.filter((hook) => hook.id !== id) : hooks.map((hook) => (hook.id === id ? { ...hook, ...change } : hook)));

  // Aktar opens the file dialog itself and makes the hook: a window can't
  // name a program to run.
  const addScript = async () => {
    try {
      const hook = await api.pickWatchScript();
      if (hook) onChange([...hooks, hook]);
    } catch (failure) {
      onError(errorMessage(failure));
    }
  };

  const runTest = async (hook: WatchHook) => {
    setTestResults((results) => ({ ...results, [hook.id]: t("Sending…") }));
    try {
      await test(hook);
      setTestResults((results) => ({ ...results, [hook.id]: t("It worked.") }));
    } catch (failure) {
      setTestResults((results) => ({ ...results, [hook.id]: errorMessage(failure) }));
    }
  };

  return (
    <>
      <Text size={200} className="secondary">
        {description}
      </Text>
      {hooks.map((hook) => (
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
          <Switch checked={hook.enabled} aria-label={hook.target} onChange={(_, data) => update(hook.id, { enabled: data.checked })} />
          <Button size="small" onClick={() => runTest(hook)}>
            {t("Test")}
          </Button>
          <Button
            size="small"
            appearance="subtle"
            icon={<DeleteRegular />}
            aria-label={t("Remove")}
            title={t("Remove")}
            onClick={() => update(hook.id, null)}
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
      <PromptDialog
        open={addingWebhook}
        title={t("Add Webhook")}
        message={t("Aktar sends a POST request with the upload’s details as JSON after each upload.")}
        label="URL"
        initialValue="https://"
        confirmLabel={t("Add")}
        validate={(url) => api.checkWebhookUrl(url.trim()).then(() => null, errorMessage)}
        onCancel={() => setAddingWebhook(false)}
        onConfirm={(url) => {
          setAddingWebhook(false);
          onChange([...hooks, { id: crypto.randomUUID().toUpperCase(), kind: "webhook", target: url.trim(), enabled: true }]);
        }}
      />
    </>
  );
}
