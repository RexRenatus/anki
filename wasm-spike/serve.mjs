// A static server for the harness: www/ only, with the wasm content type browsers require for
// streaming compilation, and no caching between runs.
import { readFile } from "node:fs/promises";
import { createServer } from "node:http";
import { extname, join, normalize } from "node:path";
import { fileURLToPath } from "node:url";

const root = fileURLToPath(new URL("./www/", import.meta.url));
const port = Number(process.env.PORT ?? 8737);
const types = {
  ".html": "text/html; charset=utf-8",
  ".js": "text/javascript; charset=utf-8",
  ".wasm": "application/wasm",
};

createServer(async (request, response) => {
  const path = normalize(new URL(request.url, "http://localhost").pathname).replace(/^\/+/, "");
  if (path.startsWith("..")) {
    response.writeHead(403).end();
    return;
  }
  try {
    const body = await readFile(join(root, path || "index.html"));
    response.writeHead(200, {
      "content-type": types[extname(path || "index.html")] ?? "application/octet-stream",
      "cache-control": "no-store",
    });
    response.end(body);
  } catch {
    response.writeHead(404).end();
  }
}).listen(port, "localhost");
