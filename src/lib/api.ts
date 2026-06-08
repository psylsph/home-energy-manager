const isTauri = typeof window !== 'undefined' && '__TAURI__' in window;

function getServerPort(): string {
  // When served by the Axum server (production Tauri or LAN browser),
  // window.location.port gives the correct port.
  // When served by Vite dev server (port 5173), fall back to 7337
  // since that's the Axum port in dev mode.
  if (typeof window !== 'undefined' && window.location.port) {
    const p = window.location.port;
    if (p !== '5173') return p;
  }
  return '7337';
}

export function getApiBase(): string {
  const port = getServerPort();
  if (isTauri) return `http://127.0.0.1:${port}`;
  return `http://${window.location.hostname}:${port}`;
}

export function getWsUrl(): string {
  const port = getServerPort();
  if (isTauri) return `ws://127.0.0.1:${port}/ws`;
  return `ws://${window.location.hostname}:${port}/ws`;
}

async function parseApiResponse<T>(res: Response): Promise<T> {
  let data: unknown = null;
  try {
    data = await res.json();
  } catch {
    // Some failed responses may not have a JSON body.
  }

  if (!res.ok) {
    // Backend returns 400 with {ok:false, error:"..."} — extract the message.
    if (
      data != null
      && typeof data === 'object'
      && 'error' in data
      && typeof (data as { error?: unknown }).error === 'string'
    ) {
      throw new Error((data as { error: string }).error);
    }
    throw new Error(`API error: ${res.status}`);
  }

  if (
    data != null
    && typeof data === 'object'
    && 'ok' in data
    && (data as { ok?: unknown }).ok === false
  ) {
    const message = 'error' in data && typeof (data as { error?: unknown }).error === 'string'
      ? (data as { error: string }).error
      : 'API returned an error';
    throw new Error(message);
  }

  return data as T;
}

export async function apiGet<T>(path: string): Promise<T> {
  const res = await fetch(`${getApiBase()}${path}`);
  return parseApiResponse<T>(res);
}

export async function apiPost<T>(path: string, body?: unknown): Promise<T> {
  const res = await fetch(`${getApiBase()}${path}`, {
    method: 'POST',
    headers: { 'Content-Type': 'application/json' },
    body: body ? JSON.stringify(body) : undefined,
  });
  return parseApiResponse<T>(res);
}

export async function apiPut<T>(path: string, body: unknown): Promise<T> {
  const res = await fetch(`${getApiBase()}${path}`, {
    method: 'PUT',
    headers: { 'Content-Type': 'application/json' },
    body: JSON.stringify(body),
  });
  return parseApiResponse<T>(res);
}

export async function fetchHistory(
  range: string,
  fields: string[],
  offset: number = 0,
  rolling: boolean = false,
): Promise<Record<string, Array<{ t: number; v: number }>>> {
  const params = new URLSearchParams({
    range,
    fields: fields.join(','),
    offset: String(offset),
  });
  if (rolling) {
    params.set('rolling', 'true');
  }
  const res = await apiGet<{ ok: boolean; data: Record<string, Array<{ t: number; v: number }>> }>(
    `/api/history?${params}`,
  );
  return res.data;
}

export { isTauri, getServerPort };
