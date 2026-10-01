/**
 * utils/logger.ts
 * Minimal logger shared by frontend utilities.
 *
 * Console output is suppressed outside development so production bundles do
 * not leak diagnostic detail, while errors are always surfaced.
 */

type LogLevel = "debug" | "info" | "warn" | "error";

const isDevelopment = process.env.NODE_ENV === "development";

function emit(level: LogLevel, args: unknown[]): void {
  if (level === "error" || isDevelopment) {
    // eslint-disable-next-line no-console
    console[level](...args);
  }
}

export const logger = {
  debug: (...args: unknown[]): void => emit("debug", args),
  info: (...args: unknown[]): void => emit("info", args),
  warn: (...args: unknown[]): void => emit("warn", args),
  error: (...args: unknown[]): void => emit("error", args),
};

export default logger;