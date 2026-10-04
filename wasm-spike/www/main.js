// Starts the scenario in a dedicated Worker, because the OPFS SyncAccessHandle pool is available
// only there, and publishes the Worker's report as a promise for the test to await.
const notes = Number(new URLSearchParams(location.search).get("notes") ?? 300);
const worker = new Worker(new URL("./worker.js", import.meta.url), { type: "module" });

window.spikeReport = new Promise((resolve) => {
  worker.onmessage = (event) => resolve(event.data);
  worker.onerror = (event) =>
    resolve({ ok: false, errors: [`worker error: ${event.message ?? "unknown"}`] });
});

worker.postMessage({ notes });
