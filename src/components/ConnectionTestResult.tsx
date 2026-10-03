import { Text } from "@fluentui/react-components";

import type { ConnectionResult, PublicLinkCheck } from "../lib/api";
import { useI18n, type Translate } from "../lib/i18n";

/** What Test Connection found, in the destination form and after an
 * import: why it couldn't reach the bucket at all (`error`), or one line
 * per step of the test, so a bucket that takes uploads but won't serve
 * them reads as a problem instead of a success. */
export function ConnectionTestResult({ result, error }: { result: ConnectionResult | null; error: string | null }) {
  const { t } = useI18n();
  if (error) {
    return (
      <Text size={200} className="text-error">
        {error}
      </Text>
    );
  }
  if (!result) return null;
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
