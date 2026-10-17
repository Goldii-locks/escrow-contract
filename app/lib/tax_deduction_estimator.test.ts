/**
 * @file tax_deduction_estimator.test.ts
 * @description Test suite for tax_deduction_estimator rate limiting and DB column precision formatting.
 * Tests issue #453 (429 warnings on rate limit threshold exceeded) and issue #452 (DB precision column formatting).
 */

import {
  TaxDeductionRateLimiter,
  calculateTaxDeduction,
  processTaxDeductionEstimator,
  formatStroopToDecimal,
} from "./tax_deduction_estimator";

function assert(condition: boolean, message: string) {
  if (!condition) {
    throw new Error(`Assertion Failed: ${message}`);
  }
}

function assertStrictEqual<T>(actual: T, expected: T, message: string) {
  if (actual !== expected) {
    throw new Error(
      `Assertion Failed: ${message}. Expected ${expected}, got ${actual}`
    );
  }
}

export function runTaxDeductionEstimatorTests() {
  console.log("Running tax_deduction_estimator test suite...");

  // ──────────────────────────────────────────────────────────────────────────
  // Issue #453 Tests: Rate Limiting Checks
  // ──────────────────────────────────────────────────────────────────────────

  // 1. Rate limiting returns 200 for initial allowed requests and 429 when threshold exceeded
  {
    const limiter = new TaxDeductionRateLimiter({ maxRequests: 3, windowMs: 60000 });
    const clientId = "client-ip-127.0.0.1";

    const req1 = processTaxDeductionEstimator(clientId, 10000000n, 500, limiter);
    assertStrictEqual(req1.status, 200, "Request 1 should return 200");
    assertStrictEqual(req1.success, true, "Request 1 should succeed");
    assertStrictEqual(req1.rateLimit.remaining, 2, "Request 1 remaining should be 2");

    const req2 = processTaxDeductionEstimator(clientId, 10000000n, 500, limiter);
    assertStrictEqual(req2.status, 200, "Request 2 should return 200");
    assertStrictEqual(req2.rateLimit.remaining, 1, "Request 2 remaining should be 1");

    const req3 = processTaxDeductionEstimator(clientId, 10000000n, 500, limiter);
    assertStrictEqual(req3.status, 200, "Request 3 should return 200");
    assertStrictEqual(req3.rateLimit.remaining, 0, "Request 3 remaining should be 0");

    // 4th request exceeds threshold limit -> must return 429 warning
    const req4 = processTaxDeductionEstimator(clientId, 10000000n, 500, limiter);
    assertStrictEqual(req4.status, 429, "Request 4 exceeding threshold should return 429");
    assertStrictEqual(req4.success, false, "Request 4 should fail");
    assert(
      req4.warning !== undefined && req4.warning.includes("429"),
      "429 warning should be returned"
    );
    assertStrictEqual(req4.rateLimit.remaining, 0, "Rate limit remaining should be 0 on 429");

    console.log("  ✅ Issue #453: Threshold limit 429 warning check passed");
  }

  // 2. Different clients have independent rate limit buckets
  {
    const limiter = new TaxDeductionRateLimiter({ maxRequests: 1, windowMs: 60000 });
    const clientA = "client-A";
    const clientB = "client-B";

    const resA1 = processTaxDeductionEstimator(clientA, 5000000n, 1000, limiter);
    assertStrictEqual(resA1.status, 200, "Client A request 1 should succeed");

    const resA2 = processTaxDeductionEstimator(clientA, 5000000n, 1000, limiter);
    assertStrictEqual(resA2.status, 429, "Client A request 2 should be rate limited with 429");

    // Client B should still be allowed
    const resB1 = processTaxDeductionEstimator(clientB, 5000000n, 1000, limiter);
    assertStrictEqual(resB1.status, 200, "Client B request 1 should succeed independently");

    console.log("  ✅ Issue #453: Independent client rate limit buckets passed");
  }

  // ──────────────────────────────────────────────────────────────────────────
  // Issue #452 Tests: Database Column Storage Precision Formatting
  // ──────────────────────────────────────────────────────────────────────────

  // 3. Format columns for DB storage preserve full precision
  {
    const gross = 12345678901234567890n; // Large 128-bit integer
    const bps = 2500; // 25.00% tax
    const result = calculateTaxDeduction(gross, bps);

    // Exact expected values
    const expectedTax = (gross * 2500n + 5000n) / 10000n;
    const expectedNet = gross - expectedTax;

    assertStrictEqual(
      result.calculation.grossAmount,
      gross,
      "Gross amount calculation precision"
    );
    assertStrictEqual(
      result.calculation.taxAmount,
      expectedTax,
      "Tax amount calculation precision"
    );
    assertStrictEqual(
      result.calculation.netAmount,
      expectedNet,
      "Net amount calculation precision"
    );

    // DB row attribute assertions preserving full precision strings
    assertStrictEqual(
      result.db_row.gross_amount,
      gross.toString(),
      "DB row gross_amount attribute preserves exact string precision"
    );
    assertStrictEqual(
      result.db_row.tax_amount,
      expectedTax.toString(),
      "DB row tax_amount attribute preserves exact string precision"
    );
    assertStrictEqual(
      result.db_row.net_amount,
      expectedNet.toString(),
      "DB row net_amount attribute preserves exact string precision"
    );

    assertStrictEqual(
      result.db_row.tax_rate_bps,
      2500,
      "DB row tax_rate_bps attribute preserved"
    );

    console.log("  ✅ Issue #452: DB column storage full precision preservation passed");
  }

  // 4. Decimal formatting helper for 7-decimal stroop precision schemas
  {
    assertStrictEqual(
      formatStroopToDecimal(10000000n),
      "1.0000000",
      "Format 10000000 stroops to 1.0000000"
    );
    assertStrictEqual(
      formatStroopToDecimal(1234567n),
      "0.1234567",
      "Format 1234567 stroops to 0.1234567"
    );
    assertStrictEqual(
      formatStroopToDecimal(50n),
      "0.0000050",
      "Format 50 stroops to 0.0000050"
    );

    console.log("  ✅ Issue #452: Stroop decimal column formatting passed");
  }

  console.log("All tax_deduction_estimator tests passed successfully!");
}

// Execute tests if invoked directly
runTaxDeductionEstimatorTests();
