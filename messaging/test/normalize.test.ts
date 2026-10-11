import { describe, expect, test } from "bun:test";
import { normalizeContent, ownHandleFromLine, parseMapUrl } from "../src/normalize";

const stream = async () => new Response("bytes").body as ReadableStream<Uint8Array>;

describe("normalizeContent", () => {
  test("text", () => {
    expect(normalizeContent({ type: "text", text: "heading to Cosmic Cantina at 6" })).toEqual({
      text: "heading to Cosmic Cantina at 6",
      attachments: [],
    });
  });

  test("an attachment keeps its MIME type and a stream", async () => {
    const out = normalizeContent({ type: "attachment", id: "g1", name: "receipt.jpg", mimeType: "image/jpeg", read: stream, stream });
    expect(out?.text).toBe("");
    expect(out?.attachments).toHaveLength(1);
    expect(out?.attachments[0]?.mimeType).toBe("image/jpeg");
    expect(await new Response(await out?.attachments[0]?.stream()).text()).toBe("bytes");
  });

  test("a voice note counts as an attachment", () => {
    const out = normalizeContent({ type: "voice", mimeType: "audio/x-caf", read: stream, stream });
    expect(out?.attachments.map((a) => a.mimeType)).toEqual(["audio/x-caf"]);
  });

  test("a group (photo with caption) keeps the text and the attachment together", () => {
    const out = normalizeContent({
      type: "group",
      items: [
        { id: "p:0/m", content: { type: "attachment", id: "a", name: "a.png", mimeType: "image/png", read: stream, stream } },
        { id: "p:1/m", content: { type: "text", text: "my receipt" } },
      ],
    });
    expect(out?.text).toBe("my receipt");
    expect(out?.attachments).toHaveLength(1);
  });

  test("a threaded reply is unwrapped", () => {
    const out = normalizeContent({ type: "reply", content: { type: "text", text: "add guac" }, target: { id: "x" } });
    expect(out?.text).toBe("add guac");
  });

  test("a shared location card gives coordinates", () => {
    const out = normalizeContent({
      type: "contact",
      name: { formatted: "Current Location" },
      urls: ["http://maps.apple.com/?ll=35.913211\\,-79.055094&q=35.913211\\,-79.055094"],
    });
    expect(out).toEqual({ text: "", attachments: [], location: { lat: 35.913211, lng: -79.055094 } });
  });

  test("a map link pasted as text gives coordinates too", () => {
    const out = normalizeContent({ type: "text", text: "here https://maps.apple.com/?ll=35.909152,-79.046905" });
    expect(out?.location).toEqual({ lat: 35.909152, lng: -79.046905 });
  });

  test("a plain contact card, reactions and receipts carry nothing for core", () => {
    expect(normalizeContent({ type: "contact", name: { formatted: "Maya" }, phones: [{ value: "+19195550101" }] })).toBeUndefined();
    expect(normalizeContent({ type: "reaction", emoji: "❤️", target: {} })).toBeUndefined();
    expect(normalizeContent({ type: "read", target: {} })).toBeUndefined();
    expect(normalizeContent({ type: "custom", raw: { imessage_type: "unsupported-message" } })).toBeUndefined();
    expect(normalizeContent({ type: "addMember", members: ["+19195550101"] })).toBeUndefined();
    expect(normalizeContent({ type: "text", text: "   " })).toBeUndefined();
  });
});

describe("parseMapUrl", () => {
  test("Apple Maps ll, coordinate and q forms, and Google's @lat,lng", () => {
    expect(parseMapUrl("https://maps.apple.com/?ll=35.9,-79.0")).toEqual({ lat: 35.9, lng: -79.0 });
    expect(parseMapUrl("https://maps.apple.com/place?coordinate=35.9,-79.0")).toEqual({ lat: 35.9, lng: -79.0 });
    expect(parseMapUrl("https://maps.apple.com/?q=35.9,-79.0")).toEqual({ lat: 35.9, lng: -79.0 });
    expect(parseMapUrl("https://www.google.com/maps/@35.9,-79.0,17z")).toEqual({ lat: 35.9, lng: -79.0 });
  });

  test("rejects out-of-range or missing coordinates", () => {
    expect(parseMapUrl("https://maps.apple.com/?ll=135,-79")).toBeUndefined();
    expect(parseMapUrl("https://maps.apple.com/?q=Cosmic+Cantina")).toBeUndefined();
    expect(parseMapUrl("not a url")).toBeUndefined();
  });
});

describe("ownHandleFromLine", () => {
  test("a dedicated line's number counts; the shared pool's placeholder does not", () => {
    expect(ownHandleFromLine("+19195550199")).toBe("+19195550199");
    expect(ownHandleFromLine("shared")).toBeUndefined();
    expect(ownHandleFromLine("")).toBeUndefined();
    expect(ownHandleFromLine(undefined)).toBeUndefined();
  });
});
