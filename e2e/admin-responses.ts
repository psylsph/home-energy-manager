/** Parse an E2E admin response and enforce its `{ ok: true }` contract. */
export async function requireOkJson(
  response: Response,
  action: string,
): Promise<Record<string, unknown>> {
  const body: unknown = await response.json().catch(() => null);
  if (!response.ok || typeof body !== 'object' || body === null || (body as { ok?: unknown }).ok !== true) {
    throw new Error(`${action} returned HTTP ${response.status}: ${JSON.stringify(body)}`);
  }
  return body as Record<string, unknown>;
}
