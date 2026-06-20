mod common;

use agent_status_lib::scanner;
use chrono::{DateTime, Utc};

#[test]
fn scans_multiple_projects_and_splits_models() {
    let tmp = tempfile::tempdir().unwrap();
    let claude = tmp.path().join("claude");
    let zai = tmp.path().join("zai");
    std::fs::create_dir_all(&zai).unwrap();

    let now: DateTime<Utc> = DateTime::parse_from_rfc3339("2026-06-17T20:00:00Z")
        .unwrap()
        .with_timezone(&Utc);

    common::write_session(
        &claude,
        "proj-a",
        &[
            common::usage_line("2026-06-17T19:00:00Z", "s1", "claude-opus-4-7", 1000, 500),
            common::usage_line("2026-06-17T19:05:00Z", "s1", "claude-opus-4-7", 200, 100),
        ],
    );
    common::write_session(
        &claude,
        "proj-b",
        &[common::usage_line(
            "2026-06-16T10:00:00Z",
            "s2",
            "claude-haiku-4-5",
            5000,
            1000,
        )],
    );

    let snap = scanner::scan(&claude, &zai, "max5x", now);

    assert_eq!(snap.meta.files_scanned, 2);
    assert_eq!(snap.week.len(), 7);
    assert_eq!(snap.models.len(), 3);

    // Two distinct sessions tracked; rows are no longer padded to a fixed
    // length, and with no z.ai logs there's no GLM summary row.
    assert_eq!(snap.providers[0].sessions, 2);
    assert_eq!(snap.sessions.len(), 2);
    // Newest first: proj-a's s1 (19:05 on 06-17) precedes proj-b's s2 (06-16).
    assert_eq!(snap.sessions[0].provider, "claude");
    assert_eq!(snap.sessions[0].model, "opus");
    assert_eq!(snap.sessions[1].model, "haiku");

    // Opus and Haiku both have non-zero token strings.
    let opus = snap.models.iter().find(|m| m.key == "opus").unwrap();
    let haiku = snap.models.iter().find(|m| m.key == "haiku").unwrap();
    assert_ne!(opus.tokens, "0");
    assert_ne!(haiku.tokens, "0");

    // Session bucket has a reset countdown and is healthy at this volume.
    assert_eq!(snap.limits.buckets.len(), 3);
    assert_eq!(snap.limits.buckets[0].name, "Session");
}
