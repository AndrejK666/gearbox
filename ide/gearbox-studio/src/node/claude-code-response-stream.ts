import { LanguageModelStreamResponsePart } from "@theia/ai-core";

/** Bridge SDK callbacks to Theia's stream, with delivery backpressure. */
export class ClaudeCodeResponseStream {
  private readonly pending: {
    part: LanguageModelStreamResponsePart;
    delivered: () => void;
  }[] = [];
  private readonly deliveries = new Set<(error: Error) => void>();
  private wake?: () => void;
  private done = false;
  private error?: unknown;

  publish(part: LanguageModelStreamResponsePart): Promise<void> {
    if (this.done) return Promise.reject(new Error("Chat response stream is closed."));
    return new Promise<void>((resolve, reject) => {
      this.deliveries.add(reject);
      this.pending.push({part, delivered: () => {
        this.deliveries.delete(reject);
        resolve();
      }});
      this.wake?.();
    });
  }

  finish(error?: unknown): void {
    this.done = true;
    this.error = error;
    this.wake?.();
  }

  dispose(): void {
    this.finish();
    for (const reject of this.deliveries) reject(new Error("Chat response stream is closed."));
    this.deliveries.clear();
    this.pending.length = 0;
  }

  async *consume(): AsyncGenerator<LanguageModelStreamResponsePart> {
    try {
      while (true) {
        const next = this.pending.shift();
        if (next) {
          yield next.part;
          // The delegate has forwarded this frame before requesting the next one.
          // A tool handler must not run until its start frame is delivered.
          next.delivered();
        } else if (this.done) {
          if (this.error) throw this.error;
          return;
        } else {
          await new Promise<void>(resolve => { this.wake = resolve; });
          this.wake = undefined;
        }
      }
    } finally {
      this.dispose();
    }
  }
}
