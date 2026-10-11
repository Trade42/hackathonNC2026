// Fakes for the Transport seam, so no test needs Photon.

import { setLogSink, type Level } from "../src/log";
import {
  UnsupportedSpaceError,
  type Button,
  type Channel,
  type ChatSpace,
  type InboundAttachment,
  type InboundKind,
  type InboundMessage,
  type LatLng,
  type Membership,
  type Transport,
} from "../src/types";

/** A push-driven async iterable: push values, then end() or fail(). */
export class Queue<T> implements AsyncIterable<T> {
  private readonly items: T[] = [];
  private waiters: ((r: IteratorResult<T>) => void)[] = [];
  private failers: ((e: unknown) => void)[] = [];
  private done = false;
  private error: unknown;

  push(value: T): void {
    const waiter = this.waiters.shift();
    this.failers.shift();
    if (waiter) waiter({ value, done: false });
    else this.items.push(value);
  }

  end(): void {
    this.done = true;
    for (const waiter of this.waiters.splice(0)) waiter({ value: undefined, done: true });
    this.failers = [];
  }

  fail(error: unknown): void {
    this.error = error;
    this.done = true;
    for (const fail of this.failers.splice(0)) fail(error);
    this.waiters = [];
  }

  [Symbol.asyncIterator](): AsyncIterator<T> {
    return {
      next: () => {
        if (this.items.length > 0) return Promise.resolve({ value: this.items.shift() as T, done: false });
        if (this.error !== undefined) return Promise.reject(this.error);
        if (this.done) return Promise.resolve({ value: undefined, done: true });
        return new Promise((resolve, reject) => {
          this.waiters.push(resolve);
          this.failers.push(reject);
        });
      },
    };
  }
}

export class FakeSpace implements ChatSpace {
  readonly sent: string[] = [];
  /** The buttons sent with each text, in order. */
  readonly buttons: (Button[] | undefined)[] = [];
  readonly events: string[] = [];
  renamed: string | undefined;
  failSend = false;
  /** Thrown by every send, as a platform refusal would be. */
  failWith: Error | undefined;
  ownHandle: string | undefined;

  constructor(
    readonly id: string,
    readonly isGroup = false,
    public members: string[] = [],
  ) {}

  async sendText(text: string, buttons?: Button[]): Promise<void> {
    if (this.failWith) throw this.failWith;
    if (this.failSend) throw new Error("send failed for any;-;+19195550123");
    this.sent.push(text);
    this.buttons.push(buttons);
    this.events.push(`send:${text}`);
  }

  async responding<T>(fn: () => Promise<T>): Promise<T> {
    this.events.push("typing:start");
    try {
      return await fn();
    } finally {
      this.events.push("typing:stop");
    }
  }

  async memberIds(): Promise<string[]> {
    if (!this.isGroup) throw new Error("only group chats support listing members");
    return this.members;
  }

  async rename(name: string): Promise<void> {
    this.renamed = name;
  }
}

export class FakeTransport implements Transport {
  readonly queue = new Queue<InboundMessage>();
  readonly messages: AsyncIterable<InboundMessage> = this.queue;
  readonly spaces = new Map<string, FakeSpace>();
  readonly created: string[][] = [];
  /** Group ids (and names) createInviteLink was asked for. */
  readonly inviteLinks: { spaceId: string; name: string | undefined }[] = [];
  inviteError: Error | undefined;
  canCreateGroups = false;
  stopped = false;

  space(id: string, isGroup = false, members: string[] = []): FakeSpace {
    const space = new FakeSpace(id, isGroup, members);
    this.spaces.set(id, space);
    return space;
  }

  async getSpace(id: string): Promise<ChatSpace> {
    // iMessage groups have ";+;" in their GUID; Telegram groups have negative ids.
    return this.spaces.get(id) ?? this.space(id, id.includes(";+;") || id.startsWith("-"));
  }

  async createInviteLink(spaceId: string, name?: string): Promise<string> {
    if (this.inviteError) throw this.inviteError;
    this.inviteLinks.push({ spaceId, name });
    return `https://t.me/+invite${this.inviteLinks.length}`;
  }

  async createSpace(handles: string[]): Promise<ChatSpace> {
    if (handles.length > 1 && !this.canCreateGroups) {
      throw new UnsupportedSpaceError("shared mode cannot create group chats");
    }
    this.created.push(handles);
    const id = handles.length === 1 ? `any;-;${handles[0]}` : `any;+;chat${this.created.length}`;
    return this.space(id, handles.length > 1, handles);
  }

  async stop(): Promise<void> {
    this.stopped = true;
    this.queue.end();
  }
}

export interface MessageInit {
  kind?: InboundKind;
  channel?: Channel;
  text?: string;
  sender?: string;
  senderName?: string;
  senderUsername?: string;
  chatTitle?: string;
  callbackData?: string;
  membership?: Membership;
  acknowledge?: () => Promise<void>;
  attachments?: InboundAttachment[];
  location?: LatLng;
  receivedAt?: Date;
}

export function message(space: FakeSpace, init: MessageInit = {}): InboundMessage {
  const msg: InboundMessage = {
    kind: init.kind ?? "message",
    channel: init.channel ?? "imessage",
    space,
    senderId: init.sender ?? "+19195550101",
    text: init.text ?? "hello",
    attachments: init.attachments ?? [],
    receivedAt: init.receivedAt ?? new Date("2026-10-10T22:00:00.000Z"),
    reply: async (text) => {
      space.sent.push(text);
      space.events.push(`reply:${text}`);
    },
  };
  if (init.location) msg.location = init.location;
  if (init.senderName) msg.senderName = init.senderName;
  if (init.senderUsername) msg.senderUsername = init.senderUsername;
  if (init.chatTitle) msg.chatTitle = init.chatTitle;
  if (init.callbackData) msg.callbackData = init.callbackData;
  if (init.membership) msg.membership = init.membership;
  if (init.acknowledge) msg.acknowledge = init.acknowledge;
  return msg;
}

export function bytesAttachment(text: string, mimeType = "image/jpeg", name = "photo.jpg"): InboundAttachment {
  return {
    mimeType,
    name,
    stream: async () => new Response(text).body as ReadableStream<Uint8Array>,
  };
}

/** Captures log lines until the returned restore() runs. */
export function captureLogs(): { lines: string[]; restore: () => void } {
  const lines: string[] = [];
  const previous = setLogSink((_level: Level, line: string) => lines.push(line));
  return { lines, restore: () => setLogSink(previous) };
}

export async function until(check: () => boolean, timeoutMs = 2_000): Promise<void> {
  const start = Date.now();
  while (!check()) {
    if (Date.now() - start > timeoutMs) throw new Error("condition not met in time");
    await Bun.sleep(5);
  }
}
