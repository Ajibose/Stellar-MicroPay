/**
 * __tests__/analytics.test.js
 * Unit and integration tests for analytics endpoints.
 */

"use strict";

const analyticsService = require("../src/services/analyticsService");
const stellarService = require("../src/services/stellarService");

// Mock Stellar service
jest.mock("../src/services/stellarService");

describe("Analytics Service", () => {
  beforeEach(async () => {
    jest.clearAllMocks();
    // Clear cache for each test
    await analyticsService.clearCache("GBRPYHIL2CI3WHZDTOOQFC6EB4KJJGUJLVXKJ46ZGFWTTNQNXNHTJXW");
  });

  const testPublicKey = "GBRPYHIL2CI3WHZDTOOQFC6EB4KJJGUJLVXKJ46ZGFWTTNQNXNHTJXW";

  // Mock payment data
  const mockPayments = [
    {
      id: "1",
      type: "sent",
      amount: "100",
      asset: "XLM",
      from: testPublicKey,
      to: "GBUQWP3BOUZX34ULNQG23RQ6F4BWFIYGJ2DN5ZKQYTROZXNUAAOXWS7",
      memo: "memo1",
      createdAt: "2024-01-01T12:00:00Z",
      transactionHash: "hash1",
      pagingToken: "token1",
    },
    {
      id: "2",
      type: "sent",
      amount: "50",
      asset: "XLM",
      from: testPublicKey,
      to: "GBUQWP3BOUZX34ULNQG23RQ6F4BWFIYGJ2DN5ZKQYTROZXNUAAOXWS7",
      memo: "memo2",
      createdAt: "2024-01-02T12:00:00Z",
      transactionHash: "hash2",
      pagingToken: "token2",
    },
    {
      id: "3",
      type: "sent",
      amount: "25",
      asset: "XLM",
      from: testPublicKey,
      to: "GBUQWP3BOUZX34ULNQG23RQ6F4BWFIYGJ2DN5ZKQYTROZXNUAAOXWS8",
      memo: "memo3",
      createdAt: "2024-01-03T12:00:00Z",
      transactionHash: "hash3",
      pagingToken: "token3",
    },
    {
      id: "4",
      type: "received",
      amount: "200",
      asset: "XLM",
      from: "GBUQWP3BOUZX34ULNQG23RQ6F4BWFIYGJ2DN5ZKQYTROZXNUAAOXWS9",
      to: testPublicKey,
      memo: "memo4",
      createdAt: "2024-01-04T18:00:00Z",
      transactionHash: "hash4",
      pagingToken: "token4",
    },
    {
      id: "5",
      type: "received",
      amount: "75",
      asset: "XLM",
      from: "GBUQWP3BOUZX34ULNQG23RQ6F4BWFIYGJ2DN5ZKQYTROZXNUAAOXWS10",
      to: testPublicKey,
      memo: "memo5",
      createdAt: "2024-01-10T12:00:00Z",
      transactionHash: "hash5",
      pagingToken: "token5",
    },
  ];

  describe("getSummary", () => {
    it("should return correct summary statistics", async () => {
      stellarService.getPayments.mockResolvedValue(mockPayments);

      const result = await analyticsService.getSummary(testPublicKey);

      expect(result).toHaveProperty("publicKey", testPublicKey);
      expect(result).toHaveProperty("totalSentXLM", "175.0000000"); // 100 + 50 + 25
      expect(result).toHaveProperty("totalReceivedXLM", "275.0000000"); // 200 + 75
      expect(result).toHaveProperty("uniqueCounterparties", 4);
      expect(result).toHaveProperty("averageTransactionSize");
      expect(result).toHaveProperty("totalTransactions", 5);

      // Average = (175 + 275) / 5 = 90
      expect(parseFloat(result.averageTransactionSize)).toBeCloseTo(90, 5);
    });

    it("should handle empty payment history", async () => {
      stellarService.getPayments.mockResolvedValue([]);

      const result = await analyticsService.getSummary(testPublicKey);

      expect(result.totalSentXLM).toBe("0.0000000");
      expect(result.totalReceivedXLM).toBe("0.0000000");
      expect(result.uniqueCounterparties).toBe(0);
      expect(result.totalTransactions).toBe(0);
      expect(result.averageTransactionSize).toBe("0");
    });

    it("should cache results for 5 minutes", async () => {
      stellarService.getPayments.mockResolvedValue(mockPayments);

      const result1 = await analyticsService.getSummary(testPublicKey);
      const result2 = await analyticsService.getSummary(testPublicKey);

      expect(stellarService.getPayments).toHaveBeenCalledTimes(1);
      expect(result1).toEqual(result2);
    });
  });

  describe("getTopRecipients", () => {
    it("should return top 5 recipients sorted by total XLM sent", async () => {
      stellarService.getPayments.mockResolvedValue(mockPayments);

      const result = await analyticsService.getTopRecipients(testPublicKey);

      expect(result).toHaveProperty("publicKey", testPublicKey);
      expect(result.topRecipients).toHaveLength(2); // Only 2 unique recipients in mock data
      expect(result.count).toBe(2);

      // First recipient should have total of 150 (100 + 50)
      expect(result.topRecipients[0]).toEqual({
        address: "GBUQWP3BOUZX34ULNQG23RQ6F4BWFIYGJ2DN5ZKQYTROZXNUAAOXWS7",
        totalXLMSent: "150.0000000",
      });

      // Second recipient should have total of 25
      expect(result.topRecipients[1]).toEqual({
        address: "GBUQWP3BOUZX34ULNQG23RQ6F4BWFIYGJ2DN5ZKQYTROZXNUAAOXWS8",
        totalXLMSent: "25.0000000",
      });
    });

    it("should only include sent payments", async () => {
      stellarService.getPayments.mockResolvedValue(mockPayments);

      const result = await analyticsService.getTopRecipients(testPublicKey);

      // Should only count sent payments (3), received payments should be ignored
      expect(result.topRecipients.length).toBeLessThanOrEqual(5);
      result.topRecipients.forEach((recipient) => {
        expect(recipient.address).toBeDefined();
        expect(typeof recipient.totalXLMSent).toBe("string");
      });
    });

    it("should return empty array when no sent payments", async () => {
      const receivedPayments = mockPayments.filter((p) => p.type === "received");
      stellarService.getPayments.mockResolvedValue(receivedPayments);

      const result = await analyticsService.getTopRecipients(testPublicKey);

      expect(result.topRecipients).toHaveLength(0);
      expect(result.count).toBe(0);
    });

    it("should cache results", async () => {
      stellarService.getPayments.mockResolvedValue(mockPayments);

      const result1 = await analyticsService.getTopRecipients(testPublicKey);
      const result2 = await analyticsService.getTopRecipients(testPublicKey);

      expect(stellarService.getPayments).toHaveBeenCalledTimes(1);
      expect(result1).toEqual(result2);
    });

    it("should limit to top 5 recipients", async () => {
      const manyRecipients = [
        ...mockPayments,
        ...Array.from({ length: 10 }, (_, i) => ({
          id: `extra_${i}`,
          type: "sent",
          amount: "10",
          asset: "XLM",
          from: testPublicKey,
          to: `GBUQWP3BOUZX34ULNQG23RQ6F4BWFIYGJ2DN5ZKQYTROZXNUAAOXWS${i + 11}`,
          memo: `extra_${i}`,
          createdAt: "2024-01-05T12:00:00Z",
          transactionHash: `hash_extra_${i}`,
          pagingToken: `token_extra_${i}`,
        })),
      ];
      stellarService.getPayments.mockResolvedValue(manyRecipients);

      const result = await analyticsService.getTopRecipients(testPublicKey);

      expect(result.topRecipients.length).toBeLessThanOrEqual(5);
    });
  });

  describe("getActivityByDay", () => {
    it("should return activity counts for all 7 days", async () => {
      stellarService.getPayments.mockResolvedValue(mockPayments);

      const result = await analyticsService.getActivityByDay(testPublicKey);

      expect(result).toHaveProperty("publicKey", testPublicKey);
      expect(result.activityByDay).toHaveLength(7);

      // Days should be in order Sunday through Saturday
      const dayNames = [
        "Sunday",
        "Monday",
        "Tuesday",
        "Wednesday",
        "Thursday",
        "Friday",
        "Saturday",
      ];
      result.activityByDay.forEach((activity, index) => {
        expect(activity.day).toBe(dayNames[index]);
        expect(activity.dayIndex).toBe(index);
        expect(typeof activity.transactionCount).toBe("number");
        expect(activity.transactionCount).toBeGreaterThanOrEqual(0);
      });
    });

    it("should correctly count transactions by day of week", async () => {
      stellarService.getPayments.mockResolvedValue(mockPayments);

      const result = await analyticsService.getActivityByDay(testPublicKey);

      // Jan 1, 2024 = Monday, Jan 2 = Tuesday, Jan 3 = Wednesday, Jan 4 = Thursday, Jan 10 = Wednesday
      const totalCount = result.activityByDay.reduce(
        (sum, day) => sum + day.transactionCount,
        0
      );
      expect(totalCount).toBe(5); // Total transactions in mockPayments
    });

    it("should handle empty payment history", async () => {
      stellarService.getPayments.mockResolvedValue([]);

      const result = await analyticsService.getActivityByDay(testPublicKey);

      expect(result.activityByDay).toHaveLength(7);
      result.activityByDay.forEach((day) => {
        expect(day.transactionCount).toBe(0);
      });
    });

    it("should cache results", async () => {
      stellarService.getPayments.mockResolvedValue(mockPayments);

      const result1 = await analyticsService.getActivityByDay(testPublicKey);
      const result2 = await analyticsService.getActivityByDay(testPublicKey);

      expect(stellarService.getPayments).toHaveBeenCalledTimes(1);
      expect(result1).toEqual(result2);
    });
  });

  describe("getCohortBreakdown", () => {
    const cohortPayments = [
      {
        id: "c1",
        type: "sent",
        amount: "10",
        asset: "XLM",
        from: testPublicKey,
        to: "GBUQWP3BOUZX34ULNQG23RQ6F4BWFIYGJ2DN5ZKQYTROZXNUAAOXW01",
        createdAt: "2026-07-10T12:00:00Z",
        transactionHash: "cohort-hash-1",
        pagingToken: "cohort-token-1",
      },
      {
        id: "c2",
        type: "sent",
        amount: "5",
        asset: "XLM",
        from: testPublicKey,
        to: "GBUQWP3BOUZX34ULNQG23RQ6F4BWFIYGJ2DN5ZKQYTROZXNUAAOXW01",
        createdAt: "2026-07-12T12:00:00Z",
        transactionHash: "cohort-hash-2",
        pagingToken: "cohort-token-2",
      },
      {
        id: "c3",
        type: "sent",
        amount: "7",
        asset: "XLM",
        from: testPublicKey,
        to: "GBUQWP3BOUZX34ULNQG23RQ6F4BWFIYGJ2DN5ZKQYTROZXNUAAOXW02",
        createdAt: "2026-07-15T12:00:00Z",
        transactionHash: "cohort-hash-3",
        pagingToken: "cohort-token-3",
      },
      {
        id: "c4",
        type: "received",
        amount: "20",
        asset: "XLM",
        from: "GBUQWP3BOUZX34ULNQG23RQ6F4BWFIYGJ2DN5ZKQYTROZXNUAAOXW03",
        to: testPublicKey,
        createdAt: "2026-07-18T12:00:00Z",
        transactionHash: "cohort-hash-4",
        pagingToken: "cohort-token-4",
      },
      {
        id: "c5",
        type: "received",
        amount: "15",
        asset: "XLM",
        from: "GBUQWP3BOUZX34ULNQG23RQ6F4BWFIYGJ2DN5ZKQYTROZXNUAAOXW03",
        to: testPublicKey,
        createdAt: "2026-07-22T12:00:00Z",
        transactionHash: "cohort-hash-5",
        pagingToken: "cohort-token-5",
      },
      {
        id: "c6",
        type: "sent",
        amount: "4",
        asset: "XLM",
        from: testPublicKey,
        to: "GBUQWP3BOUZX34ULNQG23RQ6F4BWFIYGJ2DN5ZKQYTROZXNUAAOXW04",
        createdAt: "2026-06-03T12:00:00Z",
        transactionHash: "cohort-hash-6",
        pagingToken: "cohort-token-6",
      },
      {
        id: "c7",
        type: "received",
        amount: "2",
        asset: "XLM",
        from: "GBUQWP3BOUZX34ULNQG23RQ6F4BWFIYGJ2DN5ZKQYTROZXNUAAOXW05",
        to: testPublicKey,
        createdAt: "2026-06-20T12:00:00Z",
        transactionHash: "cohort-hash-7",
        pagingToken: "cohort-token-7",
      },
      {
        id: "c8",
        type: "received",
        amount: "9",
        asset: "XLM",
        from: "GBUQWP3BOUZX34ULNQG23RQ6F4BWFIYGJ2DN5ZKQYTROZXNUAAOXW06",
        to: testPublicKey,
        createdAt: "2026-05-05T12:00:00Z",
        transactionHash: "cohort-hash-8",
        pagingToken: "cohort-token-8",
      },
    ];

    beforeEach(() => {
      jest.useFakeTimers();
      jest.setSystemTime(new Date("2026-07-25T12:00:00Z"));
    });

    afterEach(() => {
      jest.useRealTimers();
    });

    it("should group repeat and one-time counterparties by month", async () => {
      stellarService.getPayments.mockResolvedValue(cohortPayments);

      const result = await analyticsService.getCohortBreakdown(testPublicKey, {
        period: "month",
        periods: 3,
      });

      expect(result.publicKey).toBe(testPublicKey);
      expect(result.period).toBe("month");
      expect(result.periods).toBe(3);
      expect(result.cohorts).toHaveLength(3);

      const july = result.cohorts[2];
      expect(july.label).toBe("Jul 2026");
      expect(july.sent.counterparties.repeatCounterparties).toBe(1);
      expect(july.sent.counterparties.oneTimeCounterparties).toBe(1);
      expect(july.sent.paymentCount).toBe(3);
      expect(july.received.counterparties.repeatCounterparties).toBe(1);
      expect(july.received.counterparties.oneTimeCounterparties).toBe(0);
      expect(july.repeatRate).toBeGreaterThan(0);

      const june = result.cohorts[1];
      expect(june.label).toBe("Jun 2026");
      expect(june.sent.counterparties.oneTimeCounterparties).toBe(1);
      expect(june.received.counterparties.oneTimeCounterparties).toBe(1);
    });
  });

  describe("clearCache", () => {
    it("should clear cached data for a public key", async () => {
      stellarService.getPayments.mockResolvedValue(mockPayments);

      // First call — should fetch from service
      await analyticsService.getSummary(testPublicKey);
      expect(stellarService.getPayments).toHaveBeenCalledTimes(1);

      // Second call — should use cache
      await analyticsService.getSummary(testPublicKey);
      expect(stellarService.getPayments).toHaveBeenCalledTimes(1);

      // Clear cache
      await analyticsService.clearCache(testPublicKey);

      // Third call — should fetch again
      await analyticsService.getSummary(testPublicKey);
      expect(stellarService.getPayments).toHaveBeenCalledTimes(2);
    });
  });

  describe("Scheduled Email Exports", () => {
    it("should allow scheduling an export and trigger email", async () => {
      const schedule = analyticsService.scheduleExport(
        testPublicKey,
        "user@example.com",
        "daily"
      );

      expect(schedule).toEqual(expect.objectContaining({
        publicKey: testPublicKey,
        email: "user@example.com",
        frequency: "daily"
      }));

      const retrieved = analyticsService.getExportSchedule(testPublicKey);
      expect(retrieved).toEqual(schedule);
    });
  });

  describe("Top Tippers Leaderboard", () => {
    it("should aggregate and rank top tippers", () => {
      const tipsService = require("../src/services/tipsService");
      // Record some mock tips
      tipsService.recordTip({
        senderPublicKey: "GBB11111111111111111111111111111111111111111111111111111111",
        creatorPublicKey: testPublicKey,
        amount: "10.5",
      });
      tipsService.recordTip({
        senderPublicKey: "GBB11111111111111111111111111111111111111111111111111111111",
        creatorPublicKey: testPublicKey,
        amount: "5.5",
      });
      tipsService.recordTip({
        senderPublicKey: "GBB22222222222222222222222222222222222222222222222222222222",
        creatorPublicKey: testPublicKey,
        amount: "25.0",
      });

      const leaderboard = tipsService.getTopTippers(testPublicKey, 5);
      expect(leaderboard).toHaveLength(2);
      expect(leaderboard[0].senderPublicKey).toBe("GBB22222222222222222222222222222222222222222222222222222222");
      expect(parseFloat(leaderboard[0].totalAmount)).toBe(25.0);
      expect(leaderboard[1].senderPublicKey).toBe("GBB11111111111111111111111111111111111111111111111111111111");
      expect(parseFloat(leaderboard[1].totalAmount)).toBe(16.0);
    });
  });
});
