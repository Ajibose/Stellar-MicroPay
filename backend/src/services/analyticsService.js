/**
 * src/services/analyticsService.js
 * Business logic for transaction volume analytics.
 * Fetches payment data from Horizon and computes aggregated insights.
 * Includes in-memory caching with 5-minute TTL plus a per-key response
 * cache with 1-hour TTL and periodic sweep (#1210).
 */

"use strict";

const stellarService = require("./stellarService");
const logger = require("../utils/logger");

// ─── Cache Configuration ──────────────────────────────────────────────────────

const CACHE_TTL = 5 * 60 * 1000; // 5 minutes in milliseconds
const CACHE_MAX_SIZE = Number.parseInt(process.env.ANALYTICS_CACHE_MAX_SIZE, 10) || 500;
const cache = new Map();

/**
 * LRU cache backed by a Map. JavaScript Maps preserve insertion order, so
 * re-inserting an entry (delete + set) moves it to the end of the iteration
 * order and the oldest entry can be evicted from the front.
 */
function setCacheEntry(key, data) {
  cache.delete(key);
  cache.set(key, { data, timestamp: Date.now() });

  while (cache.size > CACHE_MAX_SIZE) {
    const oldestKey = cache.keys().next().value;
    if (oldestKey === undefined) break;
    cache.delete(oldestKey);
  }
}

/**
 * Cache wrapper function.
 * @param {string} key
 * @param {Function} fn - async function that returns the data
 */
async function withCache(key, fn) {
  const cached = cache.get(key);

  if (cached && Date.now() - cached.timestamp < CACHE_TTL) {
    setCacheEntry(key, cached.data);
    return cached.data;
  }

  const data = await fn();
  setCacheEntry(key, data);
  return data;
}

// ─── Analytics Functions ──────────────────────────────────────────────────────

/**
 * Get summary analytics for a public key.
 * Returns: total sent, total received, unique counterparties, avg transaction size.
 */
async function getSummary(publicKey) {
  return withCache(`summary:${publicKey}`, async () => {
    const payments = await stellarService.getPayments(publicKey, { limit: 200 });

    let totalSent = 0;
    let totalReceived = 0;
    const counterparties = new Set();
    let transactionCount = 0;

    for (const payment of payments) {
      const amount = parseFloat(payment.amount);

      if (payment.type === "sent") {
        totalSent += amount;
        counterparties.add(payment.to);
      } else {
        totalReceived += amount;
        counterparties.add(payment.from);
      }
      transactionCount++;
    }

    const totalVolume = totalSent + totalReceived;
    const avgTransactionSize =
      transactionCount > 0 ? (totalVolume / transactionCount).toFixed(7) : "0";

    return {
      publicKey,
      totalSentXLM: totalSent.toFixed(7),
      totalReceivedXLM: totalReceived.toFixed(7),
      uniqueCounterparties: counterparties.size,
      averageTransactionSize: avgTransactionSize,
      totalTransactions: transactionCount,
    };
  });
}

/**
 * Get top 5 recipients by total XLM sent.
 */
async function getTopRecipients(publicKey) {
  return withCache(`top-recipients:${publicKey}`, async () => {
    const payments = await stellarService.getPayments(publicKey, { limit: 200 });
    const recipientTotals = new Map();

    for (const payment of payments) {
      if (payment.type === "sent") {
        const amount = parseFloat(payment.amount);
        const recipient = payment.to;

        if (recipientTotals.has(recipient)) {
          recipientTotals.set(recipient, recipientTotals.get(recipient) + amount);
        } else {
          recipientTotals.set(recipient, amount);
        }
      }
    }

    const sorted = Array.from(recipientTotals.entries())
      .map(([address, total]) => ({
        address,
        totalXLMSent: total.toFixed(7),
      }))
      .sort((a, b) => parseFloat(b.totalXLMSent) - parseFloat(a.totalXLMSent))
      .slice(0, 5);

    return {
      publicKey,
      topRecipients: sorted,
      count: sorted.length,
    };
  });
}

/**
 * Get payment activity by day of week.
 * Returns counts for all 7 days (Sunday = 0, ... Saturday = 6).
 */
async function getActivityByDay(publicKey) {
  return withCache(`activity:${publicKey}`, async () => {
    const payments = await stellarService.getPayments(publicKey, { limit: 200 });

    const dayActivity = {
      0: 0,
      1: 0,
      2: 0,
      3: 0,
      4: 0,
      5: 0,
      6: 0,
    };

    for (const payment of payments) {
      const date = new Date(payment.createdAt);
      const dayOfWeek = date.getUTCDay();
      dayActivity[dayOfWeek]++;
    }

    const days = ["Sunday", "Monday", "Tuesday", "Wednesday", "Thursday", "Friday", "Saturday"];
    const activity = days.map((dayName, index) => ({
      day: dayName,
      dayIndex: index,
      transactionCount: dayActivity[index],
    }));

    return {
      publicKey,
      activityByDay: activity,
    };
  });
}

/**
 * Clear cache for a specific public key.
 * @param {string} publicKey
 * @returns {number} Number of cache entries invalidated.
 */
function clearCache(publicKey) {
  cache.delete(`summary:${publicKey}`);
  cache.delete(`top-recipients:${publicKey}`);
  cache.delete(`activity:${publicKey}`);
}module.exports = {
  getSummary,
  getTopRecipients,
  getActivityByDay,
  clearCache,
  getCachedAnalytics,
  setCachedAnalytics,
  clearAnalyticsCache,
  stopCacheSweep,
};

// ─── Per-key analytics response cache with periodic sweep (#1210) ──────────

const CACHE_TTL_MS = 60 * 60 * 1000; // 1 hour
const SWEEP_INTERVAL_MS = 10 * 60 * 1000; // 10 minutes

// Map structure: key -> { data, timestamp }
const analyticsCache = new Map();

/**
 * Evict entries older than the TTL and log how many were removed.
 * @returns {number} Number of evicted entries.
 */
function sweepCache() {
  const now = Date.now();
  let evictedCount = 0;

  for (const [key, entry] of analyticsCache.entries()) {
    if (now - entry.timestamp > CACHE_TTL_MS) {
      analyticsCache.delete(key);
      evictedCount++;
    }
  }

  if (evictedCount > 0) {
    logger.info(`Cache sweep: evicted ${evictedCount} entries`);
  }

  return evictedCount;
}

// Start the periodic background sweep; unref'd so it never blocks exit.
const sweepIntervalId = setInterval(sweepCache, SWEEP_INTERVAL_MS);
if (sweepIntervalId.unref) {
  sweepIntervalId.unref();
}

/** Stop the periodic background sweep (used by tests). */
function stopCacheSweep() {
  clearInterval(sweepIntervalId);
}

function getCachedAnalytics(publicKey) {
  const entry = analyticsCache.get(publicKey);
  if (!entry) return null;

  if (Date.now() - entry.timestamp > CACHE_TTL_MS) {
    // Lazily sweep on access so stale entries are evicted and logged
    // even between background sweeps.
    sweepCache();
    return null;
  }

  return entry.data;
}

function setCachedAnalytics(publicKey, data) {
  analyticsCache.set(publicKey, {
    data,
    timestamp: Date.now(),
  });
}

function clearAnalyticsCache() {
  analyticsCache.clear();
}
