import { Text } from "@fluentui/react-components";

import type { Job } from "../lib/api";
import { useI18n } from "../lib/i18n";

/** What an upload under way is doing, when it's more than sending bytes:
 * downloading a copied link, continuing an earlier upload, or going up with
 * its link already copied. */
export function JobStatusLine({ job, size = 100 }: { job: Job; size?: 100 | 200 }) {
  const { t } = useI18n();
  let text: string | null = null;
  if (job.downloading) text = t("Downloading the link…");
  else if (job.state.kind === "uploading" && job.state.resuming) text = t("Resuming upload…");
  else if (job.linkCopied) text = t("Link copied, uploading…");
  if (!text) return null;
  return (
    <Text size={size} className="secondary">
      {text}
    </Text>
  );
}
