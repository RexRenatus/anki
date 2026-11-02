// Copyright: Ankitects Pty Ltd and contributors
// License: GNU AGPL, version 3 or later; http://www.gnu.org/licenses/agpl.html

mod adding;
mod ankidroid;
mod ankihub;
mod ankiweb;
mod card_rendering;
mod collection;
mod config;
pub(crate) mod dbproxy;
mod error;
mod github;
mod i18n;
mod import_export;
mod media;
mod ops;
mod sync;

use std::ops::Deref;
use std::result;
use std::sync::Arc;
use std::sync::Mutex;
use std::sync::OnceLock;
use std::thread::JoinHandle;

use futures::future::AbortHandle;
use prost::Message;
use reqwest::Client;
use tokio::runtime;
use tokio::runtime::Runtime;

use crate::backend::dbproxy::db_command_bytes;
use crate::backend::sync::SyncState;
use crate::prelude::*;
use crate::progress::Progress;
use crate::progress::ProgressState;
use crate::progress::ThrottlingProgressHandler;

#[derive(Clone)]
#[repr(transparent)]
pub struct Backend(Arc<BackendInner>);

impl Deref for Backend {
    type Target = BackendInner;

    fn deref(&self) -> &Self::Target {
        &self.0
    }
}

pub struct BackendInner {
    col: Mutex<Option<Collection>>,
    pub(crate) tr: I18n,
    server: bool,
    sync_abort: Mutex<Option<AbortHandle>>,
    progress_state: Arc<Mutex<ProgressState>>,
    runtime: OnceLock<Runtime>,
    state: Mutex<BackendState>,
    backup_task: Mutex<Option<JoinHandle<Result<()>>>>,
    media_sync_task: Mutex<Option<JoinHandle<Result<()>>>>,
    web_client: Mutex<Option<Client>>,
}

#[derive(Default)]
struct BackendState {
    sync: SyncState,
}

pub fn init_backend(init_msg: &[u8]) -> result::Result<Backend, String> {
    let input: anki_proto::backend::BackendInit =
        match anki_proto::backend::BackendInit::decode(init_msg) {
            Ok(req) => req,
            Err(_) => return Err("couldn't decode init request".into()),
        };

    let tr = I18n::new(&input.preferred_langs);

    Ok(Backend::new(tr, input.server))
}

impl Backend {
    pub fn new(tr: I18n, server: bool) -> Backend {
        Backend(Arc::new(BackendInner {
            col: Mutex::new(None),
            tr,
            server,
            sync_abort: Mutex::new(None),
            progress_state: Arc::new(Mutex::new(ProgressState {
                want_abort: false,
                last_progress: None,
            })),
            runtime: OnceLock::new(),
            state: Mutex::new(BackendState::default()),
            backup_task: Mutex::new(None),
            media_sync_task: Mutex::new(None),
            web_client: Mutex::new(None),
        }))
    }

    pub fn i18n(&self) -> &I18n {
        &self.tr
    }

    pub fn run_db_command_bytes(&self, input: &[u8]) -> result::Result<Vec<u8>, Vec<u8>> {
        self.db_command(input).map_err(|err| {
            let backend_err = err.into_protobuf(&self.tr);
            let mut bytes = Vec::new();
            backend_err.encode(&mut bytes).unwrap();
            bytes
        })
    }

    /// If collection is open, run the provided closure while holding
    /// the mutex.
    /// If collection is not open, return an error.
    pub(crate) fn with_col<F, T>(&self, func: F) -> Result<T>
    where
        F: FnOnce(&mut Collection) -> Result<T>,
    {
        func(
            self.col
                .lock()
                .unwrap()
                .as_mut()
                .ok_or(AnkiError::CollectionNotOpen)?,
        )
    }

    fn runtime_handle(&self) -> runtime::Handle {
        self.runtime
            .get_or_init(|| {
                #[cfg(not(target_arch = "wasm32"))]
                let mut builder = runtime::Builder::new_multi_thread();
                #[cfg(not(target_arch = "wasm32"))]
                builder.worker_threads(1);
                // wasm32 patch current-thread-runtime: tokio has no multi-thread runtime here.
                #[cfg(target_arch = "wasm32")]
                let mut builder = runtime::Builder::new_current_thread();
                #[cfg(not(target_arch = "wasm32"))]
                let runtime = builder.enable_all().build();
                // wasm32 patch wasm-clock-threads: no driver is enabled, because tokio has no time
                // driver on wasm32-unknown-unknown, where a timer panics.
                #[cfg(target_arch = "wasm32")]
                let runtime = builder.build();
                runtime.unwrap()
            })
            .handle()
            .clone()
    }

