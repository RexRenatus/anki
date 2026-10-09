// Copyright: Ankitects Pty Ltd and contributors
// License: GNU AGPL, version 3 or later; http://www.gnu.org/licenses/agpl.html

// wasm32 patch browser-full-sync-files: a download is checked and written through SQLite on wasm32.
#[cfg(not(target_arch = "wasm32"))]
use anki_io::atomic_rename;
#[cfg(not(target_arch = "wasm32"))]
use anki_io::new_tempfile_in_parent_of;
use anki_io::read_file;
#[cfg(not(target_arch = "wasm32"))]
use anki_io::write_file;
use reqwest::Client;

#[cfg(not(target_arch = "wasm32"))]
use crate::collection::CollectionBuilder;
use crate::prelude::*;
use crate::storage::SchemaVersion;
use crate::sync::collection::protocol::EmptyInput;
use crate::sync::error::HttpResult;
use crate::sync::error::OrHttpErr;
use crate::sync::http_client::HttpSyncClient;
use crate::sync::login::SyncAuth;

impl Collection {
    /// Download collection from AnkiWeb. Caller must re-open afterwards.
    pub async fn full_download(self, auth: SyncAuth, client: Client) -> Result<()> {
        self.full_download_with_server(HttpSyncClient::new(auth, client))
            .await
    }

    // pub for tests
    pub(super) async fn full_download_with_server(self, server: HttpSyncClient) -> Result<()> {
        let col_path = self.col_path.clone();
        let _col_folder = col_path.parent().or_invalid("couldn't get col_folder")?;
        let progress = self.new_progress_handler();
        self.close(None)?;
        let out_data = server
            .download_with_progress(EmptyInput::request(), progress)
            .await?
            .data;
        // check file ok
        #[cfg(not(target_arch = "wasm32"))]
        let temp_file = new_tempfile_in_parent_of(&col_path)?;
        #[cfg(not(target_arch = "wasm32"))]
        write_file(temp_file.path(), out_data)?;
        #[cfg(not(target_arch = "wasm32"))]
        let col = CollectionBuilder::new(temp_file.path())
            .set_check_integrity(true)
            .build()?;
        #[cfg(not(target_arch = "wasm32"))]
        col.storage.db.execute_batch("update col set ls=mod")?;
        #[cfg(not(target_arch = "wasm32"))]
        col.close(None)?;
        #[cfg(not(target_arch = "wasm32"))]
        atomic_rename(temp_file, &col_path, true)?;
        // wasm32 patch browser-full-sync-files: the browser gives the engine no file system.
        #[cfg(target_arch = "wasm32")]
        replace_from_memory(&col_path, out_data)?;
        Ok(())
    }
}

/// wasm32 patch browser-full-sync-files: the browser gives the engine no file system, so the
/// received collection is deserialized into memory, where its integrity is checked and `ls` is set
/// to `mod`, as the native path does in its temporary file. An image in WAL mode is read in memory
/// as a rollback-journal one (header bytes 18 and 19 from 2 to 1), because a memory database has no
/// WAL, and the memory connection compares `unicase` as the collection's own connection does, so
/// the check can read the indexes that collate by it. SQLite's backup interface then replaces the
/// collection at `col_path` in one step, which is one transaction, through the file system the web
/// engine installs as SQLite's default. The lock is exclusive, as the collection's own is.
#[cfg(target_arch = "wasm32")]
fn replace_from_memory(col_path: &std::path::Path, mut data: Vec<u8>) -> Result<()> {
    use rusqlite::backup::Backup;
    use rusqlite::backup::StepResult;
    use unicase::UniCase;

    if data.len() > 19 && data[18] == 2 && data[19] == 2 {
        data[18] = 1;
        data[19] = 1;
    }
    let mut received = rusqlite::Connection::open_in_memory()?;
    received.create_collation("unicase", |s1: &str, s2: &str| {
        UniCase::new(s1).cmp(&UniCase::new(s2))
    })?;
    let size = data.len();
    received.deserialize_read_exact(rusqlite::MAIN_DB, data.as_slice(), size, false)?;
    let check: String = received.pragma_query_value(None, "integrity_check", |row| row.get(0))?;
    require!(check == "ok", "corrupt: {check}");
    received.execute_batch("update col set ls=mod")?;
    let mut collection = rusqlite::Connection::open(col_path)?;
    collection.pragma_update(None, "locking_mode", "exclusive")?;
    let backup = Backup::new(&received, &mut collection)?;
    let step = backup.step(-1)?;
    require!(
        step == StepResult::Done,
        "the received collection was not written: {step:?}"
    );
    Ok(())
}

pub fn server_download(
    col: &mut Option<Collection>,
    schema_version: SchemaVersion,
) -> HttpResult<Vec<u8>> {
    let col_path = {
        let mut col = col.take().or_internal_err("take col")?;
        let path = col.col_path.clone();
        col.transact_no_undo(|col| col.storage.increment_usn())
            .or_internal_err("incr usn")?;
        col.close(Some(schema_version)).or_internal_err("close")?;
        path
    };
    let data = read_file(col_path).or_internal_err("read col")?;
    Ok(data)
}
