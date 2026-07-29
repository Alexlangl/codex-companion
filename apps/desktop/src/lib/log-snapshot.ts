import type { ApiRequestLog, RelayEvent } from "../types/domain";

export function apiRequestLogsEqual(left: ApiRequestLog[], right: ApiRequestLog[]): boolean {
  if (left.length !== right.length) return false;
  return left.every((request, index) => {
    const candidate = right[index];
    return candidate !== undefined
      && request.requestId === candidate.requestId
      && request.startedAt === candidate.startedAt
      && request.method === candidate.method
      && request.path === candidate.path
      && request.model === candidate.model
      && request.clientId === candidate.clientId
      && request.clientName === candidate.clientName
      && request.providerId === candidate.providerId
      && request.statusCode === candidate.statusCode
      && request.outcome === candidate.outcome
      && request.attempts === candidate.attempts
      && request.latencyMs === candidate.latencyMs
      && request.error === candidate.error
      && requestAttemptsEqual(request.attemptLog, candidate.attemptLog);
  });
}

function requestAttemptsEqual(
  left: ApiRequestLog["attemptLog"],
  right: ApiRequestLog["attemptLog"],
): boolean {
  if (left.length !== right.length) return false;
  return left.every((attempt, index) => {
    const candidate = right[index];
    return candidate !== undefined
      && attempt.attempt === candidate.attempt
      && attempt.providerId === candidate.providerId
      && attempt.routeReason === candidate.routeReason
      && attempt.startedAt === candidate.startedAt
      && attempt.finishedAt === candidate.finishedAt
      && attempt.statusCode === candidate.statusCode
      && attempt.outcome === candidate.outcome
      && attempt.latencyMs === candidate.latencyMs
      && attempt.error === candidate.error;
  });
}

export function relayEventsEqual(left: RelayEvent[], right: RelayEvent[]): boolean {
  if (left.length !== right.length) return false;
  return left.every((event, index) => {
    const candidate = right[index];
    return candidate !== undefined
      && event.timestamp === candidate.timestamp
      && event.kind === candidate.kind
      && event.providerId === candidate.providerId
      && event.message === candidate.message;
  });
}
