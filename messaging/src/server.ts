// The internal API core calls. Every route needs X-Ferry-Secret (constant-time
// compare), else 401. Errors use the contract's {error: {code, message}} shape.

import { createHash, timingSafeEqual } from "node:crypto";
import { BridgeError, type Bridge, type GroupRequest, type InviteLinkRequest, type SendRequest } from "./bridge";
import { errorText, log } from "./log";
import type { SpectrumStatus } from "./supervisor";
import { MAX_CALLBACK_DATA_BYTES } from "./telegram";
import type { Button, Channel } from "./types";

export const MAX_TEXT = 10_000;
export const MAX_RECIPIENTS = 32;
export const MAX_BUTTONS = 8;
const MAX_BUTTON_LABEL = 64;
const MAX_URL = 2048;
/** Telegram's limit on an invite link's name. */
const MAX_INVITE_NAME = 32;
const MAX_BODY_BYTES = 64 * 1024;
const E164 = /^\+[1-9]\d{6,14}$/;
const TELEGRAM_USER_ID = /^[1-9]\d{0,19}$/;
const BUTTON_URL = /^(https?|tg):\/\/\S+$/i;

export interface ServerDeps {
  sharedSecret: string;
  bridge: Pick<Bridge, "send" | "createGroup" | "createInviteLink" | "attachment">;
  status: () => SpectrumStatus;
  /** The providers this sidecar carries, reported by /healthz. */
  channels: Channel[];
}

export function startServer(deps: ServerDeps, listen: { port: number; hostname: string }) {
  return Bun.serve({
    port: listen.port,
    hostname: listen.hostname,
    maxRequestBodySize: MAX_BODY_BYTES,
    fetch: createHandler(deps),
    error: (err) => {
      log.error("internal api error", { error: errorText(err) });
      return problem(500, "internal", "internal error");
    },
  });
}

export function createHandler(deps: ServerDeps): (req: Request) => Promise<Response> {
  const expected = digest(deps.sharedSecret);
  return async (req) => {
    if (!authorized(req.headers.get("x-ferry-secret"), expected)) {
      return problem(401, "unauthorized", "missing or wrong X-Ferry-Secret");
    }
    const { pathname } = new URL(req.url);
    try {
      if (pathname === "/healthz") {
        if (req.method !== "GET") return methodNotAllowed("GET");
        return json(200, { status: "ok", spectrum: deps.status(), providers: deps.channels });
      }
      if (pathname === "/send") {
        if (req.method !== "POST") return methodNotAllowed("POST");
        const body = await readJson(req);
        if (body instanceof Response) return body;
        const parsed = parseSend(body);
        if (typeof parsed === "string") return problem(400, "invalid_request", parsed);
        return json(200, await deps.bridge.send(parsed));
      }
      if (pathname === "/groups") {
        if (req.method !== "POST") return methodNotAllowed("POST");
        const body = await readJson(req);
        if (body instanceof Response) return body;
        const parsed = parseGroup(body);
        if (typeof parsed === "string") return problem(400, "invalid_request", parsed);
        return json(200, await deps.bridge.createGroup(parsed));
      }
      if (pathname === "/invite-links") {
        if (req.method !== "POST") return methodNotAllowed("POST");
        const body = await readJson(req);
        if (body instanceof Response) return body;
        const parsed = parseInviteLink(body);
        if (typeof parsed === "string") return problem(400, "invalid_request", parsed);
        return json(200, await deps.bridge.createInviteLink(parsed));
      }
      if (pathname.startsWith("/attachments/")) {
        if (req.method !== "GET") return methodNotAllowed("GET");
        return await serveAttachment(deps, pathname.slice("/attachments/".length));
      }
      return problem(404, "not_found", "no such route");
    } catch (err) {
      if (err instanceof BridgeError) return problem(err.status, err.code, err.message, err.details);
      log.error("internal api failure", { path: pathname, error: errorText(err) });
      return problem(500, "internal", "internal error");
    }
  };
}

function digest(value: string): Buffer {
  return createHash("sha256").update(value).digest();
}

/** Hashing first gives equal-length buffers, so the compare is constant time whatever the input length. */
function authorized(header: string | null, expected: Buffer): boolean {
  if (header === null) return false;
  return timingSafeEqual(digest(header), expected);
}

