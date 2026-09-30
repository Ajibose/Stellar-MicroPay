/**
 * src/utils/logger.js
 * Minimal structured logger.
 *
 * Deliberately dependency-free — the services that use it (networkService,
 * priceAlertsService, analyticsService) call it with the pino-style signatures
 * `logger.info("msg")` and `logger.info(obj, "msg")`, so we accept both.
 */

"use strict";

const LEVELS = { debug: 10, info: 20, warn: 30, error: 40 };

const configured = String(process.env.LOG_LEVEL || "info").toLowerCase();
const threshold = LEVELS[configured] ?? LEVELS.info;

/** Serialise a context object, tolerating circular references. */
function formatContext(context) {
  const seen = new WeakSet();

  try {
    return JSON.stringify(context, (key, value) => {
      if (value instanceof Error) {
        return { name: value.name, message: value.message, stack: value.stack };
      }
      if (typeof value === "object" && value !== null) {
        if (seen.has(value)) return "[Circular]";
        seen.add(value);
      }
      return value;
    });
  } catch {
    return String(context);
  }
}

function emit(level, args) {
  if (LEVELS[level] < threshold) return;

  const rendered = args
    .map((arg) => (typeof arg === "string" ? arg : formatContext(arg)))
    .join(" ");

  const line = `[${new Date().toISOString()}] ${level.toUpperCase()}: ${rendered}`;

  if (level === "error" || level === "warn") {
    console.error(line);
  } else {
    console.log(line);
  }
}

module.exports = {
  debug: (...args) => emit("debug", args),
  info: (...args) => emit("info", args),
  warn: (...args) => emit("warn", args),
  error: (...args) => emit("error", args),
};
