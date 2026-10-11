// One-line structured logs. Every string passes through maskText, so a phone
// or email that slips into a message or field is still masked.

import { maskText } from "./mask";

export type Level = "debug" | "info" | "warn" | "error";
export type Fields = Record<string, string | number | boolean | undefined>;
export type Sink = (level: Level, line: string) => void;

const ORDER: Record<Level, number> = { debug: 10, info: 20, warn: 30, error: 40 };

let minLevel: Level = "info";
let sink: Sink = (level, line) => {
  if (level === "error" || level === "warn") console.error(line);
  else console.log(line);
};

export function setLogLevel(level: Level): void {
  minLevel = level;
}

/** Replaces the output sink (tests use this to capture or silence logs). */
export function setLogSink(next: Sink): Sink {
  const previous = sink;
  sink = next;
  return previous;
}

function format(level: Level, msg: string, fields: Fields | undefined): string {
  const parts = [new Date().toISOString(), level.toUpperCase(), maskText(msg)];
  for (const [key, value] of Object.entries(fields ?? {})) {
    if (value === undefined) continue;
    const text = typeof value === "string" ? maskText(value) : String(value);
    parts.push(`${key}=${/\s/.test(text) ? JSON.stringify(text) : text}`);
  }
  return parts.join(" ");
}

function emit(level: Level, msg: string, fields?: Fields): void {
  if (ORDER[level] < ORDER[minLevel]) return;
  sink(level, format(level, msg, fields));
}

export const log = {
  debug: (msg: string, fields?: Fields) => emit("debug", msg, fields),
  info: (msg: string, fields?: Fields) => emit("info", msg, fields),
  warn: (msg: string, fields?: Fields) => emit("warn", msg, fields),
  error: (msg: string, fields?: Fields) => emit("error", msg, fields),
};

/** A short, masked description of an unknown thrown value. */
export function errorText(err: unknown): string {
  if (err instanceof Error) return maskText(`${err.name}: ${err.message}`).slice(0, 300);
  return maskText(String(err)).slice(0, 300);
}