async function serveAttachment(deps: ServerDeps, id: string): Promise<Response> {
  const attachment = deps.bridge.attachment(id);
  if (!attachment) return problem(404, "not_found", "attachment unknown or expired");
  try {
    const body = guardStream(await attachment.stream());
    return new Response(body, {
      headers: {
        "content-type": attachment.mimeType,
        "content-disposition": `inline; filename="${attachment.name.replace(/[^\w.-]/g, "_")}"`,
        "cache-control": "private, no-store",
      },
    });
  } catch (err) {
    log.warn("attachment download failed", { error: errorText(err) });
    return problem(502, "attachment_unavailable", "could not fetch the attachment from the messaging platform");
  }
}

/**
 * Pumps the platform's stream through one the sidecar owns. Once headers are
 * sent, Bun prints a body stream's error (message and stack) itself, past the
 * catch above and the error handler, so a read error is logged masked here and
 * the response fails with a generic error: the client sees a reset, never a
 * truncated 200, and the raw error never reaches stderr.
 */
function guardStream(inner: ReadableStream<Uint8Array>): ReadableStream<Uint8Array> {
  const reader = inner.getReader();
  return new ReadableStream<Uint8Array>({
    async pull(controller) {
      let chunk: Awaited<ReturnType<typeof reader.read>>;
      try {
        chunk = await reader.read();
      } catch (err) {
        log.warn("attachment stream failed", { error: errorText(err) });
        controller.error(new Error("attachment stream failed"));
        reader.cancel().catch(() => {});
        return;
      }
      if (chunk.done) controller.close();
      else controller.enqueue(chunk.value);
    },
    cancel(reason) {
      return reader.cancel(reason).catch(() => {});
    },
  });
}

async function readJson(req: Request): Promise<unknown | Response> {
  try {
    return await req.json();
  } catch {
    return problem(400, "invalid_request", "body must be JSON");
  }
}

type Body = Record<string, unknown>;

function isObject(value: unknown): value is Body {
  return typeof value === "object" && value !== null && !Array.isArray(value);
}

function parseText(value: unknown, name: string): string | { error: string } {
  if (typeof value !== "string" || value.trim() === "") return { error: `${name} must be a non-empty string` };
  if (value.length > MAX_TEXT) return { error: `${name} is longer than ${MAX_TEXT} characters` };
  return value;
}

function parsePhones(value: unknown, name: string, min: number): string[] | { error: string } {
  if (!Array.isArray(value)) return { error: `${name} must be an array of E.164 phone numbers` };
  if (value.length < min || value.length > MAX_RECIPIENTS) {
    return { error: `${name} must hold ${min} to ${MAX_RECIPIENTS} phone numbers` };
  }
  for (const phone of value) {
    if (typeof phone !== "string" || !E164.test(phone)) return { error: `${name} must hold E.164 phone numbers like +19195550100` };
  }
  const unique = [...new Set(value as string[])];
  if (unique.length !== value.length) return { error: `${name} has duplicates` };
  return unique;
}

function parseUserIds(value: unknown, name: string): string[] | { error: string } {
  if (!Array.isArray(value) || value.length < 1 || value.length > MAX_RECIPIENTS) {
    return { error: `${name} must hold 1 to ${MAX_RECIPIENTS} Telegram user ids` };
  }
  const ids: string[] = [];
  for (const id of value) {
    // Telegram ids fit in 52 bits, so a JSON number is exact; strings are the documented form.
    const text = typeof id === "number" && Number.isSafeInteger(id) ? String(id) : id;
    if (typeof text !== "string" || !TELEGRAM_USER_ID.test(text)) return { error: `${name} must hold Telegram user ids like "123456789"` };
    ids.push(text);
  }
  if (new Set(ids).size !== ids.length) return { error: `${name} has duplicates` };
  return ids;
}

