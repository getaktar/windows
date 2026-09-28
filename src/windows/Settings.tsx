import {
  Button,
  Input,
  Menu,
  MenuItem,
  MenuList,
  MenuPopover,
  MenuTrigger,
  MenuDivider,
  Radio,
  RadioGroup,
  Select,
  Tab,
  TabList,
  Text,
} from "@fluentui/react-components";
import {
  AddRegular,
  CircleFilled,
  CircleRegular,
  CodeRegular,
  CloudRegular,
  CopyRegular,
  EyeOffRegular,
  EyeRegular,
  GlobeRegular,
  InfoRegular,
  MoreHorizontalRegular,
  OpenRegular,
  PersonCircleRegular,
  PlugConnectedRegular,
  PuzzlePieceRegular,
  SettingsRegular,
  SparkleRegular,
  BugRegular,
  DrinkCoffeeRegular,
  WarningFilled,
  DocumentCopyRegular,
  CloudAddRegular,
} from "@fluentui/react-icons";
import { useEffect, useState, type ReactElement } from "react";

import { ConfirmDialog } from "../components/Dialogs";
import { DestinationForm } from "../components/DestinationForm";
import { ProviderIcon } from "../components/FileVisuals";
import { Card, CardRow, SettingsPage, SettingsSection, ToggleRow } from "../components/SettingsLayout";
import { ShortcutRecorder } from "../components/ShortcutRecorder";
import { api, errorMessage, type DestinationConfig, type OutputMode } from "../lib/api";
import { providerName } from "../lib/format";
import { useDestinations, useFlag, useLocalApi, useSettings, useUpdateStatus } from "../lib/hooks";
import { useI18n, websiteURL } from "../lib/i18n";
import { links } from "../lib/links";
import appIcon from "../../src-tauri/icons/128x128.png";

type TabName = "general" | "destinations" | "output" | "integrations" | "about";


export default function SettingsWindow() {
  const { t, info } = useI18n();
  const [tab, setTab] = useState<TabName>("general");

  const tabs: { name: TabName; title: string; icon: ReactElement }[] = [
    { name: "general", title: t("General"), icon: <SettingsRegular /> },
    { name: "destinations", title: t("Destinations"), icon: <CloudRegular /> },
    { name: "output", title: t("Output"), icon: <DocumentCopyRegular /> },
    { name: "integrations", title: t("Integrations"), icon: <PuzzlePieceRegular /> },
    { name: "about", title: t("About"), icon: <InfoRegular /> },
  ];

  return (
    <div className="settings">
      <nav className="settings-nav">
        <Text size={500} weight="semibold" className="settings-nav-title">
          {t("Settings")}
        </Text>
        <TabList vertical size="large" selectedValue={tab} onTabSelect={(_, data) => setTab(data.value as TabName)}>
          {tabs.map((item) => (
            <Tab key={item.name} value={item.name} icon={item.icon}>
              {item.title}
            </Tab>
          ))}
        </TabList>
        <div className="spacer" />
        {info && (
          <Text size={100} className="secondary settings-nav-footer">
            Aktar {info.version}
          </Text>
        )}
      </nav>
      <main className="settings-main">
        {tab === "general" && <GeneralSettings />}
        {tab === "destinations" && <DestinationsSettings />}
        {tab === "output" && <OutputSettings />}
        {tab === "integrations" && <IntegrationsSettings />}
        {tab === "about" && <AboutSettings />}
      </main>
    </div>
  );
}

// MARK: - General

