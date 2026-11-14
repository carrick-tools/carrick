// The one LSP framer: `Content-Length` headers over a byte stream, JSON bodies.
//
// The language server reads its client with it, the test client reads the
// server with it, and the held compiler engine reads the compiler with it.
// A header block with no `Content-Length` is dropped and reading resumes
// after it; a body that is not JSON is reported and skipped.

export type LspMessage = {
  jsonrpc?: string;
  id?: number | string;
  method?: string;
  params?: Record<string, unknown>;
  result?: unknown;
  error?: { code: number; message: string };
};

export class Framer {
  private buffer = Buffer.alloc(0);

  private readonly onError: ((what: string) => void) | undefined;

  constructor(onError?: (what: string) => void) {
    this.onError = onError;
  }

  /** Every whole message the bytes so far complete, in order. */
  push(chunk: Buffer): LspMessage[] {
    this.buffer = Buffer.concat([this.buffer, chunk]);
    const out: LspMessage[] = [];
    for (;;) {
      const separator = this.buffer.indexOf("\r\n\r\n");
      if (separator === -1) return out;
      const header = this.buffer.subarray(0, separator).toString("ascii");
      const match = /Content-Length:\s*(\d+)/i.exec(header);
      if (!match?.[1]) {
        this.onError?.("header without Content-Length");
        this.buffer = this.buffer.subarray(separator + 4);
        continue;
      }
      const length = Number.parseInt(match[1], 10);
      if (this.buffer.length < separator + 4 + length) return out;
      const body = this.buffer.subarray(separator + 4, separator + 4 + length).toString("utf8");
      this.buffer = this.buffer.subarray(separator + 4 + length);
      try {
        out.push(JSON.parse(body) as LspMessage);
      } catch (error) {
        this.onError?.(`unparseable message ${String(error)}`);
      }
    }
  }
}

/** One message as it goes on the wire. */
export function frame(message: Record<string, unknown>): Buffer {
  const body = Buffer.from(JSON.stringify({ jsonrpc: "2.0", ...message }), "utf8");
  return Buffer.concat([Buffer.from(`Content-Length: ${body.length}\r\n\r\n`, "ascii"), body]);
}
