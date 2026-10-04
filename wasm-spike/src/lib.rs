// Copyright: Ankitects Pty Ltd and contributors
// License: GNU AGPL, version 3 or later; http://www.gnu.org/licenses/agpl.html

//! Measuring harness for the engine on wasm32-unknown-unknown.
//!
//! The engine runs in a dedicated Worker. Its collection lives in the origin private file system
//! (OPFS) through the SyncAccessHandle pool VFS, which needs a Worker. The data is synthetic.
//!
//! `run_method` exports the backend's whole protocol, so the size of the built module covers the
//! entire engine rather than the few calls the harness makes. The harness calls go through the
//! same entry point, with protobuf requests built here.

#![cfg(target_arch = "wasm32")]

use std::cell::RefCell;

use anki::backend::init_backend;
use anki::backend::Backend;
use anki_proto::backend::BackendError;
use anki_proto::backend::BackendInit;
use anki_proto::collection::CloseCollectionRequest;
use anki_proto::collection::OpenCollectionRequest;
use anki_proto::notes::AddNoteRequest;
use anki_proto::notes::AddNotesRequest;
use anki_proto::notes::AddNotesResponse;
use anki_proto::notes::Note;
use anki_proto::notetypes::NotetypeId;
use anki_proto::notetypes::NotetypeNames;
use anki_proto::scheduler::card_answer::Rating;
use anki_proto::scheduler::CardAnswer;
use anki_proto::scheduler::GetQueuedCardsRequest;
use anki_proto::scheduler::QueuedCards;
use prost::Message;
use sqlite_wasm_vfs::sahpool::install;
use sqlite_wasm_vfs::sahpool::OpfsSAHPoolCfgBuilder;
use sqlite_wasm_vfs::sahpool::OpfsSAHPoolUtil;
use wasm_bindgen::prelude::*;

// Service and method indices of `Backend::run_service_method` at this revision, read from the
// generated dispatcher (the odd service numbers are the backend's services).
const COLLECTION: u32 = 3;
const OPEN_COLLECTION: u32 = 0;
const CLOSE_COLLECTION: u32 = 1;
const UNDO: u32 = 8;
const SCHEDULER: u32 = 13;
const GET_QUEUED_CARDS: u32 = 3;
const ANSWER_CARD: u32 = 4;
const NOTETYPES: u32 = 23;
const GET_NOTETYPE_NAMES: u32 = 8;
const NOTES: u32 = 25;
const NEW_NOTE: u32 = 0;
const ADD_NOTES: u32 = 2;

/// The default deck of a new collection.
const DEFAULT_DECK: i64 = 1;

thread_local! {
    static BACKEND: RefCell<Option<Backend>> = const { RefCell::new(None) };
    static POOL: RefCell<Option<OpfsSAHPoolUtil>> = const { RefCell::new(None) };
    static LAST_PANIC: RefCell<Option<String>> = const { RefCell::new(None) };
}

fn backend() -> Result<Backend, JsValue> {
    BACKEND
        .with(|b| b.borrow().clone())
        .ok_or_else(|| JsValue::from_str("backend not initialised"))
}

fn call(service: u32, method: u32, input: &[u8]) -> Result<Vec<u8>, JsValue> {
    backend()?
        .run_service_method(service, method, input)
        .map_err(|bytes| match BackendError::decode(bytes.as_slice()) {
            Ok(err) => JsValue::from_str(&format!("backend error {}: {}", err.kind, err.message)),
            Err(_) => JsValue::from_str("backend error (undecodable)"),
        })
}

fn decode<M: Message + Default>(bytes: Vec<u8>) -> Result<M, JsValue> {
    M::decode(bytes.as_slice()).map_err(|e| JsValue::from_str(&e.to_string()))
}

/// Registers the OPFS SyncAccessHandle pool as SQLite's default VFS, so a plain path opens a
/// file in OPFS. `clear` empties the pool first. Returns the pool's file count.
#[wasm_bindgen]
pub async fn install_opfs(directory: String, clear: bool) -> Result<u32, JsValue> {
    let cfg = OpfsSAHPoolCfgBuilder::new()
        .directory(&directory)
        .clear_on_init(clear)
        .build();
    let pool = install::<sqlite_wasm_rs::WasmOsCallback>(&cfg, true)
        .await
        .map_err(|e| JsValue::from_str(&format!("opfs pool: {e}")))?;
    let count = pool.count();
    POOL.with(|p| *p.borrow_mut() = Some(pool));
    Ok(count)
}

/// Names of the files the OPFS pool holds.
#[wasm_bindgen]
pub fn opfs_files() -> Vec<String> {
    POOL.with(|p| {
        p.borrow()
            .as_ref()
            .map(|pool| pool.list())
            .unwrap_or_default()
    })
}

/// The message of the last panic. A panic on wasm32 aborts as a bare `unreachable` trap, so the
/// hook keeps its message for the harness to report.
#[wasm_bindgen]
pub fn last_panic() -> Option<String> {
    LAST_PANIC.with(|p| p.borrow().clone())
}