function GeneralSettings() {
  const { t, info, reload } = useI18n();
  const [settings] = useSettings();
  const [updateStatus] = useUpdateStatus();
  const [launchAtLogin, setLaunchAtLogin] = useState<boolean | null>(null);
  const [shortcutError, setShortcutError] = useState<string | null>(null);

  useEffect(() => {
    api.getLaunchAtLogin().then(setLaunchAtLogin).catch(() => setLaunchAtLogin(false));
  }, []);

  if (!settings) return null;
  const checking = updateStatus.kind === "checking" || updateStatus.kind === "downloading";

  return (
    <SettingsPage title={t("General")} subtitle={t("Control how the app behaves.")}>
      <SettingsSection title={t("Language")}>
        <Card>
          <CardRow title={t("App language")} subtitle={t("Follows your Windows display language unless you pick one here.")}>
            <Select
              value={settings.language ?? ""}
              onChange={async (_, data) => {
                await api.setLanguage(data.value || null);
                reload();
              }}
            >
              <option value="">{t("System Default")}</option>
              {info?.languages.map(([code, name]) => (
                <option key={code} value={code}>
                  {name}
                </option>
              ))}
            </Select>
          </CardRow>
        </Card>
      </SettingsSection>

      <SettingsSection title={t("Updates")}>
        <Card>
          <ToggleRow
            title={t("Automatically check for updates")}
            subtitle={t("Look for a new version on GitHub once a day")}
            checked={settings.autoCheckUpdates}
            onChange={(value) => api.updateSettings({ autoCheckUpdates: value })}
          />
          <ToggleRow
            title={t("Automatically install updates")}
            subtitle={t("Download new versions in the background and install them when Aktar quits")}
            checked={settings.autoInstallUpdates}
            disabled={!settings.autoCheckUpdates}
            onChange={(value) => api.updateSettings({ autoInstallUpdates: value })}
          />
        </Card>
        <div>
          <Button disabled={checking} onClick={() => api.checkForUpdates()}>
            {t("Check for Updates…")}
          </Button>
        </div>
      </SettingsSection>

      <SettingsSection title={t("Startup")}>
        <Card>
          <ToggleRow
            title={t("Launch at login")}
            subtitle={t("Open the app automatically when you sign in")}
            checked={launchAtLogin ?? false}
            disabled={launchAtLogin === null}
            onChange={async (value) => {
              try {
                setLaunchAtLogin(await api.setLaunchAtLogin(value));
              } catch {
                setLaunchAtLogin(!value);
              }
            }}
          />
        </Card>
      </SettingsSection>

      <SettingsSection title={t("Keyboard shortcut")}>
        <Card>
          <CardRow
            title={t("Paste & upload from anywhere")}
            subtitle={t("Works even when the panel is closed. Click to record, or press Delete to clear it.")}
          >
            <ShortcutRecorder
              value={settings.shortcut}
              error={shortcutError}
              onChange={async (accelerator) => {
                try {
                  await api.setShortcut(accelerator);
                  setShortcutError(null);
                } catch (error) {
                  setShortcutError(errorMessage(error));
                }
              }}
            />
          </CardRow>
        </Card>
      </SettingsSection>

      <SettingsSection title={t("Uploads")}>
        <Card>
          <ToggleRow
            title={t("Show notification after upload")}
            subtitle={t("Notify me when an upload completes")}
            checked={settings.showNotification}
            onChange={(value) => api.updateSettings({ showNotification: value })}
          />
          <ToggleRow
            title={t("Close panel after upload")}
            subtitle={t("Automatically close after a successful upload")}
            checked={settings.closePanelAfterUpload}
            onChange={(value) => api.updateSettings({ closePanelAfterUpload: value })}
          />
        </Card>
      </SettingsSection>
    </SettingsPage>
  );
}

// MARK: - Destinations