function parseButtons(value: unknown): Button[] | { error: string } {
  if (!Array.isArray(value) || value.length < 1 || value.length > MAX_BUTTONS) {
    return { error: `buttons must hold 1 to ${MAX_BUTTONS} buttons` };
  }
  const buttons: Button[] = [];
  for (const raw of value) {
    if (!isObject(raw)) return { error: "each button must be an object" };
    const { label, url, callback_data } = raw;
    if (typeof label !== "string" || label.trim() === "" || label.length > MAX_BUTTON_LABEL) {
      return { error: `each button needs a label of 1 to ${MAX_BUTTON_LABEL} characters` };
    }
    const hasUrl = url !== undefined && url !== null;
    const hasData = callback_data !== undefined && callback_data !== null;
    if (hasUrl === hasData) return { error: "each button needs exactly one of url or callback_data" };
    if (hasUrl) {
      if (typeof url !== "string" || url.length > MAX_URL || !BUTTON_URL.test(url)) {
        return { error: "a button url must be an http, https or tg URL" };
      }
      buttons.push({ label, url });
    } else {
      const bytes = typeof callback_data === "string" ? new TextEncoder().encode(callback_data).length : 0;
      if (typeof callback_data !== "string" || bytes < 1 || bytes > MAX_CALLBACK_DATA_BYTES) {
        return { error: `a button's callback_data must be 1 to ${MAX_CALLBACK_DATA_BYTES} bytes` };
      }
      buttons.push({ label, callback_data });
    }
  }
  return buttons;
}

export function parseSend(body: unknown): SendRequest | string {
  if (!isObject(body)) return "body must be a JSON object";
  const text = parseText(body.text, "text");
  if (typeof text !== "string") return text.error;
  const present = (key: string) => body[key] !== undefined && body[key] !== null;
  const targets = ["space_id", "to_phones", "to_user_ids"].filter(present);
  if (targets.length !== 1) return "give exactly one of space_id, to_phones or to_user_ids";
  let buttons: Button[] | undefined;
  if (present("buttons")) {
    const parsed = parseButtons(body.buttons);
    if (!Array.isArray(parsed)) return parsed.error;
    buttons = parsed;
  }
  const withButtons = buttons ? { text, buttons } : { text };
  if (present("space_id")) {
    if (typeof body.space_id !== "string" || body.space_id.trim() === "" || body.space_id.length > 512) {
      return "space_id must be a non-empty string";
    }
    return { space_id: body.space_id, ...withButtons };
  }
  if (present("to_user_ids")) {
    const ids = parseUserIds(body.to_user_ids, "to_user_ids");
    if (!Array.isArray(ids)) return ids.error;
    return { to_user_ids: ids, ...withButtons };
  }
  const phones = parsePhones(body.to_phones, "to_phones", 1);
  if (!Array.isArray(phones)) return phones.error;
  return { to_phones: phones, ...withButtons };
}

export function parseGroup(body: unknown): GroupRequest | string {
  if (!isObject(body)) return "body must be a JSON object";
  const phones = parsePhones(body.phones, "phones", 2);
  if (!Array.isArray(phones)) return phones.error;
  if (typeof body.name !== "string" || body.name.trim() === "" || body.name.length > 100) {
    return "name must be a non-empty string of at most 100 characters";
  }
  const intro = parseText(body.intro_text, "intro_text");
  if (typeof intro !== "string") return intro.error;
  return { phones, name: body.name, intro_text: intro };
}

export function parseInviteLink(body: unknown): InviteLinkRequest | string {
  if (!isObject(body)) return "body must be a JSON object";
  if (typeof body.space_id !== "string" || body.space_id.trim() === "" || body.space_id.length > 512) {
    return "space_id must be a non-empty string";
  }
  if (body.name === undefined || body.name === null) return { space_id: body.space_id };
  if (typeof body.name !== "string" || body.name.trim() === "" || body.name.length > MAX_INVITE_NAME) {
    return `name must be a non-empty string of at most ${MAX_INVITE_NAME} characters`;
  }
  return { space_id: body.space_id, name: body.name };
}

function json(status: number, body: unknown): Response {
  return Response.json(body, { status });
}

function problem(status: number, code: string, message: string, details?: unknown): Response {
  return json(status, { error: details === undefined ? { code, message } : { code, message, details } });
}

function methodNotAllowed(allow: string): Response {
  const res = problem(405, "method_not_allowed", `use ${allow}`);
  res.headers.set("allow", allow);
  return res;
}
