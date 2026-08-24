const URL_PATTERN = /https?:\/\/[^\s)]+/gi;

export const UPDATE_CHECK_RETRY_DELAYS_MS = [800, 2_000, 5_000] as const;
export const UPDATE_DOWNLOAD_RETRY_DELAYS_MS = [1_000, 2_500, 5_000] as const;

const NON_RETRYABLE_HINTS = [
  "signature",
  "checksum",
  "hash mismatch",
  "target not found",
  "no matching platform",
  "permission denied",
  "no space left",
  "disk full",
];

const RETRYABLE_NETWORK_HINTS = [
  "error sending request",
  "failed to send request",
  "timeout",
  "timed out",
  "network",
  "dns",
  "tls",
  "ssl",
  "connection reset",
  "connection refused",
  "connection aborted",
  "broken pipe",
  "unexpected eof",
  "temporarily unavailable",
  "temporary failure",
  "name or service not known",
  "no route to host",
  "unreachable",
];

export type RetryContext = {
  retryIndex: number;
  totalRetries: number;
  delayMs: number;
  error: unknown;
};

export type RetryOptions = {
  delaysMs: readonly number[];
  shouldRetry: (error: unknown) => boolean;
  onRetry?: (context: RetryContext) => void | Promise<void>;
};

export function normalizeUpdaterErrorMessage(error: unknown): string {
  if (error instanceof Error && error.message) {
    return error.message;
  }
  if (typeof error === "string") {
    return error;
  }
  try {
    return JSON.stringify(error) ?? String(error);
  } catch {
    return String(error);
  }
}

export function sanitizeUpdaterErrorMessage(error: unknown, maxLength = 240): string {
  const normalized = normalizeUpdaterErrorMessage(error).replace(URL_PATTERN, "[URL]");
  const compact = normalized.replace(/\s+/g, " ").trim();
  if (compact.length <= maxLength) {
    return compact;
  }
  return `${compact.slice(0, maxLength)}...`;
}

function parseHttpStatusCode(message: string): number | null {
  const directMatch = message.match(/\bstatus(?:\s+code)?[:=\s]+(\d{3})\b/i);
  if (directMatch?.[1]) {
    return Number(directMatch[1]);
  }
  const httpMatch = message.match(/\bhttp\s*(\d{3})\b/i);
  if (httpMatch?.[1]) {
    return Number(httpMatch[1]);
  }
  return null;
}

export function isRetryableUpdaterError(error: unknown): boolean {
  const raw = normalizeUpdaterErrorMessage(error).toLowerCase();
  if (!raw || NON_RETRYABLE_HINTS.some((hint) => raw.includes(hint))) {
    return false;
  }

  const statusCode = parseHttpStatusCode(raw);
  if (statusCode !== null) {
    if (statusCode >= 500 || statusCode === 408 || statusCode === 429) {
      return true;
    }
    if (statusCode >= 400) {
      return false;
    }
  }

  return RETRYABLE_NETWORK_HINTS.some((hint) => raw.includes(hint));
}

function sleep(ms: number): Promise<void> {
  return new Promise((resolve) => globalThis.setTimeout(resolve, ms));
}

function withJitter(delayMs: number): number {
  return delayMs + Math.floor(Math.random() * 350);
}

export async function retryWithBackoff<T>(
  operation: (attempt: number) => Promise<T>,
  options: RetryOptions,
): Promise<T> {
  const maxAttempts = Math.max(1, options.delaysMs.length + 1);
  let lastError: unknown;

  for (let attempt = 1; attempt <= maxAttempts; attempt += 1) {
    try {
      return await operation(attempt);
    } catch (error) {
      lastError = error;
      if (!options.shouldRetry(error) || attempt >= maxAttempts) {
        throw error;
      }

      const retryIndex = attempt;
      const delayMs = withJitter(options.delaysMs[retryIndex - 1] ?? options.delaysMs.at(-1) ?? 0);
      await options.onRetry?.({
        retryIndex,
        totalRetries: maxAttempts - 1,
        delayMs,
        error,
      });
      await sleep(delayMs);
    }
  }

  throw lastError ?? new Error("Update retry failed without an explicit error");
}
