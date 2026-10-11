// Masks phones and emails in everything printed through console. The
// Spectrum SDK's own logger (@photon-ai/otel) sanitizes its attributes but
// hands raw Error objects to console.warn and console.error, and their
// message, stack, fields and cause can hold a chat GUID or a handle.
// install-console-mask.ts installs this before spectrum-ts loads.

import { format } from "node:util";
import { maskText } from "./mask";

const METHODS = ["log", "info", "warn", "error", "debug"] as const;

export type ConsoleLike = Record<(typeof METHODS)[number], (...args: unknown[]) => void>;

/**
 * One masked line for a console call. format() renders strings, format
 * specifiers, objects and errors (message, stack, own fields, cause) the way
 * console would, and the whole line is masked at once.
 */
export function maskConsoleArgs(args: unknown[]): string {
  return maskText(args.length === 0 ? "" : format(args[0], ...args.slice(1)));
}

/** Masks console.log, info, warn, error and debug on target. Returns a function that restores them. */
export function installConsoleMask(target: ConsoleLike = console): () => void {
  const originals = METHODS.map((method) => [method, target[method]] as const);
  for (const [method, original] of originals) {
    target[method] = (...args: unknown[]) => original.call(target, maskConsoleArgs(args));
  }
  return () => {
    for (const [method, original] of originals) target[method] = original;
  };
}
