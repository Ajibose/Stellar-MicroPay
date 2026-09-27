import {
  parseStellarURI,
  uriToPrefillData,
  type ParsedStellarURI,
} from "../lib/sep0007";

const VALID_DESTINATION =
  "GBZXN7PIRZGNMHGA72BBHBFAQXKE3S2EXWMT3KPIS244U6MTK2CSRSU4";
const OTHER_VALID_DESTINATION =
  "GA5ZSEJYB37JRC5AVCIA5MOP4RHTM335X2KGX3IHOJAPP5RE34K4KZVN";

describe("sep0007 - uriToPrefillData", () => {
  describe("Required Scenarios", () => {
    it("handles valid web+stellar:pay?destination=G...&amount=10&memo=hello", () => {
      const uri = `web+stellar:pay?destination=${VALID_DESTINATION}&amount=10&memo=hello`;
      const result = uriToPrefillData(uri);

      expect(result).toEqual({
        destination: VALID_DESTINATION,
        amount: "10",
        memo: "hello",
      });
    });

    it("handles missing destination parameter", () => {
      const uri = "web+stellar:pay?amount=10&memo=hello";
      const result = uriToPrefillData(uri);

      expect(result).toBeNull();
    });

    it("handles invalid amount (non-numeric)", () => {
      const uri = `web+stellar:pay?destination=${VALID_DESTINATION}&amount=notanumber&memo=hello`;
      const result = uriToPrefillData(uri);

      expect(result).toBeNull();
    });

    it("ignores extra unknown parameters", () => {
      const uri = `web+stellar:pay?destination=${VALID_DESTINATION}&amount=10&memo=hello&extra_foo=bar&unknown_param=12345&source=partner`;
      const result = uriToPrefillData(uri);

      expect(result).toEqual({
        destination: VALID_DESTINATION,
        amount: "10",
        memo: "hello",
      });
    });

    it("handles web+stellar://pay?... double-slash variant", () => {
      const uri = `web+stellar://pay?destination=${VALID_DESTINATION}&amount=10&memo=hello`;
      const result = uriToPrefillData(uri);

      expect(result).toEqual({
        destination: VALID_DESTINATION,
        amount: "10",
        memo: "hello",
      });
    });
  });

  describe("ParsedStellarURI object input", () => {
    it("converts a full ParsedStellarURI object to prefill data", () => {
      const parsed: ParsedStellarURI = {
        destination: VALID_DESTINATION,
        amount: "25.5",
        memo: "payment-ref-123",
        assetCode: "XLM",
      };

      const result = uriToPrefillData(parsed);

      expect(result).toEqual({
        destination: VALID_DESTINATION,
        amount: "25.5",
        memo: "payment-ref-123",
      });
    });

    it("defaults missing optional amount and memo to empty strings", () => {
      const parsed: ParsedStellarURI = {
        destination: OTHER_VALID_DESTINATION,
      };

      const result = uriToPrefillData(parsed);

      expect(result).toEqual({
        destination: OTHER_VALID_DESTINATION,
        amount: "",
        memo: "",
      });
    });
  });

  describe("Edge Cases for URI input", () => {
    it("handles URI with destination only (amount and memo default to empty strings)", () => {
      const uri = `web+stellar:pay?destination=${VALID_DESTINATION}`;
      const result = uriToPrefillData(uri);

      expect(result).toEqual({
        destination: VALID_DESTINATION,
        amount: "",
        memo: "",
      });
    });

    it("returns null for non-positive amounts (e.g. 0 or negative)", () => {
      expect(
        uriToPrefillData(`web+stellar:pay?destination=${VALID_DESTINATION}&amount=0`)
      ).toBeNull();
      expect(
        uriToPrefillData(`web+stellar:pay?destination=${VALID_DESTINATION}&amount=-5`)
      ).toBeNull();
    });

    it("returns null for invalid destination format", () => {
      const uri = "web+stellar:pay?destination=INVALID_DESTINATION&amount=10";
      const result = uriToPrefillData(uri);

      expect(result).toBeNull();
    });

    it("returns null for malformed or unsupported URI schemes", () => {
      expect(
        uriToPrefillData(`https://example.com/pay?destination=${VALID_DESTINATION}`)
      ).toBeNull();
      expect(
        uriToPrefillData("not-a-valid-uri")
      ).toBeNull();
    });
  });
});

