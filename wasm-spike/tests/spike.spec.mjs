import { expect, test } from "@playwright/test";

test("the engine opens, answers and undoes over OPFS in a Worker", async ({ playwright, browserName, baseURL }, testInfo) => {
  // A persistent profile, as a user's browser has. An empty path is a fresh temporary profile.
  const context = await playwright[browserName].launchPersistentContext("", { baseURL });
  try {
    const page = await context.newPage();
    const logs = [];
    page.on("console", (message) => logs.push(message.text()));
    page.on("pageerror", (error) => logs.push(String(error)));

    await page.goto("/index.html");
    const report = await page.evaluate(() => window.spikeReport);
    report.browser = browserName;
    report.console = logs;

    // One line per run, read from the run log by the record.
    process.stdout.write(`SPIKE-REPORT ${JSON.stringify(report)}\n`);
    await testInfo.attach("report", {
      body: JSON.stringify(report, null, 2),
      contentType: "application/json",
    });

    expect(report.errors).toEqual([]);
    expect(report.notes_after_reopen).toBe(report.notes_added);
    expect(report.undo.revlog_rows_after_answer).toBe(1);
    expect(report.undo.revlog_rows_after_undo).toBe(0);
    expect(report.undo.cards_changed_by_answer).toBe(true);
    expect(report.undo.cards_restored_by_undo).toBe(true);
  } finally {
    await context.close();
  }
});
