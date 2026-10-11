// Child process for console-mask.test.ts. It logs, through the Spectrum SDK's
// own logger (@photon-ai/otel, a dependency of spectrum-ts pinned by
// bun.lock), an Error whose message, field and cause hold a full phone
// number. With "raw" as its argument it skips the console mask, so the test
// can show what leaks without it.

if (process.argv[2] !== "raw") await import("../../src/install-console-mask");
const { createLogger, setLogLevel } = await import("@photon-ai/otel");

const phone = process.argv[3] ?? "+19195550123";
setLogLevel("warn");
const err = Object.assign(new Error(`chat any;-;${phone} not found`), {
  details: `no chat for ${phone}`,
  cause: new Error(`upstream rejected ${phone}`),
});
const logger = createLogger("spectrum.imessage");
logger.warn("failed to share contact card", { "spectrum.imessage.contact.chat": "any;-;+1•••0123" }, err);
logger.error("send failed", undefined, err);

export {};