    // wasm32 patch browser-tls: the browser's fetch owns TLS and its trust store; reqwest's
    // wasm32 backend has no rustls and no root certificates to add.
    #[cfg(all(feature = "rustls", not(target_arch = "wasm32")))]
    fn set_custom_certificate_inner(&self, cert_str: String) -> Result<()> {
        use reqwest::Certificate;

        let mut web_client = self.web_client.lock().unwrap();

        if cert_str.is_empty() {
            let _ = web_client.insert(Client::builder().http1_only().build().unwrap());
            return Ok(());
        }

        if pem_item_count(cert_str.as_bytes()) != 1 {
            return Err(AnkiError::InvalidCertificateFormat);
        }

        if let Ok(certificate) = Certificate::from_pem(cert_str.as_bytes()) {
            if let Ok(new_client) = Client::builder()
                .use_rustls_tls()
                .add_root_certificate(certificate)
                .http1_only()
                .build()
            {
                let _ = web_client.insert(new_client);
                return Ok(());
            }
        }

        Err(AnkiError::InvalidCertificateFormat)
    }

    fn web_client(&self) -> Client {
        // currently limited to http1, as nginx doesn't support http2 proxies
        let mut web_client = self.web_client.lock().unwrap();

        web_client
            .get_or_insert_with(|| {
                let builder = Client::builder();
                #[cfg(not(target_arch = "wasm32"))]
                let builder = builder.http1_only();
                // wasm32 patch browser-fetch: the browser's fetch chooses the protocol; no http1_only here.
                builder.build().unwrap()
            })
            .clone()
    }

    fn db_command(&self, input: &[u8]) -> Result<Vec<u8>> {
        self.with_col(|col| db_command_bytes(col, input))
    }

    /// Useful for operations that function with a closed collection, such as
    /// a colpkg import. For collection operations, you can use
    /// [Collection::new_progress_handler] instead.
    pub(crate) fn new_progress_handler<P: Into<Progress> + Default + Clone>(
        &self,
    ) -> ThrottlingProgressHandler<P> {
        ThrottlingProgressHandler::new(self.progress_state.clone())
    }
}

/// The number of items rustls-pemfile 2.2.0's `read_all` yields for `pem`,
/// read with rustls-pki-types' PEM reader. Like `read_all`, it counts each
/// section of a kind `read_all` returns and each error (which `read_all`
/// yields as one item before reading on), and skips other sections, such as
/// ECHCONFIG or an unknown label.
#[cfg(all(feature = "rustls", not(target_arch = "wasm32")))]
fn pem_item_count(pem: &[u8]) -> usize {
    use rustls_pki_types::pem::SectionKind;

    let mut rd = std::io::Cursor::new(pem);
    let mut n = 0;
    loop {
        match rustls_pki_types::pem::from_buf(&mut rd) {
            Ok(None) => return n,
            Ok(Some((kind, _))) => match kind {
                SectionKind::Certificate
                | SectionKind::PublicKey
                | SectionKind::RsaPrivateKey
                | SectionKind::PrivateKey
                | SectionKind::EcPrivateKey
                | SectionKind::Crl
                | SectionKind::Csr => n += 1,
                _ => {}
            },
            Err(_) => n += 1,
        }
    }
}

#[cfg(all(test, feature = "rustls", not(target_arch = "wasm32")))]
mod test {
    use std::sync::mpsc;
    use std::thread;
    use std::time::Duration;

    use super::pem_item_count;

    const BODY: &str = "AAECAwQF";

    fn section(label: &str, body: &str) -> String {
        format!("-----BEGIN {label}-----\n{body}\n-----END {label}-----\n")
    }

    /// The fixture rows: a label, the PEM text, and the count rustls-pemfile
    /// 2.2.0's `read_all` gave for it while it was still a dependency.
    fn rows() -> Vec<(&'static str, String, usize)> {
        let cert = section("CERTIFICATE", BODY);
        vec![
            ("empty", String::new(), 0),
            ("one-certificate", cert.clone(), 1),
            ("two-certificates", format!("{cert}{cert}"), 2),
            (
                "certificate-and-pkcs8-key",
                format!("{cert}{}", section("PRIVATE KEY", BODY)),
                2,
            ),
            ("unknown-label", section("SOMETHING ELSE", BODY), 0),
            ("echconfig", section("ECHCONFIG", BODY), 0),
            ("malformed-base64", section("CERTIFICATE", "%%%%"), 1),
            (
                "begin-without-end",
                format!("-----BEGIN CERTIFICATE-----\n{BODY}\n"),
                1,
            ),
        ]
    }

    /// Runs `count` on its own thread: None when it has not returned within
    /// 2 seconds, so a hang reads red rather than stuck.
    fn within_deadline(count: impl FnOnce() -> usize + Send + 'static) -> Option<usize> {
        let (tx, rx) = mpsc::channel();
        thread::spawn(move || {
            let _ = tx.send(count());
        });
        rx.recv_timeout(Duration::from_secs(2)).ok()
    }

    #[test]
    fn pem_item_count_equals_rustls_pemfile() {
        let rows = rows();
        let mut differ = 0;
        for (label, pem, golden) in rows.iter().cloned() {
            let ours = within_deadline(move || pem_item_count(pem.as_bytes()));
            let same = ours == Some(golden);
            println!("pem row={label} ours={ours:?} rustls_pemfile={golden} same={same}");
            differ += usize::from(!same);
        }
        println!("pem_item_count examined {} differ={differ}", rows.len());
        assert_eq!(rows.len(), 8);
        assert_eq!(differ, 0, "pem_item_count differs from rustls-pemfile");
    }
}
