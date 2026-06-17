//! Persistent backend for WhatsApp Signal Protocol storage using rusqlite.
//!
//! Replaces `InMemoryBackend` to preserve pairing and Signal sessions across
//! restarts. Uses a separate database file (`whatsapp-store.db`) so it cannot
//! conflict with the history database.
//!
//! WHY NOT `whatsapp-rust-sqlite-storage`:
//! That crate pulls in diesel + r2d2, which shares a bundled SQLite with
//! rusqlite. The two connection pools race on the same `sqlite3` symbols,
//! producing intermittent `disk I/O error` crashes that corrupt Signal
//! sessions. This implementation uses rusqlite directly — no diesel, no
//! connection pool, no conflict.

use std::path::Path;
use std::sync::Mutex;

use async_trait::async_trait;
use bytes::Bytes;
use rusqlite::Connection;
use wacore::appstate::hash::HashState;
use wacore::appstate::processor::AppStateMutationMAC;
use wacore::store::Device;
use wacore::store::error::Result;
use wacore::store::error::StoreError;
use wacore::store::traits::{
    AppSyncStore, AppStateSyncKey, DeviceListRecord, DeviceStore,
    LidPnMappingEntry, ProtocolStore, SignalStore, TcTokenEntry,
};

/// Key-value backend using a simple SQLite table.
///
/// Schema: `CREATE TABLE wa (k TEXT PRIMARY KEY, v BLOB)`
///  - `k` = `"{ns}:{key}"`  (e.g. `"sig:identity:447700@s.whatsapp.net"`)
///  - `v` = raw bytes or JSON according to the method semantics
///
/// All operations go through rusqlite (no diesel, no r2d2). The Mutex
/// serialises concurrent access from the async event loop.
pub struct SqliteBackend {
    conn: Mutex<Connection>,
    next_device_id: std::sync::atomic::AtomicI32,
}

impl SqliteBackend {
    /// Open (or create) the database at `path`.
    pub fn open(path: &Path) -> Result<Self> {
        let conn =
            Connection::open(path)
                .map_err(|e| StoreError::Connection(Box::new(e)))?;

        conn.execute_batch(
            "CREATE TABLE IF NOT EXISTS wa (k TEXT PRIMARY KEY, v BLOB);
             PRAGMA journal_mode = WAL;",
        )
        .map_err(|e| StoreError::Database(Box::new(e)))?;

        // Load the next device id counter
        let next_id: i32 = conn
            .query_row(
                "SELECT COALESCE(CAST(v AS INTEGER), 1) FROM wa WHERE k = 'meta:next_device_id'",
                [],
                |r| r.get(0),
            )
            .unwrap_or(1);

        Ok(Self {
            conn: Mutex::new(conn),
            next_device_id: std::sync::atomic::AtomicI32::new(next_id),
        })
    }

    /// Helper: get a BLOB value by key.
    fn get(&self, ns: &str, key: &str) -> Option<Vec<u8>> {
        let sql = format!("{ns}:{key}");
        self.conn
            .lock()
            .unwrap()
            .query_row("SELECT v FROM wa WHERE k = ?1", [&sql], |r| r.get(0))
            .ok()
    }

    /// Helper: put a BLOB value.
    fn put(&self, ns: &str, key: &str, val: &[u8]) {
        let sql = format!("{ns}:{key}");
        self.conn
            .lock()
            .unwrap()
            .execute(
                "INSERT OR REPLACE INTO wa (k, v) VALUES (?1, ?2)",
                rusqlite::params![sql, val],
            )
            .ok();
    }

    /// Helper: delete a key.
    fn del(&self, ns: &str, key: &str) {
        let sql = format!("{ns}:{key}");
        self.conn
            .lock()
            .unwrap()
            .execute("DELETE FROM wa WHERE k = ?1", [&sql])
            .ok();
    }

    /// Helper: get value as JSON-deserialised.
    fn get_json<T: serde::de::DeserializeOwned>(&self, ns: &str, key: &str) -> Option<T> {
        self.get(ns, key)
            .and_then(|v| serde_json::from_slice(&v).ok())
    }

