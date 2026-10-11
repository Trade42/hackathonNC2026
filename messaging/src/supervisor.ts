// Keeps one Spectrum connection alive: connects, pumps inbound messages to
// the bridge, and reconnects with exponential backoff when the stream ends
// or throws, or when connecting hangs.

import { errorText, log } from "./log";
import { withTimeout } from "./timeout";
import type { InboundMessage, Transport, TransportFactory } from "./types";

export type SpectrumStatus = "connected" | "disconnected" | "disabled";

export interface Backoff {
  initialMs: number;
  maxMs: number;
  /** A connection that lasted this long resets the delay to initialMs. */
  stableMs: number;
}

export const DEFAULT_BACKOFF: Backoff = { initialMs: 1_000, maxMs: 30_000, stableMs: 30_000 };
/** A Spectrum() that has not connected by then is abandoned and retried with backoff. */
export const CONNECT_TIMEOUT_MS = 30_000;

export interface SupervisorOptions {
  /** Undefined when Photon credentials are missing: Spectrum stays disabled. */
  factory: TransportFactory | undefined;
  onMessage: (msg: InboundMessage) => void;
  /** Called after each connection closes, so stale handles are dropped. */
  onDisconnect?: () => void;
  backoff?: Backoff;
  connectTimeoutMs?: number;
}

export class Supervisor {
  private state: SpectrumStatus;
  private transport: Transport | undefined;
  private stopped = false;
  private wake: (() => void) | undefined;
  private readonly backoff: Backoff;
  private readonly connectTimeoutMs: number;
  /** Successful connects so far (tests read it). */
  connects = 0;

  constructor(private readonly opts: SupervisorOptions) {
    this.state = opts.factory ? "disconnected" : "disabled";
    this.backoff = opts.backoff ?? DEFAULT_BACKOFF;
    this.connectTimeoutMs = opts.connectTimeoutMs ?? CONNECT_TIMEOUT_MS;
  }

  get status(): SpectrumStatus {
    return this.state;
  }

  /** The live connection, if any. */
  current(): Transport | undefined {
    return this.state === "connected" ? this.transport : undefined;
  }

  /** Runs until stop(). Resolves at once when Spectrum is disabled. */
  async run(): Promise<void> {
    const factory = this.opts.factory;
    if (!factory) return;
    let delay = this.backoff.initialMs;
    while (!this.stopped) {
      const startedAt = Date.now();
      try {
        this.transport = await this.connect(factory);
        if (this.stopped) break;
        this.state = "connected";
        this.connects++;
        log.info("spectrum connected");
        for await (const msg of this.transport.messages) {
          if (this.stopped) break;
          this.opts.onMessage(msg);
        }
        if (!this.stopped) log.warn("spectrum stream ended");
      } catch (err) {
        if (!this.stopped) log.warn("spectrum connection failed", { error: errorText(err) });
      }
      this.state = "disconnected";
      await this.closeTransport();
      this.opts.onDisconnect?.();
      if (this.stopped) break;
      if (Date.now() - startedAt >= this.backoff.stableMs) delay = this.backoff.initialMs;
      log.info("spectrum reconnecting", { in_ms: delay });
      await this.sleep(delay);
      delay = Math.min(delay * 2, this.backoff.maxMs);
    }
    await this.closeTransport();
  }

  async stop(): Promise<void> {
    this.stopped = true;
    this.wake?.();
    await this.closeTransport();
  }

  private async connect(factory: TransportFactory): Promise<Transport> {
    const pending = factory();
    try {
      return await withTimeout(pending, this.connectTimeoutMs, "spectrum connect");
    } catch (err) {
      // A connect that finishes after the timeout is stopped at once, so it
      // never runs beside the connection that replaces it.
      pending.then(
        (late) => {
          log.info("stopping a spectrum connection that finished after its timeout");
          return late.stop();
        },
        () => {},
      ).catch((stopErr) => log.warn("spectrum stop failed", { error: errorText(stopErr) }));
      throw err;
    }
  }

  private async closeTransport(): Promise<void> {
    const transport = this.transport;
    this.transport = undefined;
    if (!transport) return;
    try {
      await transport.stop();
    } catch (err) {
      log.warn("spectrum stop failed", { error: errorText(err) });
    }
  }

  private sleep(ms: number): Promise<void> {
    return new Promise((resolve) => {
      const timer = setTimeout(done, ms);
      function done() {
        clearTimeout(timer);
        resolve();
      }
      this.wake = done;
    });
  }
}