describe("sep0007 - parseStellarURI", () => {
  it("parses valid stellar:pay URI", () => {
    const uri = `stellar:pay?destination=${VALID_DESTINATION}&amount=15&memo=invoice`;
    const result = parseStellarURI(uri);

    expect(result.success).toBe(true);
    expect(result.isExternal).toBe(false);
    expect(result.data?.destination).toBe(VALID_DESTINATION);
    expect(result.data?.amount).toBe("15");
    expect(result.data?.memo).toBe("invoice");
  });

  it("parses double-slash stellar://pay URI", () => {
    const uri = `stellar://pay?destination=${VALID_DESTINATION}&amount=20`;
    const result = parseStellarURI(uri);

    expect(result.success).toBe(true);
    expect(result.isExternal).toBe(false);
    expect(result.data?.destination).toBe(VALID_DESTINATION);
    expect(result.data?.amount).toBe("20");
  });

  it("parses web+stellar:pay and sets isExternal to true", () => {
    const uri = `web+stellar:pay?destination=${VALID_DESTINATION}&amount=50`;
    const result = parseStellarURI(uri);

    expect(result.success).toBe(true);
    expect(result.isExternal).toBe(true);
    expect(result.data?.destination).toBe(VALID_DESTINATION);
    expect(result.data?.amount).toBe("50");
  });

  it("parses web+stellar://pay double-slash variant and sets isExternal to true", () => {
    const uri = `web+stellar://pay?destination=${VALID_DESTINATION}&amount=50`;
    const result = parseStellarURI(uri);

    expect(result.success).toBe(true);
    expect(result.isExternal).toBe(true);
    expect(result.data?.destination).toBe(VALID_DESTINATION);
    expect(result.data?.amount).toBe("50");
  });

  it("fails when destination is missing", () => {
    const uri = "web+stellar:pay?amount=10";
    const result = parseStellarURI(uri);

    expect(result.success).toBe(false);
    expect(result.error).toContain("Missing required parameter: destination");
  });

  it("fails when amount is non-numeric or non-positive", () => {
    expect(
      parseStellarURI(`web+stellar:pay?destination=${VALID_DESTINATION}&amount=xyz`).success
    ).toBe(false);
    expect(
      parseStellarURI(`web+stellar:pay?destination=${VALID_DESTINATION}&amount=-10`).success
    ).toBe(false);
    expect(
      parseStellarURI(`web+stellar:pay?destination=${VALID_DESTINATION}&amount=0`).success
    ).toBe(false);
  });

  it("validates asset_issuer requirement for non-XLM assets", () => {
    const withoutIssuer = `web+stellar:pay?destination=${VALID_DESTINATION}&asset_code=USDC`;
    expect(parseStellarURI(withoutIssuer).success).toBe(false);

    const withIssuer = `web+stellar:pay?destination=${VALID_DESTINATION}&asset_code=USDC&asset_issuer=${OTHER_VALID_DESTINATION}`;
    const result = parseStellarURI(withIssuer);
    expect(result.success).toBe(true);
    expect(result.data?.assetCode).toBe("USDC");
    expect(result.data?.assetIssuer).toBe(OTHER_VALID_DESTINATION);
  });

  it("validates memo_type", () => {
    const invalidMemoType = `web+stellar:pay?destination=${VALID_DESTINATION}&memo=test&memo_type=INVALID_TYPE`;
    expect(parseStellarURI(invalidMemoType).success).toBe(false);

    const validMemoType = `web+stellar:pay?destination=${VALID_DESTINATION}&memo=test&memo_type=MEMO_TEXT`;
    const result = parseStellarURI(validMemoType);
    expect(result.success).toBe(true);
    expect(result.data?.memoType).toBe("MEMO_TEXT");
  });
});