    /// Helper: put value as JSON.
    fn put_json<T: serde::Serialize>(&self, ns: &str, key: &str, val: &T) {
        if let Ok(json) = serde_json::to_vec(val) {
            self.put(ns, key, &json);
        }
    }

    /// Helper: delete all keys matching a prefix.
    fn del_prefix(&self, prefix: &str) {
        let pattern = format!("{prefix}%");
        self.conn
            .lock()
            .unwrap()
            .execute("DELETE FROM wa WHERE k LIKE ?1", [&pattern])
            .ok();
    }

    /// Helper: scan keys by prefix.
    fn scan_keys(&self, prefix: &str) -> Vec<String> {
        let pattern = format!("{prefix}%");
        let conn = self.conn.lock().unwrap();
        let mut stmt = conn.prepare("SELECT k FROM wa WHERE k LIKE ?1").unwrap();
        let rows = stmt
            .query_map([&pattern], |r| r.get::<_, String>(0))
            .unwrap();
        rows.filter_map(|r| r.ok()).collect()
    }

    /// Helper: scan all (key, value) pairs for a prefix.
    fn scan_kv(&self, prefix: &str) -> Vec<(String, Vec<u8>)> {
        let pattern = format!("{prefix}%");
        let conn = self.conn.lock().unwrap();
        let mut stmt = conn.prepare("SELECT k, v FROM wa WHERE k LIKE ?1").unwrap();
        let rows = stmt
            .query_map([&pattern], |r| Ok((r.get::<_, String>(0)?, r.get::<_, Vec<u8>>(1)?)))
            .unwrap();
        rows.filter_map(|r| r.ok()).collect()
    }
}

impl Default for SqliteBackend {
    fn default() -> Self {
        // Tests use an in-memory database
        let conn = Connection::open_in_memory().unwrap();
        conn.execute_batch(
            "CREATE TABLE IF NOT EXISTS wa (k TEXT PRIMARY KEY, v BLOB);",
        )
        .unwrap();
        Self {
            conn: Mutex::new(conn),
            next_device_id: std::sync::atomic::AtomicI32::new(1),
        }
    }
}

// ---------------------------------------------------------------------------
// SignalStore
// ---------------------------------------------------------------------------

#[async_trait]
impl SignalStore for SqliteBackend {
    async fn put_identity(&self, address: &str, key: [u8; 32]) -> Result<()> {
        self.put("sig", &format!("identity:{address}"), &key);
        Ok(())
    }

    async fn load_identity(&self, address: &str) -> Result<Option<[u8; 32]>> {
        Ok(self
            .get("sig", &format!("identity:{address}"))
            .and_then(|v| v.try_into().ok()))
    }

    async fn delete_identity(&self, address: &str) -> Result<()> {
        self.del("sig", &format!("identity:{address}"));
        Ok(())
    }

    async fn get_session(&self, address: &str) -> Result<Option<Bytes>> {
        Ok(self
            .get("sig", &format!("session:{address}"))
            .map(Bytes::from))
    }

    async fn put_session(&self, address: &str, session: &[u8]) -> Result<()> {
        self.put("sig", &format!("session:{address}"), session);
        Ok(())
    }

    async fn delete_session(&self, address: &str) -> Result<()> {
        self.del("sig", &format!("session:{address}"));
        Ok(())
    }

    async fn has_session(&self, address: &str) -> Result<bool> {
        Ok(self.get("sig", &format!("session:{address}")).is_some())
    }

    async fn store_prekey(&self, id: u32, record: &[u8], _uploaded: bool) -> Result<()> {
        self.put("sig", &format!("prekey:{id}"), record);
        Ok(())
    }

    async fn load_prekey(&self, id: u32) -> Result<Option<Bytes>> {
        Ok(self
            .get("sig", &format!("prekey:{id}"))
            .map(Bytes::from))
    }

