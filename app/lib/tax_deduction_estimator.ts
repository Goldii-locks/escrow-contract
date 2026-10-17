/**
 * @file tax_deduction_estimator.ts
 * @description Estimated withholding tax generator module with rate limiting checks and DB precision formatting.
 *
 * Implements:
 *  - Issue #453: Rate limiting checks on tax_deduction_estimator calls returning 429 warnings when thresholds are exceeded.
 *  - Issue #452: Formatting calculated values for DB storage, preserving full precision across database schema columns.
 */

export interface TaxDeductionCalculation {
  grossAmount: bigint;
  taxAmount: bigint;
  netAmount: bigint;
  taxRateBps: number;
}

export interface FormattedDbStorageColumns {
  gross_amount: string;
  tax_amount: string;
  net_amount: string;
  tax_rate_bps: number;
  gross_amount_stroop_decimal: string;
  tax_amount_stroop_decimal: string;
  net_amount_stroop_decimal: string;
  created_at_iso: string;
}

export interface TaxDeductionResult {
  calculation: TaxDeductionCalculation;
  db_row: FormattedDbStorageColumns;
}

export interface RateLimitConfig {
  maxRequests: number;
  windowMs: number;
}

export interface RateLimitCheckResult {
  allowed: boolean;
  statusCode: number;
  remaining: number;
  resetMs: number;
  warning?: string;
}

export interface TaxDeductionEstimatorResponse {
  status: number;
  success: boolean;
  data?: TaxDeductionResult;
  error?: string;
  warning?: string;
  rateLimit: {
    remaining: number;
    resetMs: number;
  };
}

/**
 * Rates limiter for tax deduction estimation endpoints using timestamp sliding-window tracking.
 * Time Complexity: O(N) where N is requests in current window per client.
 * Space Complexity: O(M) where M is active client entries.
 */
export class TaxDeductionRateLimiter {
  private requests: Map<string, number[]> = new Map();
  private maxRequests: number;
  private windowMs: number;

  constructor(config: RateLimitConfig = { maxRequests: 5, windowMs: 60000 }) {
    this.maxRequests = config.maxRequests;
    this.windowMs = config.windowMs;
  }

  /**
   * Resets rate limiter memory (useful for testing).
   */
  public reset(): void {
    this.requests.clear();
  }

  /**
   * Evaluates if a request from a specific client IP / identifier exceeds the rate limit threshold.
   * Returns 429 warning metadata when limit is exceeded.
   */
  public check(clientId: string): RateLimitCheckResult {
    const now = Date.now();
    const timestamps = this.requests.get(clientId) || [];

    // Evict timestamps outside current sliding window
    const validTimestamps = timestamps.filter(
      (ts) => now - ts < this.windowMs
    );

    if (validTimestamps.length >= this.maxRequests) {
      const oldestTs = validTimestamps[0];
      const resetMs = Math.max(0, this.windowMs - (now - oldestTs));
      return {
        allowed: false,
        statusCode: 429,
        remaining: 0,
        resetMs,
        warning: `Rate limit exceeded: maximum ${this.maxRequests} requests per ${this.windowMs / 1000}s allowed for tax_deduction_estimator. (HTTP 429 Too Many Requests)`,
      };
    }

    validTimestamps.push(now);
    this.requests.set(clientId, validTimestamps);

    return {
      allowed: true,
      statusCode: 200,
      remaining: this.maxRequests - validTimestamps.length,
      resetMs: this.windowMs,
    };
  }
}

/** Default singleton instance of rate limiter */
export const defaultRateLimiter = new TaxDeductionRateLimiter({
  maxRequests: 5,
  windowMs: 60000,
});

/**
 * Formats a BigInt raw stroop amount into exact decimal representation without precision loss.
 * Example: 10000000n -> "1.0000000" (7 decimals for Stellar stroop precision)
 */
export function formatStroopToDecimal(
  amount: bigint,
  decimals: number = 7
): string {
  const isNegative = amount < 0n;
  const absAmount = isNegative ? -amount : amount;
  const divisor = 10n ** BigInt(decimals);

  const integerPart = (absAmount / divisor).toString();
  const fractionalPart = (absAmount % divisor)
    .toString()
    .padStart(decimals, "0");

  const sign = isNegative ? "-" : "";
  return `${sign}${integerPart}.${fractionalPart}`;
}

/**
 * Calculates estimated tax withholding deduction amounts with round-nearest arithmetic.
 * Formats DB storage columns to match precision schemas without float precision degradation.
 *
 * Time Complexity: O(1)
 * Space Complexity: O(1)
 */
export function calculateTaxDeduction(
  grossAmountInput: bigint | string | number,
  taxRateBps: number
): TaxDeductionResult {
  if (taxRateBps < 0 || taxRateBps > 10000) {
    throw new Error("Invalid tax_rate_bps: must be between 0 and 10000");
  }

  const grossAmount = BigInt(grossAmountInput);
  if (grossAmount <= 0n) {
    throw new Error("Invalid gross_amount: must be positive");
  }

  // tax_amount = round_nearest(gross * taxRateBps / 10000)
  // integer round-nearest: (gross * bps + 5000) / 10000
  const bpsScale = 10000n;
  const scaled = grossAmount * BigInt(taxRateBps);
  const taxAmount = (scaled + bpsScale / 2n) / bpsScale;
  const netAmount = grossAmount - taxAmount;

  const calculation: TaxDeductionCalculation = {
    grossAmount,
    taxAmount,
    netAmount,
    taxRateBps,
  };

  // Formats columns for DB storage preserving full string precision
  const db_row: FormattedDbStorageColumns = {
    gross_amount: grossAmount.toString(),
    tax_amount: taxAmount.toString(),
    net_amount: netAmount.toString(),
    tax_rate_bps: taxRateBps,
    gross_amount_stroop_decimal: formatStroopToDecimal(grossAmount),
    tax_amount_stroop_decimal: formatStroopToDecimal(taxAmount),
    net_amount_stroop_decimal: formatStroopToDecimal(netAmount),
    created_at_iso: new Date().toISOString(),
  };

  return { calculation, db_row };
}

/**
 * Main entry point for tax_deduction_estimator with path rate limiting and DB column formatting.
 * Returns 429 warning if request rate exceeds threshold.
 *
 * @param clientId Client IP or identifier for rate limiting
 * @param grossAmount Gross amount to withhold tax from
 * @param taxRateBps Tax rate in basis points (0-10000)
 * @param rateLimiter Optional custom rate limiter instance
 */
export function processTaxDeductionEstimator(
  clientId: string,
  grossAmount: bigint | string | number,
  taxRateBps: number,
  rateLimiter: TaxDeductionRateLimiter = defaultRateLimiter
): TaxDeductionEstimatorResponse {
  const rateLimitResult = rateLimiter.check(clientId);

  if (!rateLimitResult.allowed) {
    return {
      status: 429,
      success: false,
      error: rateLimitResult.warning,
      warning: rateLimitResult.warning,
      rateLimit: {
        remaining: 0,
        resetMs: rateLimitResult.resetMs,
      },
    };
  }

  try {
    const data = calculateTaxDeduction(grossAmount, taxRateBps);
    return {
      status: 200,
      success: true,
      data,
      rateLimit: {
        remaining: rateLimitResult.remaining,
        resetMs: rateLimitResult.resetMs,
      },
    };
  } catch (err: any) {
    return {
      status: 400,
      success: false,
      error: err?.message || "Invalid input parameters for tax_deduction_estimator",
      rateLimit: {
        remaining: rateLimitResult.remaining,
        resetMs: rateLimitResult.resetMs,
      },
    };
  }
}
