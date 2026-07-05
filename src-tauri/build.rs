// Build-time injection of the maintainer's support-bot credentials.
//
// The support-bundle delivery path posts to the maintainer's Telegram bot
// (`HomeEnergyManagerSupportBot`). The bot token and destination chat_id are
// real secrets, so — unlike the old hard-coded ntfy topic — they must NOT live
// in the public source tree. Instead `build.rs` injects them at compile time
// via `cargo:rustc-env`, where `option_env!` in `support/mod.rs` picks them up
// uniformly.
//
// Two sources, in priority order:
//
// 1. Real process environment (CI path). GitHub Actions sets `SUPPORT_BOT_TOKEN`
//    and `SUPPORT_CHAT_ID` from repository secrets on the build step.
// 2. `src-tauri/support-secrets.local` (local-dev path). A hand-edited file with
//    one `KEY=VALUE` per line. It is gitignored by the repo's existing
//    `*.local` rule, so a developer can drop real test credentials in without
//    any risk of committing them. Real env wins over the file, so a stale local
//    file can never shadow CI secrets.
//
// When neither source provides BOTH values, the build still succeeds but prints
// a `cargo:warning`; at runtime `submit_support_bundle` returns a clear "not
// configured" error rather than silently dropping the bundle.

use std::collections::HashMap;

fn main() {
    tauri_build::build();
    inject_support_credentials();
}

const SUPPORT_SECRET_KEYS: &[&str] = &["SUPPORT_BOT_TOKEN", "SUPPORT_CHAT_ID"];

fn inject_support_credentials() {
    let local = read_local_secrets();
    let mut found = 0usize;

    for key in SUPPORT_SECRET_KEYS {
        // Real env (CI) wins over the local file.
        let val = std::env::var(key)
            .ok()
            .filter(|v| !v.is_empty())
            .or_else(|| local.get(*key).cloned());
        if let Some(val) = val {
            // `cargo:rustc-env=NAME=VALUE` makes the value visible to
            // `option_env!("NAME")` at compile time.
            println!("cargo:rustc-env={key}={val}");
            found += 1;
        }
    }

    // Rebuild when either source changes.
    println!("cargo:rerun-if-changed=support-secrets.local");
    for key in SUPPORT_SECRET_KEYS {
        println!("cargo:rerun-if-env-changed={key}");
    }

    if found < SUPPORT_SECRET_KEYS.len() {
        println!(
            "cargo:warning=Support bundle delivery not fully configured ({found}/{} secrets). \
             Set SUPPORT_BOT_TOKEN and SUPPORT_CHAT_ID via the CI environment or \
             src-tauri/support-secrets.local. The Submit Support Bundle button will return a \
             'not configured' error in this build.",
            SUPPORT_SECRET_KEYS.len()
        );
    }
}

/// Parse `src-tauri/support-secrets.local` into a map.
///
/// Format: one `KEY=VALUE` per line. `#` comments and blank lines are ignored;
/// surrounding whitespace on key and value is trimmed. An absent or unreadable
/// file yields an empty map. Hand-rolled (no dotenv dependency) since there are
/// only two values and the format is trivial.
fn read_local_secrets() -> HashMap<String, String> {
    let mut map = HashMap::new();
    let Some(manifest) = std::env::var("CARGO_MANIFEST_DIR").ok() else {
        return map;
    };
    let path = std::path::Path::new(&manifest).join("support-secrets.local");
    let Ok(contents) = std::fs::read_to_string(&path) else {
        return map;
    };
    for line in contents.lines() {
        let line = line.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        if let Some((k, v)) = line.split_once('=') {
            let (k, v) = (k.trim(), v.trim());
            if !k.is_empty() && !v.is_empty() {
                map.insert(k.to_string(), v.to_string());
            }
        }
    }
    map
}