function DestinationsSettings() {
  const { t } = useI18n();
  const [destinations] = useDestinations();
  const [editing, setEditing] = useState<DestinationConfig | null>(null);
  const [formOpen, setFormOpen] = useState(false);
  const [removing, setRemoving] = useState<DestinationConfig | null>(null);

  const openForm = (destination: DestinationConfig | null) => {
    setEditing(destination);
    setFormOpen(true);
  };

  return (
    <SettingsPage title={t("Destinations")} subtitle={t("Manage where your files are uploaded.")}>
      {destinations.length === 0 ? (
        <div className="empty-state">
          <CloudAddRegular fontSize={36} className="secondary" />
          <Text weight="semibold" size={400}>
            {t("No destinations yet")}
          </Text>
          <Text className="secondary pre-line" align="center">
            {t("Connect an S3-compatible storage provider\nto start uploading files.")}
          </Text>
          <Button appearance="primary" onClick={() => openForm(null)}>
            {t("Add Destination")}
          </Button>
        </div>
      ) : (
        <>
          <SettingsSection title={t("Upload destinations")}>
            <Card>
              {destinations.map((destination) => (
                <CardRow
                  key={destination.id}
                  icon={<ProviderIcon preset={destination.preset} />}
                  title={destination.name}
                  subtitle={`${providerName(destination.preset, t)} · ${destination.bucket}`}
                >
                  {destination.isDefault && (
                    <Text size={200} className="secondary">
                      {t("Default")}
                    </Text>
                  )}
                  <Menu>
                    <MenuTrigger disableButtonEnhancement>
                      <Button appearance="subtle" icon={<MoreHorizontalRegular />} aria-label={t("More")} />
                    </MenuTrigger>
                    <MenuPopover>
                      <MenuList>
                        <MenuItem onClick={() => openForm(destination)}>{t("Edit")}</MenuItem>
                        {!destination.isDefault && (
                          <MenuItem onClick={() => api.setDefaultDestination(destination.id)}>{t("Set as Default")}</MenuItem>
                        )}
                        <MenuDivider />
                        <MenuItem className="menu-destructive" onClick={() => setRemoving(destination)}>
                          {t("Remove")}
                        </MenuItem>
                      </MenuList>
                    </MenuPopover>
                  </Menu>
                </CardRow>
              ))}
            </Card>
          </SettingsSection>
          <div>
            <Button icon={<AddRegular />} onClick={() => openForm(null)}>
              {t("Add Destination")}
            </Button>
          </div>
        </>
      )}

      <DestinationForm
        open={formOpen}
        existing={editing}
        onCancel={() => setFormOpen(false)}
        onSaved={() => setFormOpen(false)}
      />
      <ConfirmDialog
        open={removing !== null}
        title={t("Remove “{0}”?", removing?.name ?? "")}
        message={t("Aktar forgets this destination and its keys. Files already in the bucket aren’t touched.")}
        confirmLabel={t("Remove")}
        destructive
        onCancel={() => setRemoving(null)}
        onConfirm={() => {
          if (removing) api.removeDestination(removing.id);
          setRemoving(null);
        }}
      />
    </SettingsPage>
  );
}

// MARK: - Output

function OutputSettings() {
  const { t } = useI18n();
  const [settings] = useSettings();
  const [template, setTemplate] = useState<string | null>(null);

  useEffect(() => {
    if (settings && template === null) setTemplate(settings.customTemplate);
  }, [settings, template]);

  if (!settings) return null;

  const options: { mode: OutputMode; title: string; example: string }[] = [
    { mode: "url", title: "URL", example: "https://cdn.example.com/image.png" },
    { mode: "markdown", title: "Markdown", example: "![](https://cdn.example.com/image.png)" },
    { mode: "html", title: "HTML", example: '<img src="https://cdn.example.com/...">' },
    { mode: "custom", title: t("Custom"), example: t("Define your own template.") },
  ];

  return (
    <SettingsPage title={t("Output")} subtitle={t("Choose what is copied after an upload.")}>
      <SettingsSection title={t("Copied format")}>
        <Text size={200} className="secondary">
          {t("Choose what gets copied to your clipboard after a successful upload.")}
        </Text>
        <Card>
          <RadioGroup
            value={settings.outputMode}
            onChange={(_, data) => api.updateSettings({ outputMode: data.value as OutputMode })}
            className="output-options"
          >
            {options.map((option) => (
              <Radio
                key={option.mode}
                value={option.mode}
                label={
                  <span className="output-option">
                    <Text>{option.title}</Text>
                    <Text size={200} className="secondary ellipsis">
                      {option.example}
                    </Text>
                  </span>
                }
              />
            ))}
          </RadioGroup>
          {settings.outputMode === "custom" && (
            <div className="card-block">
              <Text size={200} className="secondary">
                {t("Template")}
              </Text>
              <Input
                value={template ?? ""}
                onChange={(_, data) => setTemplate(data.value)}
                onBlur={() => template !== null && api.updateSettings({ customTemplate: template })}
                onKeyDown={(event) => {
                  if (event.key === "Enter" && template !== null) api.updateSettings({ customTemplate: template });
                }}
              />
              <Text size={100} className="secondary">
                {t("Available variables: {url} {filename} {name} {ext}")}
              </Text>
            </div>
          )}
        </Card>
      </SettingsSection>
    </SettingsPage>
  );
}

