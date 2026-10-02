import { createServer } from "node:http";

import { app } from "./app.mjs";

const server = createServer(async (incoming, outgoing) => {
  try {
    const request = new Request(new URL(incoming.url ?? "/", `http://${incoming.headers.host ?? "127.0.0.1"}`), {
      method: incoming.method,
      headers: incoming.headers,
    });
    const response = await app.fetch(request);
    outgoing.writeHead(response.status, Object.fromEntries(response.headers));
    outgoing.end(Buffer.from(await response.arrayBuffer()));
  } catch {
    outgoing.writeHead(500, { "content-type": "text/plain; charset=utf-8" });
    outgoing.end("Internal server error");
  }
});

const port = Number(process.env.PORT ?? 3000);
server.listen(port, "127.0.0.1", () => {
  console.log(`news-site fixture listening on http://127.0.0.1:${port}`);
});
