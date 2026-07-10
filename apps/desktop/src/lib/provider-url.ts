export function providerEndpointIsChatCompletions(baseUrl: string) {
  const path = baseUrl.trim().split("?")[0]?.replace(/\/+$/, "") ?? "";
  return path.endsWith("/chat/completions");
}