// MARK: - Integrations

function IntegrationsSettings() {
  const { t, language } = useI18n();
  const [state] = useLocalApi();
  const [port, setPort] = useState("");
  const [tokenVisible, setTokenVisible] = useState(false);
  const [copied, flashCopied] = useFlag();
  const [portError, setPortError] = useState<string | null>(null);

  useEffect(() => {
    if (state) setPort(String(state.port));
  }, [state?.port]);

  if (!state) return null;

  const applyPort = async () => {
    const value = Number.parseInt(port.trim(), 10);
    if (!Number.isInteger(value) || value < 1024 || value > 65535) {
      setPort(String(state.port));
      setPortError(t("The port must be a number between 1024 and 65535."));
      return;
    }
    setPortError(null);
    try {
      await api.setLocalApiPort(value);
    } catch (error) {
      setPortError(errorMessage(error));
    }
  };

  const status = (() => {
    switch (state.status.kind) {
      case "off":
        return (
          <span className="status">
            <CircleRegular /> {t("Off")}
          </span>
        );
      case "starting":
        return <span className="status">{t("Starting…")}</span>;
      case "running":
        return (
          <span className="status">
            <CircleFilled className="text-success" /> 127.0.0.1:{state.port}
          </span>
        );
      case "failed":
        return (
          <span className="status" title={state.status.message}>
            <WarningFilled className="text-warning" /> {state.status.message}
          </span>
        );
    }
  })();

  return (
    <SettingsPage title={t("Integrations")} subtitle={t("Use Aktar from other apps on this PC.")}>
      <SettingsSection title="Raycast">
        <Card>
          <CardRow
            icon={<PuzzlePieceRegular fontSize={24} className="accent" />}
            title={t("Aktar for Raycast")}
            subtitle={t(
              "Upload files and the clipboard, search your history, and browse your buckets without opening Aktar. Run “Connect to Aktar” in Raycast to pair it in one click.",
            )}
          >
            <Button icon={<OpenRegular />} onClick={() => api.openUrl(`${websiteURL(language)}raycast/`)}>
              {t("Get Extension")}
            </Button>
          </CardRow>
        </Card>
      </SettingsSection>

      <SettingsSection title={t("Local API")}>
        <Card>
          <ToggleRow
            title={t("Allow local connections")}
            subtitle={t("Listen on 127.0.0.1 so the Raycast extension can reach Aktar. Requests need the token below.")}
            checked={state.enabled}
            onChange={(value) => api.setLocalApiEnabled(value)}
          />
          <CardRow title={t("Status")}>{status}</CardRow>
          <CardRow
            title={t("Port")}
            subtitle={portError ?? t("Change it only if another app already uses this one.")}
          >
            <Input
              className="port-input"
              value={port}
              inputMode="numeric"
              onChange={(_, data) => setPort(data.value.replace(/\D/g, ""))}
              onBlur={applyPort}
              onKeyDown={(event) => event.key === "Enter" && applyPort()}
              aria-label={t("Port")}
            />
          </CardRow>
          <CardRow
            title={t("Token")}
            subtitle={t("Paste it into the extension’s preferences if you don’t use “Connect to Aktar”.")}
          >
            <code className="token">{tokenVisible ? state.token || "-" : "•".repeat(12)}</code>
            <Button
              appearance="subtle"
              icon={tokenVisible ? <EyeOffRegular /> : <EyeRegular />}
              title={tokenVisible ? t("Hide token") : t("Show token")}
              aria-label={tokenVisible ? t("Hide token") : t("Show token")}
              onClick={() => setTokenVisible(!tokenVisible)}
            />
            <Button
              icon={<CopyRegular />}
              disabled={!state.token}
              onClick={() => {
                api.copyText(state.token);
                flashCopied();
              }}
            >
              {copied ? t("Copied") : t("Copy")}
            </Button>
            <Button
              title={t("Create a new token. Anything already connected will have to connect again.")}
              onClick={() => api.regenerateApiToken()}
            >
              {t("Regenerate")}
            </Button>
          </CardRow>
        </Card>
      </SettingsSection>
    </SettingsPage>
  );
}

