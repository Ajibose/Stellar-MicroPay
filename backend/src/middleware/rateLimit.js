/**
 * src/middleware/rateLimit.js
 * Dedicated rate limiters for different route sensitivity levels.
 */

"use strict";

const rateLimit = require("express-rate-limit");

/**
 * Strict rate limiting — 20 requests per minute.
 * Applied to sensitive lookups like accounts and payments.
 */
const strictLimiter = rateLimit({
  windowMs: 1 * 60 * 1000,
  max: 20,
  standardHeaders: true,
  legacyHeaders: false,
  message: { error: "Too many requests to sensitive routes, please wait 1 minute." },
});

function createAuthLimiter(max) {
  return rateLimit({
    windowMs: 1 * 60 * 1000,
    max,
    standardHeaders: true,
    legacyHeaders: false,
    handler: (req, res, next, options) => {
      const resetTime = req.rateLimit && req.rateLimit.resetTime;
      const retryAfter = resetTime
        ? Math.max(1, Math.ceil((resetTime.getTime() - Date.now()) / 1000))
        : Math.ceil(options.windowMs / 1000);

      res.setHeader("Retry-After", String(retryAfter));
      res.status(options.statusCode).json({
        error: "Too many authentication requests, please try again later.",
      });
    },
  });
}

const authChallengeLimiter = createAuthLimiter(10);
const authVerifyLimiter = createAuthLimiter(5);

module.exports = { strictLimiter, authChallengeLimiter, authVerifyLimiter };
