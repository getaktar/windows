import { Button, Text } from "@fluentui/react-components";
import { useState } from "react";

import { DestinationForm } from "../components/DestinationForm";
import { ImportDestinationDialog } from "../components/TransferDialogs";
import { api } from "../lib/api";
import { useI18n } from "../lib/i18n";
import appIcon from "../../src-tauri/icons/128x128.png";

export default function Onboarding() {
  const { t } = useI18n();
  const [showForm, setShowForm] = useState(false);
  const [showImport, setShowImport] = useState(false);

  return (
    <div className="onboarding">
      <img src={appIcon} alt="" width={72} height={72} draggable={false} />
      <Text as="h1" size={700} weight="semibold">
        {t("Welcome")}
      </Text>
      <Text align="center">{t("Upload files to your own storage, then instantly copy a shareable URL.")}</Text>
      <Text size={200} className="secondary" align="center">
        {t("No account. No proprietary cloud. Your files stay yours.")}
      </Text>
      <div className="onboarding-actions">
        <Button appearance="primary" size="large" onClick={() => setShowForm(true)}>
          {t("Connect Storage")}
        </Button>
        <Button appearance="subtle" onClick={() => setShowImport(true)}>
          {t("Import from Another Device")}
        </Button>
      </div>
      <Text size={200} className="secondary onboarding-tray-hint" align="center">
        {t("Aktar lives in the notification area, next to the clock. If you don’t see its icon, click the arrow there and drag Aktar onto the taskbar.")}
      </Text>

      <DestinationForm
        open={showForm}
        existing={null}
        onCancel={() => setShowForm(false)}
        onSaved={() => {
          setShowForm(false);
          // Show where Aktar lives from now on. Rust closes this window
          // first, so focus moving on doesn't close the panel right away.
          api.finishOnboarding();
        }}
      />
      <ImportDestinationDialog
        open={showImport}
        onClose={() => setShowImport(false)}
        onImported={() => api.finishOnboarding()}
      />
    </div>
  );
}