// MARK: - About

function LinkRow({ title, detail, icon, url }: { title: string; detail?: string; icon: ReactElement; url: string }) {
  return (
    <button type="button" className="card-row link-row" onClick={() => api.openUrl(url)} title={url}>
      <div className="card-row-icon secondary">{icon}</div>
      <div className="card-row-text">
        <Text>{title}</Text>
      </div>
      {detail && (
        <Text size={200} className="secondary">
          {detail}
        </Text>
      )}
      <OpenRegular className="secondary" />
    </button>
  );
}

function AboutSettings() {
  const { t, info, language } = useI18n();
  const [updateStatus] = useUpdateStatus();
  const website = websiteURL(language);
  const checking = updateStatus.kind === "checking" || updateStatus.kind === "downloading";

  return (
    <SettingsPage title={t("About")} subtitle={t("Version details, links, and who makes Aktar.")}>
      <div className="about-header">
        <img src={appIcon} alt="" width={64} height={64} draggable={false} />
        <div className="about-text">
          <Text size={500} weight="semibold">
            Aktar
          </Text>
          <Text className="secondary">{t("Your files. Your storage. One shortcut away.")}</Text>
          {info && (
            <Text size={200} className="secondary selectable">
              {t("Version {0}", info.version)}
            </Text>
          )}
        </div>
        <div className="spacer" />
        <Button disabled={checking} onClick={() => api.checkForUpdates()}>
          {t("Check for Updates…")}
        </Button>
      </div>

      <SettingsSection title="Aktar">
        <Card>
          <LinkRow title={t("Website")} detail="getaktar.com" icon={<GlobeRegular />} url={website} />
          <LinkRow title={t("Source Code")} icon={<CodeRegular />} url={links.repository} />
          <LinkRow title={t("What’s New")} icon={<SparkleRegular />} url={links.releases} />
          <LinkRow title={t("Report an Issue")} icon={<BugRegular />} url={links.issues} />
        </Card>
      </SettingsSection>

      <SettingsSection title={t("Developer")}>
        <Card>
          <LinkRow title="Mert Topuz" detail="merttopuz.com" icon={<PersonCircleRegular />} url={links.developerWebsite} />
          <LinkRow title="GitHub" detail="@merttopuz" icon={<PlugConnectedRegular />} url={links.developerGitHub} />
          <LinkRow title={t("Buy Me a Coffee")} icon={<DrinkCoffeeRegular />} url={links.sponsor} />
        </Card>
      </SettingsSection>

      <Text size={200} className="secondary">
        {t("Free and open source under the MIT License.")}
      </Text>
    </SettingsPage>
  );
}
