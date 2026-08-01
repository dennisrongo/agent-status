import { useUpdater } from "../hooks/useUpdater";

/**
 * Slim banner shown only when a newer signed build is available from the
 * update endpoint. Hidden entirely otherwise (incl. dev / offline).
 */
export function UpdateBanner() {
  const { phase, version, error, install } = useUpdater();

  if (phase === "idle" || phase === "checking" || phase === "uptodate") return null;

  const busy = phase === "downloading" || phase === "ready";

  return (
    <div className={`update-banner ${phase === "error" ? "err" : ""}`} role="status">
      {phase === "error" ? (
        <span>Update failed: {error}</span>
      ) : (
        <>
          <span>
            {busy ? "Installing" : "Update available"}
            {version ? ` · v${version}` : ""}
          </span>
          <button className="update-btn" onClick={install} disabled={busy}>
            {phase === "downloading"
              ? "Downloading…"
              : phase === "ready"
                ? "Restarting…"
                : "Update & restart"}
          </button>
        </>
      )}
    </div>
  );
}
