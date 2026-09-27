/**
 * src/routes/payments.js
 * Payment history and logging endpoints.
 */

"use strict";

const express = require("express");
const router = express.Router();
const { strictLimiter } = require("../middleware/rateLimit");
const { sanitizePublicKey } = require("../middleware/sanitization");
const paymentController = require("../controllers/paymentController");
const streamController = require("../controllers/streamController");

/**
 * GET /api/payments/stream-status/:streamId
 * Read the current streaming-payment channel state from the Soroban contract (#1066).
 * Registered before /:publicKey so "stream-status" is not matched as a key.
 */
router.get("/stream-status/:streamId", strictLimiter, streamController.getStreamStatus);

/**
 * GET /api/payments/:publicKey
 * Fetch payment history for an account via Horizon.
 *
 * Query params:
 *   limit  — number of results (default: 20, max: 100)
 *   cursor — pagination cursor
 */
router.get("/:publicKey", strictLimiter, sanitizePublicKey, paymentController.getPayments);

/**
 * GET /api/payments/:publicKey/stats
 * Return aggregate stats for an account (total sent, received, count).
 */
router.get("/:publicKey/stats", paymentController.getStats);

module.exports = router;
