/** Contrato del puente Tauri: JSON conserva texto y base64 conserva otros bytes. */
interface Reply {
  status: number;
  headers: Record<string, string>;
  body: string;
  bodyBase64?: string;
}

type Invoke = <T>(command: string, args: Record<string, unknown>) => Promise<T>;

function encode(bytes: Uint8Array): string {
  const chunks: string[] = [];
  for (let start = 0; start < bytes.length; start += 32768) {
    chunks.push(String.fromCharCode(...bytes.subarray(start, start + 32768)));
  }
  return btoa(chunks.join(""));
}

/** Stops waiting when aborted; a command already sent may keep running. */
function waiting<T>(run: () => Promise<T>, signal: AbortSignal): Promise<T> {
  return new Promise((resolve, reject) => {
    const aborted = () => {
      signal.removeEventListener("abort", aborted);
      reject(signal.reason);
    };
    signal.addEventListener("abort", aborted, { once: true });
    if (signal.aborted) {
      aborted();
      return;
    }
    Promise.resolve().then(() => {
      signal.throwIfAborted();
      return run();
    }).then(
      (value) => { signal.removeEventListener("abort", aborted); resolve(value); },
      (error) => { signal.removeEventListener("abort", aborted); reject(error); },
    );
  });
}

/** Adapts the Studio's requests to the native command, without depending on window. */
export function createDesktopFetch(invoke: Invoke): typeof fetch {
  return async (input, init) => {
    const request = new Request(input, init);
    request.signal.throwIfAborted();
    const headers: Record<string, string> = {};
    request.headers.forEach((value, name) => { headers[name] = value; });
    const bodyBase64 = request.body === null ? null : encode(new Uint8Array(await waiting(() => request.arrayBuffer(), request.signal)));
    let reply: Reply;
    try {
      reply = await waiting(() => invoke<Reply>("request", {
        method: request.method, url: request.url, headers, bodyBase64,
      }), request.signal);
    } catch (error) {
      if (request.signal.aborted) throw request.signal.reason;
      throw new TypeError(String(error), { cause: error });
    }
    const empty = request.method === "HEAD" || [204, 205, 304].includes(reply.status);
    const body = reply.bodyBase64 === undefined
      ? reply.body
      : Uint8Array.from(atob(reply.bodyBase64), (character) => character.charCodeAt(0));
    return new Response(empty ? null : body, { status: reply.status, headers: reply.headers });
  };
}
