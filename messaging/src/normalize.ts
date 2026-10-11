// Reduces a Spectrum message's content to text, attachments and an optional
// location. Structural types only, so tests feed plain objects.
//
// Spectrum has no location content type. iOS shares a location as a
// `.loc.vcf` card whose URL is an Apple Maps link, and the SDK parses vCards
// into `contact` content, so the coordinates are read from `contact.urls`
// (and from map links pasted as text). See docs/DECISIONS.md, Photon Spectrum.

import type { InboundAttachment, LatLng } from "./types";

/**
 * The agent's own number from an iMessage space's `phone`. A dedicated line
 * puts its number there. On the shared pool spectrum-ts 12.10.1 puts the
 * placeholder "shared" there instead, because it cannot tell which pool
 * number serves a chat, so only an E.164 value counts.
 */
export function ownHandleFromLine(phone: string | undefined): string | undefined {
  return phone !== undefined && /^\+[1-9]\d{6,14}$/.test(phone) ? phone : undefined;
}

export interface ContentLike {
  readonly type: string;
  readonly [key: string]: unknown;
}

export interface Normalized {
  text: string;
  attachments: InboundAttachment[];
  location?: LatLng;
}

/** Content types that carry nothing for core: reactions, receipts, typing, membership and so on. */
export function normalizeContent(content: ContentLike): Normalized | undefined {
  const texts: string[] = [];
  const attachments: InboundAttachment[] = [];
  const urls: string[] = [];
  collect(content, texts, attachments, urls, 0);

  const text = texts.join("\n").trim();
  const location = firstLocation([...urls, text]);
  if (text === "" && attachments.length === 0 && location === undefined) return undefined;
  return location ? { text, attachments, location } : { text, attachments };
}

function collect(content: ContentLike, texts: string[], attachments: InboundAttachment[], urls: string[], depth: number): void {
  if (depth > 3) return;
  switch (content.type) {
    case "text":
      if (typeof content.text === "string") texts.push(content.text);
      return;
    case "markdown":
      if (typeof content.markdown === "string") texts.push(content.markdown);
      return;
    case "attachment":
    case "voice": {
      const stream = content.stream;
      if (typeof stream !== "function") return;
      attachments.push({
        mimeType: typeof content.mimeType === "string" ? content.mimeType : "application/octet-stream",
        name: typeof content.name === "string" ? content.name : "attachment",
        stream: () => (stream as () => Promise<ReadableStream<Uint8Array>>).call(content),
      });
      return;
    }
    case "contact":
      if (Array.isArray(content.urls)) for (const url of content.urls) if (typeof url === "string") urls.push(url);
      return;
    case "group":
      if (Array.isArray(content.items)) {
        for (const item of content.items) {
          const inner = (item as { content?: unknown } | null)?.content;
          if (isContent(inner)) collect(inner, texts, attachments, urls, depth + 1);
        }
      }
      return;
    case "reply":
      if (isContent(content.content)) collect(content.content, texts, attachments, urls, depth + 1);
      return;
    default:
      return;
  }
}

function isContent(value: unknown): value is ContentLike {
  return typeof value === "object" && value !== null && typeof (value as { type?: unknown }).type === "string";
}

const MAP_URL = /https?:\/\/(?:maps\.apple\.com|maps\.google\.com|(?:www\.)?google\.com\/maps)[^\s"'<>]*/gi;

function firstLocation(sources: string[]): LatLng | undefined {
  for (const source of sources) {
    for (const match of source.matchAll(MAP_URL)) {
      const location = parseMapUrl(match[0]);
      if (location) return location;
    }
  }
  return undefined;
}

/** Reads coordinates from an Apple or Google Maps URL (ll, coordinate, q, sll, or an @lat,lng path). */
export function parseMapUrl(raw: string): LatLng | undefined {
  let url: URL;
  try {
    url = new URL(raw.replace(/\\,/g, ",").replace(/\\:/g, ":"));
  } catch {
    return undefined;
  }
  for (const key of ["ll", "coordinate", "q", "sll", "query"]) {
    const location = parsePair(url.searchParams.get(key));
    if (location) return location;
  }
  const at = /@(-?\d+(?:\.\d+)?),(-?\d+(?:\.\d+)?)/.exec(url.pathname);
  return at ? parsePair(`${at[1]},${at[2]}`) : undefined;
}

function parsePair(value: string | null): LatLng | undefined {
  if (!value) return undefined;
  const match = /^\s*(-?\d+(?:\.\d+)?)\s*,\s*(-?\d+(?:\.\d+)?)\s*$/.exec(value);
  if (!match) return undefined;
  const lat = Number(match[1]);
  const lng = Number(match[2]);
  if (!Number.isFinite(lat) || !Number.isFinite(lng) || Math.abs(lat) > 90 || Math.abs(lng) > 180) return undefined;
  return { lat, lng };
}
