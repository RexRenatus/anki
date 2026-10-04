// The measured scenario: a synthetic collection in OPFS, opened, one card answered, the answer
// undone. Every note is generated here; no real collection data is read or written.
import init, * as engine from "./pkg/anki_wasm_spike.js";

const COLLECTION = "/spike/collection.anki2";
const SCHEDULER = 13;
const GET_QUEUED_CARDS = 3;

function timed(fn) {
  const start = performance.now();
  const value = fn();
  return [value, performance.now() - start];
}

const query = (sql) => JSON.parse(engine.db_query(sql));
const cardsTable = () => JSON.stringify(query("select * from cards order by id"));
const revlogRows = (cid) => query(`select count() from revlog where cid = ${cid}`)[0][0];

self.onmessage = async ({ data }) => {
  const report = { ok: false, errors: [], steps: {} };
  let stage = "load";
  try {
    let start = performance.now();
    await init();
    report.steps.load_ms = performance.now() - start;

    stage = "opfs";
    start = performance.now();
    await engine.install_opfs("anki-wasm-spike", true);
    report.steps.opfs_ms = performance.now() - start;

    stage = "init";
    engine.init();

    // Seed a fresh collection, then close it, so the measured open reads it back from OPFS.
    stage = "seed";
    engine.open_collection(COLLECTION);
    const [added, seedMs] = timed(() => engine.seed_synthetic_notes(data.notes));
    report.steps.seed_ms = seedMs;
    report.notes_added = added;
    engine.close_collection();

    stage = "open";
    const [, openMs] = timed(() => engine.open_collection(COLLECTION));
    report.open_ms = openMs;
    report.notes = query("select count() from notes")[0][0];
    report.cards = query("select count() from cards")[0][0];

    report.journal_mode = query("pragma journal_mode")[0][0];

    // The reviewer's two calls, timed apart: building the queue (through the raw protocol export,
    // GetQueuedCardsRequest { fetch_limit: 1 } encoded by hand), then answering, whose own queue
    // read is served from the queue just built.
    stage = "queue";
    const [, queueMs] = timed(() =>
      engine.run_method(SCHEDULER, GET_QUEUED_CARDS, new Uint8Array([0x08, 0x01])),
    );
    report.queue_ms = queueMs;

    stage = "answer";
    const before = cardsTable();
    const [cid, answerMs] = timed(() => engine.answer_first_card(1000));
    report.answer_ms = answerMs;
    const revlogAfterAnswer = revlogRows(cid);
    const afterAnswer = cardsTable();

    stage = "undo";
    const [, undoMs] = timed(() => engine.undo());
    report.undo_ms = undoMs;
    const revlogAfterUndo = revlogRows(cid);
    const afterUndo = cardsTable();
    report.undo = {
      revlog_rows_after_answer: revlogAfterAnswer,
      revlog_rows_after_undo: revlogAfterUndo,
      cards_changed_by_answer: before !== afterAnswer,
      cards_restored_by_undo: before === afterUndo,
    };
    report.undo.correct =
      revlogAfterAnswer === 1 &&
      revlogAfterUndo === 0 &&
      report.undo.cards_changed_by_answer &&
      report.undo.cards_restored_by_undo;
    engine.close_collection();

    // Persistence: reopen from OPFS and read the notes back.
    stage = "reopen";
    engine.open_collection(COLLECTION);
    report.notes_after_reopen = query("select count() from notes")[0][0];
    engine.close_collection();
    report.opfs_files = engine.opfs_files();
    report.ok = true;
  } catch (error) {
    let panic = null;
    try {
      panic = engine.last_panic();
    } catch {
      // The module may not have loaded; the error itself is reported below.
    }
    report.errors.push(`${stage}: ${error?.message ?? error}${panic ? ` (panic: ${panic})` : ""}`);
  }
  self.postMessage(report);
};
