import { Button, Link, ProgressBar, Spinner, Text } from "@fluentui/react-components";
import { CheckmarkCircleRegular, ErrorCircleRegular } from "@fluentui/react-icons";

import { api } from "../lib/api";
import { useUpdateStatus } from "../lib/hooks";
import { useI18n } from "../lib/i18n";
import appIcon from "../../src-tauri/icons/128x128.png";
import { links } from "../lib/links";

/** The Windows counterpart of Sparkle's update alert. */
export default function Update() {
  const { t, info } = useI18n();
  const [status] = useUpdateStatus();
  const close = () => api.closeWindow("update");
  const releaseNotes = (version: string) => `${links.releases}/tag/v${version}`;

  const body = (() => {
    switch (status.kind) {
      case "idle":
      case "checking":
        return (
          <div className="update-center">
            <Spinner label={t("Checking for updates…")} />
          </div>
        );
      case "upToDate":
        return (
          <>
            <Text size={500} weight="semibold">
              <CheckmarkCircleRegular className="text-success" /> {t("You’re up to date!")}
            </Text>
            <Text>{t("Aktar {0} is currently the newest version available.", info?.version ?? "")}</Text>
            <div className="update-actions">
              <Button appearance="primary" onClick={close}>
                {t("OK")}
              </Button>
            </div>
          </>
        );
      case "available":
        return (
          <>
            <Text size={500} weight="semibold">
              {t("A new version of Aktar is available!")}
            </Text>
            <Text>{t("Aktar {0} is available. You have {1}. Would you like to install it now?", status.version, info?.version ?? "")}</Text>
            {status.notes && <Text className="update-notes selectable">{status.notes}</Text>}
            <Link onClick={() => api.openUrl(releaseNotes(status.version))}>{t("What’s New")}</Link>
            <div className="update-actions">
              <Button onClick={close}>{t("Remind Me Later")}</Button>
              <Button appearance="primary" onClick={() => api.installUpdate()}>
                {t("Install and Relaunch")}
              </Button>
            </div>
          </>
        );
      case "downloading":
        return (
          <>
            <Text size={500} weight="semibold">
              {t("Downloading Aktar {0}…", status.version)}
            </Text>
            <ProgressBar thickness="large" value={status.progress ?? undefined} />
          </>
        );
      case "readyToInstall":
        return (
          <>
            <Text size={500} weight="semibold">
              {t("Aktar {0} is ready to install", status.version)}
            </Text>
            <Text>{t("It will be installed the next time you quit Aktar.")}</Text>
            <div className="update-actions">
              <Button onClick={close}>{t("Later")}</Button>
              <Button appearance="primary" onClick={() => api.installUpdate()}>
                {t("Install and Relaunch")}
              </Button>
            </div>
          </>
        );
      case "failed":
        return (
          <>
            <Text size={500} weight="semibold">
              <ErrorCircleRegular className="text-error" /> {t("Update check failed")}
            </Text>
            <Text className="selectable">{status.message}</Text>
            <div className="update-actions">
              <Button onClick={close}>{t("Cancel")}</Button>
              <Button appearance="primary" onClick={() => api.checkForUpdates()}>
                {t("Try Again")}
              </Button>
            </div>
          </>
        );
    }
  })();

  return (
    <div className="update">
      <img src={appIcon} alt="" width={64} height={64} draggable={false} />
      <div className="update-body">{body}</div>
    </div>
  );
}