    async fn remove_prekey(&self, id: u32) -> Result<()> {
        self.del("sig", &format!("prekey:{id}"));
        Ok(())
    }

    async fn get_max_prekey_id(&self) -> Result<u32> {
        let conn = self.conn.lock().unwrap();
        let max: Option<u32> = conn
            .query_row(
                "SELECT CAST(SUBSTR(k, 11) AS INTEGER) FROM wa
                 WHERE k LIKE 'sig:prekey:%' ORDER BY k DESC LIMIT 1",
                [],
                |r| r.get(0),
            )
            .ok();
        Ok(max.unwrap_or(0))
    }

    async fn store_signed_prekey(&self, id: u32, record: &[u8]) -> Result<()> {
        self.put("sig", &format!("signed_prekey:{id}"), record);
        Ok(())
    }

    async fn load_signed_prekey(&self, id: u32) -> Result<Option<Vec<u8>>> {
        Ok(self.get("sig", &format!("signed_prekey:{id}")))
    }

    async fn load_all_signed_prekeys(&self) -> Result<Vec<(u32, Vec<u8>)>> {
        let kvs = self.scan_kv("sig:signed_prekey:");
        let mut out = Vec::with_capacity(kvs.len());
        for (k, v) in kvs {
            if let Some(id) = k.rsplit(':').next().and_then(|s| s.parse::<u32>().ok()) {
                out.push((id, v));
            }
        }
        Ok(out)
    }

    async fn remove_signed_prekey(&self, id: u32) -> Result<()> {
        self.del("sig", &format!("signed_prekey:{id}"));
        Ok(())
    }

    async fn put_sender_key(&self, address: &str, record: &[u8]) -> Result<()> {
        self.put("sig", &format!("sender_key:{address}"), record);
        Ok(())
    }

    async fn get_sender_key(&self, address: &str) -> Result<Option<Vec<u8>>> {
        Ok(self.get("sig", &format!("sender_key:{address}")))
    }

    async fn delete_sender_key(&self, address: &str) -> Result<()> {
        self.del("sig", &format!("sender_key:{address}"));
        Ok(())
    }
}

// ---------------------------------------------------------------------------
// AppSyncStore
// ---------------------------------------------------------------------------

#[async_trait]
impl AppSyncStore for SqliteBackend {
    async fn get_sync_key(&self, key_id: &[u8]) -> Result<Option<AppStateSyncKey>> {
        let hex = hex::encode(key_id);
        Ok(self.get_json("app", &format!("sync_key:{hex}")))
    }

    async fn set_sync_key(&self, key_id: &[u8], key: AppStateSyncKey) -> Result<()> {
        let hex = hex::encode(key_id);
        self.put_json("app", &format!("sync_key:{hex}"), &key);
        Ok(())
    }

    async fn get_version(&self, name: &str) -> Result<HashState> {
        Ok(self
            .get_json("app", &format!("version:{name}"))
            .unwrap_or_default())
    }

    async fn set_version(&self, name: &str, state: HashState) -> Result<()> {
        self.put_json("app", &format!("version:{name}"), &state);
        Ok(())
    }

    async fn put_mutation_macs(
        &self,
        name: &str,
        version: u64,
        mutations: &[AppStateMutationMAC],
    ) -> Result<()> {
        for (i, mac) in mutations.iter().enumerate() {
            let key = format!("mutmac:{name}:{version}:{i}:{}", hex::encode(&mac.index_mac));
            self.put("app", &key, &mac.value_mac);
        }
        Ok(())
    }

    async fn get_mutation_mac(&self, name: &str, index_mac: &[u8]) -> Result<Option<Vec<u8>>> {
        let prefix = format!("app:mutmac:{name}:");
        for (k, v) in self.scan_kv(&prefix) {
            if k.ends_with(&format!(":{}", hex::encode(index_mac))) {
                return Ok(Some(v));
            }
        }
        Ok(None)
    }

