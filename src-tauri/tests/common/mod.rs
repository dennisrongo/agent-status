//! Shared test helpers.

use std::io::Write;
use std::path::Path;

/// Write a `.jsonl` session log under `<root>/<project>/session.jsonl`.
pub fn write_session(root: &Path, project: &str, lines: &[String]) {
    let pdir = root.join(project);
    std::fs::create_dir_all(&pdir).unwrap();
    let mut f = std::fs::File::create(pdir.join("session.jsonl")).unwrap();
    for l in lines {
        writeln!(f, "{l}").unwrap();
    }
}

/// Build a single usage JSONL line.
pub fn usage_line(ts: &str, session: &str, model: &str, input: u64, output: u64) -> String {
    format!(
        r#"{{"timestamp":"{ts}","sessionId":"{session}","message":{{"model":"{model}","usage":{{"input_tokens":{input},"output_tokens":{output},"cache_creation_input_tokens":0,"cache_read_input_tokens":0}}}}}}"#
    )
}
