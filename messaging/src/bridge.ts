// Moves messages between Spectrum and ferry-core. No business logic: it
// forwards each inbound event, sends what core returns, and keeps the
// handles (spaces, attachments) that later sends and fetches need.

import { CoreClient } from "./core-client";
import { errorText, log } from "./log";
import { maskHandle, maskSpaceId } from "./mask";
import { isTelegramChatId, isTelegramGroupId, MAX_TELEGRAM_TEXT } from "./telegram";
import { withTimeout } from "./timeout";
import {
  ChatMigratedError,
  NotAdminError,
  RecipientUnreachableError,
  UnsupportedSpaceError,
  type Button,
  type Channel,
  type ChatSpace,
  type InboundAttachment,
  type InboundMessage,
  type InboundPayload,
  type Transport,
} from "./types";

export const TASK_TIMEOUT_MS = 15_000;
/** Inbound messages that may wait behind the running one in a space; past this the oldest waiting one is dropped. */
export const MAX_QUEUED_PER_SPACE = 20;
/** Bounds each platform call made for core (send, create, rename, invite link, callback answer). */
export const SEND_TIMEOUT_MS = 10_000;
const MEMBERS_TIMEOUT_MS = 2_000;
const MEMBERS_TTL_MS = 30_000;
const ATTACHMENT_TTL_MS = 60 * 60_000;
const MAX_SPACES = 5_000;
const MAX_ATTACHMENTS = 1_000;

/** A failure the internal API turns into an HTTP status. */
export class BridgeError extends Error {
  constructor(
    readonly status: number,
    readonly code: string,
    message: string,
    readonly details?: unknown,
  ) {
    super(message);
  }
}

export interface BridgeOptions {
  core: CoreClient;
  sidecarUrl: string;
  echo: boolean;
  agentPhone: string | undefined;
  /** The providers this sidecar carries: asked for in MESSAGING_PROVIDERS and configured. */
  channels: Channel[];
  /** The live connection, or undefined while disabled or reconnecting. */
  transport: () => Transport | undefined;
  taskTimeoutMs?: number;
  sendTimeoutMs?: number;
  maxQueuedPerSpace?: number;
}

/** One space's inbound messages: waiting ones, and the loop that runs them in order. */
interface SpaceQueue {
  waiting: InboundMessage[];
  drained: Promise<void>;
}

export interface SendRequest {
  space_id?: string;
  to_phones?: string[];
  to_user_ids?: string[];
  text: string;
  buttons?: Button[];
}

export interface GroupRequest {
  phones: string[];
  name: string;
  intro_text: string;
}

export interface InviteLinkRequest {
  space_id: string;
  name?: string;
}

/** Which provider a space id belongs to: Telegram chat ids are integers, iMessage ids are chat GUIDs. */
export function channelOf(spaceId: string): Channel {
  return isTelegramChatId(spaceId) ? "telegram" : "imessage";
}

export class Bridge {
  private readonly spaces = new Map<string, ChatSpace>();
  private readonly attachments = new Map<string, { attachment: InboundAttachment; expiresAt: number }>();
  private readonly members = new Map<string, { ids: string[]; at: number }>();
  private readonly queues = new Map<string, SpaceQueue>();
  private readonly taskTimeoutMs: number;
  private readonly sendTimeoutMs: number;
  private readonly maxQueued: number;

  constructor(private readonly opts: BridgeOptions) {
    this.taskTimeoutMs = opts.taskTimeoutMs ?? TASK_TIMEOUT_MS;
    this.sendTimeoutMs = opts.sendTimeoutMs ?? SEND_TIMEOUT_MS;
    this.maxQueued = opts.maxQueuedPerSpace ?? MAX_QUEUED_PER_SPACE;
  }

  /**
   * Queues one inbound event and returns at once, so the inbound loop never
   * blocks. A button press is answered here, ahead of its space's queue, so
   * Telegram stops the button's spinner whatever core does. Events in the
   * same space run in order, each with a timeout that cancels it. At most
   * MAX_QUEUED_PER_SPACE wait per space; past that the oldest waiting one is
   * dropped.
   */
  dispatch(msg: InboundMessage): void {
    if (msg.acknowledge) this.acknowledge(msg);
    const key = msg.space.id;
    const queue = this.queues.get(key);
    if (!queue) {
      const fresh: SpaceQueue = { waiting: [msg], drained: Promise.resolve() };
      this.queues.set(key, fresh);
      // Starts on a later microtask, so no handler code runs inside dispatch.
      fresh.drained = Promise.resolve().then(() => this.drain(key, fresh));
      return;
    }
    queue.waiting.push(msg);
    if (queue.waiting.length > this.maxQueued) {
      const dropped = queue.waiting.shift();
      log.warn("inbound queue full; dropped the oldest waiting message", {
        space: maskSpaceId(key),
        from: maskHandle(dropped?.senderId),
        waiting: queue.waiting.length,
      });
    }
  }

