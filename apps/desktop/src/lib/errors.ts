export function userFacingError(error: unknown): string {
  const message = error instanceof Error ? error.message : String(error);
  return message.replace(/^Error:\s*/i, "").replace(/^invalid config:\s*/i, "");
}
