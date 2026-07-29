import type { RelayEvent } from "../types/domain";

export type RelayRequestEventGroup = {
  type: "request";
  requestId: string;
  events: RelayEvent[];
  latestAt: string;
};

export type RelayStandaloneEventGroup = {
  type: "standalone";
  event: RelayEvent;
  latestAt: string;
};

export type RelayDiagnosticGroup = RelayRequestEventGroup | RelayStandaloneEventGroup;

export function groupRelayDiagnosticEvents(events: RelayEvent[]): RelayDiagnosticGroup[] {
  const requestGroups = new Map<string, RelayEvent[]>();
  const standaloneGroups: RelayStandaloneEventGroup[] = [];

  for (const event of events) {
    const requestId = relayEventRequestId(event);
    if (!requestId) {
      standaloneGroups.push({ type: "standalone", event, latestAt: event.timestamp });
      continue;
    }
    const groupEvents = requestGroups.get(requestId) ?? [];
    groupEvents.push(event);
    requestGroups.set(requestId, groupEvents);
  }

  const groupedRequests = [...requestGroups.entries()].map(([requestId, groupEvents]) => {
    const sortedEvents = [...groupEvents].sort(compareEventTime);
    const latestEvent = sortedEvents.at(-1);
    return {
      type: "request" as const,
      requestId,
      events: sortedEvents,
      latestAt: latestEvent?.timestamp ?? "",
    };
  });

  return [...groupedRequests, ...standaloneGroups].sort((left, right) => (
    Date.parse(right.latestAt) - Date.parse(left.latestAt)
  ));
}

export function relayEventRequestId(event: RelayEvent): string | null {
  return event.message.match(/^\[([^\]]+)]/)?.[1] ?? null;
}

export function relayEventMessageText(event: RelayEvent): string {
  const requestPrefix = event.message.match(/^\[[^\]]+]\s*/)?.[0] ?? "";
  const normalized = requestPrefix ? event.message.slice(requestPrefix.length) : event.message;
  if (!event.providerId) return normalized || "Companion 本地代理事件";
  return normalized.startsWith(event.providerId)
    ? normalized.slice(event.providerId.length).trimStart()
    : normalized;
}

function compareEventTime(left: RelayEvent, right: RelayEvent): number {
  return Date.parse(left.timestamp) - Date.parse(right.timestamp);
}