/// Creates the backend, as `init_backend` does for the desktop's bridge.
#[wasm_bindgen]
pub fn init() -> Result<(), JsValue> {
    std::panic::set_hook(Box::new(|info| {
        let message = info.to_string();
        LAST_PANIC.with(|p| {
            if let Ok(mut slot) = p.try_borrow_mut() {
                *slot = Some(message);
            }
        });
    }));
    let msg = BackendInit {
        preferred_langs: vec!["en".into()],
        locale_folder_path: String::new(),
        server: false,
    };
    let backend = init_backend(&msg.encode_to_vec()).map_err(|e| JsValue::from_str(&e))?;
    BACKEND.with(|b| *b.borrow_mut() = Some(backend));
    Ok(())
}

/// The backend's protocol: a service and method index, a protobuf request, a protobuf reply.
#[wasm_bindgen]
pub fn run_method(service: u32, method: u32, input: &[u8]) -> Result<Vec<u8>, JsValue> {
    call(service, method, input)
}

/// Runs a read-only query through the backend's database proxy and returns its JSON rows.
#[wasm_bindgen]
pub fn db_query(sql: &str) -> Result<String, JsValue> {
    let request = serde_json::json!({
        "kind": "query",
        "sql": sql,
        "args": [],
        "first_row_only": false,
    });
    let reply = backend()?
        .run_db_command_bytes(request.to_string().as_bytes())
        .map_err(|_| JsValue::from_str("db query failed"))?;
    String::from_utf8(reply).map_err(|e| JsValue::from_str(&e.to_string()))
}

#[wasm_bindgen]
pub fn open_collection(path: &str) -> Result<(), JsValue> {
    let request = OpenCollectionRequest {
        collection_path: path.into(),
        media_folder_path: String::new(),
        media_db_path: String::new(),
    };
    call(COLLECTION, OPEN_COLLECTION, &request.encode_to_vec()).map(|_| ())
}

#[wasm_bindgen]
pub fn close_collection() -> Result<(), JsValue> {
    let request = CloseCollectionRequest {
        downgrade_to_schema11: false,
    };
    call(COLLECTION, CLOSE_COLLECTION, &request.encode_to_vec()).map(|_| ())
}

/// Adds `count` synthetic Basic notes to the default deck in one operation. Returns the number
/// of notes added.
#[wasm_bindgen]
pub fn seed_synthetic_notes(count: u32) -> Result<u32, JsValue> {
    let names: NotetypeNames = decode(call(NOTETYPES, GET_NOTETYPE_NAMES, &[])?)?;
    let basic = names
        .entries
        .iter()
        .find(|entry| entry.name == "Basic")
        .ok_or_else(|| JsValue::from_str("no Basic notetype"))?
        .id;
    let template: Note = decode(call(
        NOTES,
        NEW_NOTE,
        &NotetypeId { ntid: basic }.encode_to_vec(),
    )?)?;
    let requests = (0..count)
        .map(|i| {
            let mut note = template.clone();
            note.fields = vec![
                format!("synthetic front {i}"),
                format!("synthetic back {i}"),
            ];
            AddNoteRequest {
                note: Some(note),
                deck_id: DEFAULT_DECK,
            }
        })
        .collect();
    let reply: AddNotesResponse = decode(call(
        NOTES,
        ADD_NOTES,
        &AddNotesRequest { requests }.encode_to_vec(),
    )?)?;
    Ok(reply.nids.len() as u32)
}

/// Answers the first queued card Good, as a reviewer does, and returns its card id.
#[wasm_bindgen]
pub fn answer_first_card(milliseconds_taken: u32) -> Result<f64, JsValue> {
    let request = GetQueuedCardsRequest {
        fetch_limit: 1,
        intraday_learning_only: false,
    };
    let queued: QueuedCards = decode(call(SCHEDULER, GET_QUEUED_CARDS, &request.encode_to_vec())?)?;
    let first = queued
        .cards
        .into_iter()
        .next()
        .ok_or_else(|| JsValue::from_str("no card queued"))?;
    let card = first
        .card
        .ok_or_else(|| JsValue::from_str("queued card without card"))?;
    let states = first
        .states
        .ok_or_else(|| JsValue::from_str("queued card without states"))?;
    let answer = CardAnswer {
        card_id: card.id,
        current_state: states.current,
        new_state: states.good,
        rating: Rating::Good as i32,
        answered_at_millis: js_sys::Date::now() as i64,
        milliseconds_taken,
    };
    call(SCHEDULER, ANSWER_CARD, &answer.encode_to_vec())?;
    Ok(card.id as f64)
}

/// Undoes the last operation, as the reviewer's undo does.
#[wasm_bindgen]
pub fn undo() -> Result<(), JsValue> {
    call(COLLECTION, UNDO, &[]).map(|_| ())
}