    async fn delete_mutation_macs(&self, name: &str, index_macs: &[Vec<u8>]) -> Result<()> {
        let hex_set: std::collections::HashSet<String> =
            index_macs.iter().map(hex::encode).collect();
        let prefix = format!("app:mutmac:{name}:");
        for (k, _) in self.scan_kv(&prefix) {
            if hex_set.iter().any(|h| k.ends_with(h)) {
                self.conn.lock().unwrap().execute("DELETE FROM wa WHERE k = ?1", [&k]).ok();
            }
        }
        Ok(())
    }

    async fn get_latest_sync_key_id(&self) -> Result<Option<Vec<u8>>> {
        Ok(self.get("app", "latest_sync_key_id"))
    }
}

// ---------------------------------------------------------------------------
// ProtocolStore
// ---------------------------------------------------------------------------

#[async_trait]
impl ProtocolStore for SqliteBackend {
    async fn get_sender_key_devices(&self, group_jid: &str) -> Result<Vec<(String, bool)>> {
        let prefix = format!("proto:skd:{group_jid}:");
        let kvs = self.scan_kv(&prefix);
        Ok(kvs
            .into_iter()
            .filter_map(|(k, v)| {
                k.rsplit(':').next().map(|jid| (jid.to_string(), v.first() == Some(&1)))
            })
            .collect())
    }

    async fn set_sender_key_status(
        &self,
        group_jid: &str,
        entries: &[(&str, bool)],
    ) -> Result<()> {
        for (device_jid, has_key) in entries {
            let v: &[u8] = if *has_key { &[1] } else { &[0] };
            self.put("proto", &format!("skd:{group_jid}:{device_jid}"), v);
        }
        Ok(())
    }

    async fn clear_sender_key_devices(&self, group_jid: &str) -> Result<()> {
        self.del_prefix(&format!("proto:skd:{group_jid}:"));
        Ok(())
    }

    async fn delete_sender_key_device_rows(&self, device_jids: &[&str]) -> Result<()> {
        let targets: std::collections::HashSet<&str> = device_jids.iter().copied().collect();
        for (k, _) in self.scan_kv("proto:skd:") {
            if targets.iter().any(|t| k.contains(&format!(":{t}"))) {
                self.conn.lock().unwrap().execute("DELETE FROM wa WHERE k = ?1", [&k]).ok();
            }
        }
        Ok(())
    }

    async fn clear_all_sender_key_devices(&self) -> Result<()> {
        self.del_prefix("proto:skd:");
        Ok(())
    }

    async fn get_lid_mapping(&self, lid: &str) -> Result<Option<LidPnMappingEntry>> {
        Ok(self.get_json("proto", &format!("lid:{lid}")))
    }

    async fn get_pn_mapping(&self, phone: &str) -> Result<Option<LidPnMappingEntry>> {
        Ok(self.get_json("proto", &format!("pn:{phone}")))
    }

    async fn put_lid_mapping(&self, entry: &LidPnMappingEntry) -> Result<()> {
        self.put_json("proto", &format!("lid:{}", entry.lid), entry);
        self.put_json("proto", &format!("pn:{}", entry.phone_number), entry);
        Ok(())
    }

    async fn get_all_lid_mappings(&self) -> Result<Vec<LidPnMappingEntry>> {
        let kvs = self.scan_kv("proto:lid:");
        Ok(kvs
            .into_iter()
            .filter_map(|(_, v)| serde_json::from_slice(&v).ok())
            .collect())
    }

    async fn save_base_key(&self, address: &str, message_id: &str, base_key: &[u8]) -> Result<()> {
        self.put("proto", &format!("basekey:{address}:{message_id}"), base_key);
        Ok(())
    }

    async fn has_same_base_key(
        &self,
        address: &str,
        message_id: &str,
        current_base_key: &[u8],
    ) -> Result<bool> {
        Ok(self
            .get("proto", &format!("basekey:{address}:{message_id}"))
            .as_deref()
            == Some(current_base_key))
    }

