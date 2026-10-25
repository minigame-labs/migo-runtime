// WebKit cannot construct a nested Worker inside the producer. The page creates
// one and hands the producer a direct MessagePort; game payloads bypass the page.
const BOOTSTRAP_URL = new URL("./worker-nested-bootstrap.js", import.meta.url);
let active = null;

function teardown(id) {
  if (active === null || active.id !== id) return;
  active.worker.terminate();
  active = null;
}

export function bindWorkerRelay(port) {
  port.onmessage = (event) => {
    const message = event.data;
    if (!message || typeof message !== "object") return;
    switch (message.k) {
      case "create": {
        const { id, path } = message;
        if (active !== null) teardown(active.id);
        let worker, channel;
        try {
          const scriptURL = new URL(path, new URL("/", location.href));
          const bootstrapURL = new URL(BOOTSTRAP_URL);
          bootstrapURL.search = "script=" + encodeURIComponent(scriptURL.href);
          worker = new Worker(bootstrapURL);
          channel = new MessageChannel();
          const attempt = { id, worker, errors: new Map(), errorBytes: 0, nextError: 0 };
          active = attempt;
          worker.onerror = (event) => {
            if (active !== attempt || attempt.errors.size >= 16) return;
            // Bound independent diagnostics as well as data. A game can throw
            // huge strings; truncation here cannot affect a game payload.
            const detail = JSON.stringify({ message: String(event.message).slice(0, 32768),
              filename: String(event.filename).slice(0, 4096), lineno: event.lineno });
            const bytes = detail.length * 2;
            if (bytes > 1024 * 1024 - attempt.errorBytes) return;
            const seq = ++attempt.nextError;
            attempt.errors.set(seq, bytes); attempt.errorBytes += bytes;
            port.postMessage({ k: "err", id, seq, detail });
          };
          worker.postMessage({ k: "migo-worker-init", port: channel.port1 }, [channel.port1]);
          port.postMessage({ k: "created", id, port: channel.port2 }, [channel.port2]);
        } catch (error) {
          worker?.terminate(); channel?.port1.close(); channel?.port2.close();
          if (active?.id === id) active = null;
          port.postMessage({ k: "create-failed", id, detail: String(error) });
        }
        break;
      }
      case "error-ack": {
        if (active === null || active.id !== message.id) break;
        const bytes = active.errors.get(message.seq);
        if (bytes !== undefined) { active.errorBytes -= bytes; active.errors.delete(message.seq); }
        break;
      }
      case "term": teardown(message.id); break;
    }
  };
}