  /** Waits for every queued inbound task (tests and shutdown). */
  async idle(): Promise<void> {
    while (this.queues.size > 0) await Promise.all([...this.queues.values()].map((queue) => queue.drained));
  }

  private acknowledge(msg: InboundMessage): void {
    const ack = msg.acknowledge as () => Promise<void>;
    // Started in a later microtask, like the inbound task, and never awaited by dispatch.
    void Promise.resolve()
      .then(() => withTimeout(ack.call(msg), this.sendTimeoutMs, "callback answer"))
      .catch((err) => log.warn("callback answer failed", { space: maskSpaceId(msg.space.id), error: errorText(err) }));
  }

  private async drain(key: string, queue: SpaceQueue): Promise<void> {
    for (let msg = queue.waiting.shift(); msg; msg = queue.waiting.shift()) await this.runTask(msg);
    this.queues.delete(key);
  }

  private async runTask(msg: InboundMessage): Promise<void> {
    const controller = new AbortController();
    try {
      await withTimeout(this.handleInbound(msg, controller.signal), this.taskTimeoutMs, "inbound task");
    } catch (err) {
      // The queue moves on to the next message now, so whatever this task has
      // left (the call to core, replies not yet sent) must not happen later.
      controller.abort();
      log.warn("inbound task failed", { space: maskSpaceId(msg.space.id), error: errorText(err) });
    }
  }

  /** Handles one inbound event. Once signal aborts, it calls core no more and sends no further reply. */
  async handleInbound(msg: InboundMessage, signal?: AbortSignal): Promise<void> {
    this.rememberSpace(msg.space);
    const fields = {
      space: maskSpaceId(msg.space.id),
      from: maskHandle(msg.senderId),
      group: msg.space.isGroup,
      channel: msg.channel,
      kind: msg.kind,
    };

    if (this.opts.echo) {
      await this.echo(msg, fields, signal);
      return;
    }

    const payload = await this.toPayload(msg);
    const replies = await this.opts.core.forward(payload, signal);
    if (replies === undefined) {
      // A cancelled task was already logged as timed out.
      if (!signal?.aborted) log.warn("core unavailable; no reply sent", fields);
      return;
    }
    log.info("forwarded inbound message", { ...fields, replies: replies.length });
    for (const [index, reply] of replies.entries()) {
      if (signal?.aborted) {
        log.warn("inbound task timed out; replies not sent", { ...fields, skipped: replies.length - index });
        return;
      }
      try {
        await this.sendToSpaceId(reply.space_id, reply.text);
      } catch (err) {
        log.warn("reply send failed", { space: maskSpaceId(reply.space_id), error: errorText(err) });
      }
    }
  }

  /** The M0 proof, per kind: a message is echoed with typing first, a button press names its data, a join or promotion says so. */
  private async echo(msg: InboundMessage, fields: Record<string, string | boolean>, signal?: AbortSignal): Promise<void> {
    if (msg.kind === "membership") {
      if (!msg.membership?.inChat || !msg.space.isGroup) {
        log.info("membership event (no echo: the bot is not in the group)", { ...fields, in_chat: msg.membership?.inChat });
        return;
      }
      if (signal?.aborted) return;
      const { migratedFrom, isAdmin } = msg.membership;
      await msg.reply(
        migratedFrom ? "echo: this group moved to a new chat id" : isAdmin ? "echo: Ferry is an admin in this group" : "echo: Ferry joined this group",
      );
      log.info("echoed membership event", fields);
      return;
    }
    if (msg.kind === "callback") {
      if (signal?.aborted) return;
      await msg.reply(`echo: button ${msg.callbackData ?? ""}`);
      log.info("echoed button press", fields);
      return;
    }
    await msg.space.responding(async () => {
      if (signal?.aborted) return;
      await msg.reply(`echo: ${msg.text || "(no text)"}`);
      log.info("echoed inbound message", fields);
    });
  }