    async fn delete_base_key(&self, address: &str, message_id: &str) -> Result<()> {
        self.del("proto", &format!("basekey:{address}:{message_id}"));
        Ok(())
    }

    async fn update_device_list(&self, record: DeviceListRecord) -> Result<()> {
        self.put_json("proto", &format!("devlist:{}", record.user), &record);
        Ok(())
    }

    async fn get_devices(&self, user: &str) -> Result<Option<DeviceListRecord>> {
        Ok(self.get_json("proto", &format!("devlist:{user}")))
    }

    async fn delete_devices(&self, user: &str) -> Result<()> {
        self.del("proto", &format!("devlist:{user}"));
        Ok(())
    }

    async fn get_tc_token(&self, jid: &str) -> Result<Option<TcTokenEntry>> {
        Ok(self.get_json("proto", &format!("tctoken:{jid}")))
    }

    async fn put_tc_token(&self, jid: &str, entry: &TcTokenEntry) -> Result<()> {
        self.put_json("proto", &format!("tctoken:{jid}"), &entry);
        Ok(())
    }

    async fn delete_tc_token(&self, jid: &str) -> Result<()> {
        self.del("proto", &format!("tctoken:{jid}"));
        Ok(())
    }

    async fn get_all_tc_token_jids(&self) -> Result<Vec<String>> {
        let keys = self.scan_keys("proto:tctoken:");
        Ok(keys
            .into_iter()
            .filter_map(|k| k.split("tctoken:").nth(1).map(|s| s.to_string()))
            .collect())
    }

    async fn delete_expired_tc_tokens(&self, cutoff_timestamp: i64) -> Result<u32> {
        let mut count = 0u32;
        for (k, v) in self.scan_kv("proto:tctoken:") {
            if let Ok(token) = serde_json::from_slice::<TcTokenEntry>(&v) {
                if token.token_timestamp < cutoff_timestamp {
                    self.conn.lock().unwrap().execute("DELETE FROM wa WHERE k = ?1", [&k]).ok();
                    count += 1;
                }
            }
        }
        Ok(count)
    }

    async fn store_sent_message(
        &self,
        chat_jid: &str,
        message_id: &str,
        payload: &[u8],
    ) -> Result<()> {
        self.put("proto", &format!("sentmsg:{chat_jid}:{message_id}"), payload);
        Ok(())
    }

    async fn take_sent_message(
        &self,
        chat_jid: &str,
        message_id: &str,
    ) -> Result<Option<Vec<u8>>> {
        let val = self.get("proto", &format!("sentmsg:{chat_jid}:{message_id}"));
        if val.is_some() {
            self.del("proto", &format!("sentmsg:{chat_jid}:{message_id}"));
        }
        Ok(val)
    }

    async fn delete_expired_sent_messages(&self, _cutoff_timestamp: i64) -> Result<u32> {
        // sent messages don't have timestamps in this store — keep them forever
        // (expiry is not critical for correctness)
        Ok(0)
    }
}

// ---------------------------------------------------------------------------
// DeviceStore
// ---------------------------------------------------------------------------

#[async_trait]
impl DeviceStore for SqliteBackend {
    async fn save(&self, device: &Device) -> Result<()> {
        self.put_json("dev", "device", device);
        Ok(())
    }

    async fn load(&self) -> Result<Option<Device>> {
        Ok(self.get_json("dev", "device"))
    }

    async fn exists(&self) -> Result<bool> {
        Ok(self.get("dev", "device").is_some())
    }

    async fn create(&self) -> Result<i32> {
        let id = self.next_device_id.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        // Persist counter
        self.put(
            "meta",
            "next_device_id",
            &id.to_be_bytes(),
        );
        if !self.exists().await.unwrap_or(false) {
            self.put_json("dev", "device", &Device::new());
        }
        Ok(id)
    }

    async fn snapshot_db(&self, _name: &str, _extra_content: Option<&[u8]>) -> Result<()> {
        // WAL mode + synchronous writes give us crash safety
        Ok(())
    }
}
