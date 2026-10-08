// Copyright: Ankitects Pty Ltd and contributors
// License: GNU AGPL, version 3 or later; http://www.gnu.org/licenses/agpl.html

pub(crate) mod full_sync;
pub(crate) mod io_monitor;
mod protocol;

use std::time::Duration;

use reqwest::Client;
use reqwest::Error;
use reqwest::StatusCode;
use reqwest::Url;

use crate::notes;
use crate::sync::collection::protocol::AsSyncEndpoint;
use crate::sync::error::HttpError;
use crate::sync::error::HttpResult;
use crate::sync::http_client::io_monitor::IoMonitor;
use crate::sync::login::SyncAuth;
use crate::sync::request::header_and_stream::SyncHeader;
use crate::sync::request::header_and_stream::SYNC_HEADER_NAME;
use crate::sync::request::SyncRequest;
use crate::sync::response::SyncResponse;

#[derive(Clone)]
pub struct HttpSyncClient {
    /// Set to the empty string for initial login
    pub sync_key: String,
    session_key: String,
    client: Client,
    pub endpoint: Url,
    pub io_timeout: Duration,
}

impl HttpSyncClient {
    pub fn new(auth: SyncAuth, client: Client) -> HttpSyncClient {
        let io_timeout = Duration::from_secs(auth.io_timeout_secs.unwrap_or(30) as u64);
        HttpSyncClient {
            sync_key: auth.hkey,
            session_key: simple_session_id(),
            client,
            endpoint: auth
                .endpoint
                .unwrap_or_else(|| Url::try_from("https://sync.ankiweb.net/").unwrap()),
            io_timeout,
        }
    }

    async fn request<I, O>(
        &self,
        method: impl AsSyncEndpoint,
        request: SyncRequest<I>,
    ) -> HttpResult<SyncResponse<O>> {
        self.request_ext(method, request, IoMonitor::new()).await
    }

    async fn request_ext<I, O>(
        &self,
        method: impl AsSyncEndpoint,
        request: SyncRequest<I>,
        io_monitor: IoMonitor,
    ) -> HttpResult<SyncResponse<O>> {
        let header = SyncHeader {
            sync_version: request.sync_version,
            sync_key: self.sync_key.clone(),
            client_ver: request.client_version,
            session_key: self.session_key.clone(),
        };
        let data = request.data;
        let url = method.as_sync_endpoint(&self.endpoint);
        let request = self
            .client
            .post(url)
            .header(&SYNC_HEADER_NAME, serde_json::to_string(&header).unwrap());
        io_monitor
            .zstd_request_with_timeout(request, data, self.io_timeout)
            .await
            .map(SyncResponse::from_vec)
    }

    #[cfg(test)]
    pub(crate) fn endpoint(&self) -> &Url {
        &self.endpoint
    }

    #[cfg(test)]
    pub(crate) fn set_skey(&mut self, skey: String) {
        self.session_key = skey;
    }

    #[cfg(test)]
    pub(crate) fn skey(&self) -> &str {
        &self.session_key
    }
}

impl From<Error> for HttpError {
    fn from(err: Error) -> Self {
        HttpError {
            // we should perhaps make this Optional instead
            code: err.status().unwrap_or(StatusCode::SEE_OTHER),
            context: "from reqwest".into(),
            source: Some(Box::new(err) as _),
        }
    }
}

/// wasm32 patch browser-xhr: reqwest's wasm32 backend is the browser's asynchronous fetch, whose
/// answer never arrives while the engine blocks the Worker, so the request is a synchronous
/// XMLHttpRequest from the dedicated Worker. Its URL, method and headers are read from the built
/// request, the body is zstd-encoded in memory, and the answer is read as bytes and decoded by its
/// size header. An answer from another URL is refused, a server's status reaches the status
/// mapping through `BrowserStatus`, and the stall duration is the request's timeout.
#[cfg(target_arch = "wasm32")]
fn xhr_request(
    request: reqwest::RequestBuilder,
    body: Vec<u8>,
    stall: Duration,
) -> HttpResult<Vec<u8>> {
    use js_sys::JsString;
    use js_sys::Reflect;
    use js_sys::Uint8Array;
    use reqwest::header::CONTENT_TYPE;
    use web_sys::XmlHttpRequest;
    use web_sys::XmlHttpRequestResponseType;

    use crate::error::network::BrowserStatus;
    use crate::sync::error::OrHttpErr;
    use crate::sync::response::ORIGINAL_SIZE;

    let browser = |context: &str| HttpError::new_without_source(StatusCode::SEE_OTHER, context);
    let request = request
        .header(CONTENT_TYPE, "application/octet-stream")
        .build()?;
    let encoded = zstd::encode_all(body.as_slice(), 0)
        .or_http_err(StatusCode::SEE_OTHER, "encoding the request")?;
    let xhr = XmlHttpRequest::new().map_err(|_| browser("creating the request"))?;
    xhr.open_with_async(request.method().as_str(), request.url().as_str(), false)
        .map_err(|_| browser("opening the request"))?;
    xhr.set_response_type(XmlHttpRequestResponseType::Arraybuffer);
    xhr.set_timeout(u32::try_from(stall.as_millis()).unwrap_or(u32::MAX));
    for (name, value) in request.headers() {
        let value = value
            .to_str()
            .or_bad_request("header was not visible ascii")?;
        xhr.set_request_header(name.as_str(), value)
            .map_err(|_| browser("setting a header"))?;
    }
    if let Err(error) = xhr.send_with_opt_u8_array(Some(&encoded)) {
        let name = Reflect::get(&error, &JsString::from("name"))
            .ok()
            .and_then(|name| name.as_string());
        let code = if name.as_deref() == Some("TimeoutError") {
            StatusCode::REQUEST_TIMEOUT
        } else {
            StatusCode::SEE_OTHER
        };
        return Err(HttpError::new_without_source(code, "sending the request"));
    }
    if xhr.response_url() != request.url().as_str() {
        return Err(HttpError::new_without_source(
            StatusCode::MISDIRECTED_REQUEST,
            "answered from another url",
        ));
    }
    let code = xhr.status().map_err(|_| browser("reading the status"))?;
    let code =
        StatusCode::from_u16(code).or_http_err(StatusCode::SEE_OTHER, "reading the status")?;
    if !code.is_success() {
        return Err(HttpError {
            code,
            context: "answered with a status".into(),
            source: Some(Box::new(BrowserStatus(code)) as _),
        });
    }
    let size = xhr
        .get_response_header(ORIGINAL_SIZE.as_str())
        .ok()
        .flatten()
        .and_then(|size| size.parse::<usize>().ok())
        .or_bad_request("missing original size")?;
    let answer = xhr.response().map_err(|_| browser("reading the answer"))?;
    zstd::bulk::decompress(&Uint8Array::new(&answer).to_vec(), size)
        .or_http_err(StatusCode::SEE_OTHER, "decoding the answer")
}

fn simple_session_id() -> String {
    let table = b"abcdefghijklmnopqrstuvwxyzABCDEFGHIJKLMNOPQRSTUVWXYZ\
0123456789";
    notes::to_base_n(rand::random::<u32>() as u64, table)
}