  async send(req: SendRequest): Promise<{ space_id: string }> {
    if (req.to_phones !== undefined) {
      if (!this.opts.channels.includes("imessage")) {
        throw new BridgeError(400, "phones_unsupported", "this sidecar carries Telegram, which has no phone numbers: use space_id or to_user_ids");
      }
      if (req.buttons) throw new BridgeError(400, "buttons_unsupported", "iMessage has no buttons");
      const transport = this.requireTransport();
      const space = await this.createSpace(transport, req.to_phones);
      await this.guard("send", () => withTimeout(space.sendText(req.text), this.sendTimeoutMs, "send"));
      return { space_id: space.id };
    }

    let target: string;
    if (req.to_user_ids !== undefined) {
      this.requireChannel("telegram");
      if (req.to_user_ids.length !== 1) {
        throw new BridgeError(501, "unsupported", "Telegram bots cannot create group chats: send to one user at a time");
      }
      // A Telegram DM's chat id is the user's id.
      target = req.to_user_ids[0] as string;
    } else {
      target = req.space_id as string;
      this.requireChannel(channelOf(target));
    }
    const channel = channelOf(target);
    if (channel === "imessage" && req.buttons) throw new BridgeError(400, "buttons_unsupported", "iMessage has no buttons");
    if (channel === "telegram" && req.text.length > MAX_TELEGRAM_TEXT) {
      throw new BridgeError(400, "invalid_request", `text is longer than Telegram's ${MAX_TELEGRAM_TEXT} characters`);
    }
    const transport = this.requireTransport();
    await this.guard("send", () => this.sendToSpaceId(target, req.text, transport, req.buttons));
    return { space_id: target };
  }

  async createGroup(req: GroupRequest): Promise<{ space_id: string }> {
    if (!this.opts.channels.includes("imessage")) {
      throw new BridgeError(501, "unsupported", "Telegram bots cannot create groups: add the bot to a group, and use POST /invite-links there");
    }
    const transport = this.requireTransport();
    const space = await this.createSpace(transport, req.phones);
    try {
      await withTimeout(space.rename(req.name), this.sendTimeoutMs, "rename");
    } catch (err) {
      log.warn("group rename failed", { space: maskSpaceId(space.id), error: errorText(err) });
    }
    await this.guard("group intro", () => withTimeout(space.sendText(req.intro_text), this.sendTimeoutMs, "group intro"), {
      space_id: space.id,
    });
    log.info("group created", { space: maskSpaceId(space.id), members: req.phones.length });
    return { space_id: space.id };
  }

  /** A new invite link to a Telegram group the bot administers. */
  async createInviteLink(req: InviteLinkRequest): Promise<{ url: string }> {
    this.requireChannel("telegram");
    if (!isTelegramGroupId(req.space_id)) {
      throw new BridgeError(400, "invalid_request", "space_id must be a Telegram group's chat id (a negative number)");
    }
    const transport = this.requireTransport();
    const create = transport.createInviteLink?.bind(transport);
    if (!create) throw new BridgeError(501, "unsupported", "this connection cannot create invite links");
    const url = await this.guard("invite link", () => withTimeout(create(req.space_id, req.name), this.sendTimeoutMs, "invite link"));
    log.info("invite link created", { space: maskSpaceId(req.space_id) });
    return { url };
  }

  /** An attachment handed to core by URL, while it is still cached. */
  attachment(id: string): InboundAttachment | undefined {
    const entry = this.attachments.get(id);
    if (!entry) return undefined;
    if (entry.expiresAt < Date.now()) {
      this.attachments.delete(id);
      return undefined;
    }
    return entry.attachment;
  }

  /** Drops handles bound to a closed connection. */
  reset(): void {
    this.spaces.clear();
    this.attachments.clear();
    this.members.clear();
  }

  private async toPayload(msg: InboundMessage): Promise<InboundPayload> {
    const sender = msg.senderId ?? "";
    const participants = msg.space.isGroup ? await this.groupMembers(msg.space, sender) : sender ? [sender] : [];
    const payload: InboundPayload = {
      kind: msg.kind,
      channel: msg.channel,
      space_id: msg.space.id,
      is_group: msg.space.isGroup,
      participants,
      sender_phone: msg.channel === "imessage" ? sender : null,
      sender_id: msg.senderId ?? null,
      sender_name: msg.senderName ?? null,
      sender_username: msg.senderUsername ?? null,
      chat_title: msg.chatTitle ?? null,
      text: msg.text,
      attachments: msg.attachments.map((a) => ({
        type: a.mimeType,
        url: `${this.opts.sidecarUrl}/attachments/${this.storeAttachment(a)}`,
      })),
      received_at: msg.receivedAt.toISOString(),
    };
    if (msg.location) payload.location = msg.location;
    if (msg.kind === "callback") payload.callback_data = msg.callbackData ?? "";
    if (msg.kind === "membership" && msg.membership) {
      payload.bot_in_chat = msg.membership.inChat;
      payload.bot_is_admin = msg.membership.isAdmin;
      if (msg.membership.migratedFrom) payload.migrated_from_space_id = msg.membership.migratedFrom;
    }
    return payload;
  }

