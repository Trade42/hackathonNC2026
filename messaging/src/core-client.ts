// Forwards one inbound message to ferry-core: 3 s timeout, one retry.

import { errorText, log } from "./log";
import type { CoreReply, InboundPayload } from "./types";

export const CORE_TIMEOUT_MS = 3_000;
const ATTEMPTS = 2;

export interface CoreClientOptions {
  coreUrl: string;
  sharedSecret: string;
  timeoutMs?: number;
}

export class CoreClient {
  private readonly url: string;
  private readonly secret: string;
  private readonly timeoutMs: number;

  constructor(opts: CoreClientOptions) {
    this.url = `${opts.coreUrl}/internal/messages/inbound`;
    this.secret = opts.sharedSecret;
    this.timeoutMs = opts.timeoutMs ?? CORE_TIMEOUT_MS;
  }

  /**
   * Core's replies, or undefined when core could not be reached or answered
   * badly. Aborting signal (the inbound task's timeout) ends the call and
   * skips the retry.
   */
  async forward(payload: InboundPayload, signal?: AbortSignal): Promise<CoreReply[] | undefined> {
    const body = JSON.stringify(payload);
    for (let attempt = 1; attempt <= ATTEMPTS; attempt++) {
      if (signal?.aborted) return undefined;
      const timeout = AbortSignal.timeout(this.timeoutMs);
      try {
        const res = await fetch(this.url, {
          method: "POST",
          headers: { "content-type": "application/json", "x-ferry-secret": this.secret },
          body,
          signal: signal ? AbortSignal.any([timeout, signal]) : timeout,
        });
        if (res.ok) return parseReplies(await res.json());
        // A 4xx will not change on retry.
        if (res.status < 500) {
          log.warn("core rejected inbound message", { status: res.status, space: payload.space_id });
          return undefined;
        }
        log.warn("core inbound failed", { status: res.status, attempt, space: payload.space_id });
      } catch (err) {
        if (signal?.aborted) return undefined;
        log.warn("core inbound failed", { error: errorText(err), attempt, space: payload.space_id });
      }
    }
    return undefined;
  }
}

function parseReplies(body: unknown): CoreReply[] {
  const replies = (body as { replies?: unknown } | null)?.replies;
  if (replies === undefined || replies === null) return [];
  if (!Array.isArray(replies)) {
    log.warn("core inbound response has no replies array");
    return [];
  }
  const out: CoreReply[] = [];
  for (const reply of replies) {
    const r = reply as { space_id?: unknown; text?: unknown } | null;
    if (typeof r?.space_id === "string" && r.space_id !== "" && typeof r.text === "string" && r.text !== "") {
      out.push({ space_id: r.space_id, text: r.text });
    } else {
      log.warn("dropping malformed reply from core");
    }
  }
  return out;
}