  private async groupMembers(space: ChatSpace, sender: string): Promise<string[]> {
    const cached = this.members.get(space.id);
    let ids: string[];
    if (cached && Date.now() - cached.at < MEMBERS_TTL_MS) {
      ids = cached.ids;
    } else {
      try {
        ids = await withTimeout(space.memberIds(), MEMBERS_TIMEOUT_MS, "getMembers");
        this.members.set(space.id, { ids, at: Date.now() });
      } catch (err) {
        log.warn("group members unavailable; sending the sender only", { space: maskSpaceId(space.id), error: errorText(err) });
        ids = [];
      }
    }
    // The SDK already leaves out a dedicated line's own number. On the shared
    // pool it cannot, and core gets the pool number too (see README).
    const own = [this.opts.agentPhone, space.ownHandle];
    const out = ids.filter((id) => !own.includes(id));
    if (sender && !out.includes(sender)) out.push(sender);
    return out;
  }

  private storeAttachment(attachment: InboundAttachment): string {
    const id = crypto.randomUUID();
    this.attachments.set(id, { attachment, expiresAt: Date.now() + ATTACHMENT_TTL_MS });
    trim(this.attachments, MAX_ATTACHMENTS);
    return id;
  }

  private rememberSpace(space: ChatSpace): void {
    this.spaces.delete(space.id);
    this.spaces.set(space.id, space);
    trim(this.spaces, MAX_SPACES);
  }

  private async sendToSpaceId(id: string, text: string, transport = this.requireTransport(), buttons?: Button[]): Promise<void> {
    await withTimeout(
      (async () => {
        let space = this.spaces.get(id);
        if (!space) {
          space = await transport.getSpace(id);
          this.rememberSpace(space);
        }
        await space.sendText(text, buttons);
      })(),
      this.sendTimeoutMs,
      "send",
    );
  }

  private async createSpace(transport: Transport, handles: string[]): Promise<ChatSpace> {
    try {
      const space = await withTimeout(transport.createSpace(handles), this.sendTimeoutMs, "create chat");
      this.rememberSpace(space);
      return space;
    } catch (err) {
      if (err instanceof UnsupportedSpaceError) {
        throw new BridgeError(501, "unsupported", "this iMessage line cannot create that chat (group creation needs a dedicated line)");
      }
      log.warn("space create failed", { error: errorText(err) });
      throw new BridgeError(502, "send_failed", "could not create the chat");
    }
  }

  private requireChannel(channel: Channel): void {
    if (!this.opts.channels.includes(channel)) {
      throw new BridgeError(400, "channel_disabled", `${channel} is not enabled on this sidecar (MESSAGING_PROVIDERS and its credentials)`);
    }
  }

  private requireTransport(): Transport {
    const transport = this.opts.transport();
    if (!transport) throw new BridgeError(503, "spectrum_unavailable", "Spectrum is not connected");
    return transport;
  }

  private async guard<T>(what: string, fn: () => Promise<T>, details?: unknown): Promise<T> {
    try {
      return await fn();
    } catch (err) {
      if (err instanceof BridgeError) throw err;
      if (err instanceof RecipientUnreachableError) {
        throw new BridgeError(403, "recipient_not_reachable", `Telegram refused the ${what}: ${err.message}. The user must open the bot and press Start, and not block it; in a group, the bot must still be a member.`);
      }
      if (err instanceof ChatMigratedError) {
        throw new BridgeError(409, "chat_migrated", `the group became a supergroup with a new chat id: use details.space_id`, {
          space_id: err.newSpaceId,
        });
      }
      if (err instanceof NotAdminError) {
        throw new BridgeError(403, "not_admin", `the bot is not an administrator of this group: ${err.message}`);
      }
      if (err instanceof UnsupportedSpaceError) throw new BridgeError(501, "unsupported", err.message);
      log.warn(`${what} failed`, { error: errorText(err) });
      throw new BridgeError(502, "send_failed", `${what} failed`, details);
    }
  }
}

function trim<K, V>(map: Map<K, V>, max: number): void {
  while (map.size > max) {
    const oldest = map.keys().next();
    if (oldest.done) return;
    map.delete(oldest.value);
  }
}
