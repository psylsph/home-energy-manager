const isTauri = typeof window !== 'undefined' && '__TAURI__' in window;

export function getApiBase(): string {
  if (isTauri) return 'http://127.0.0.1:7337';
  return `http://${window.location.hostname}:7337`;
}

export function getWsUrl(): string {
  if (isTauri) return 'ws://127.0.0.1:7337/ws';
  return `ws://${window.location.hostname}:7337/ws`;
}

export async function apiGet<T>(path: string): Promise<T> {
  const res = await fetch(`${getApiBase()}${path}`);
  if (!res.ok) throw new Error(`API error: ${res.status}`);
  return res.json();
}

export async function apiPost<T>(path: string, body?: unknown): Promise<T> {
  const res = await fetch(`${getApiBase()}${path}`, {
    method: 'POST',
    headers: { 'Content-Type': 'application/json' },
    body: body ? JSON.stringify(body) : undefined,
  });
  if (!res.ok) throw new Error(`API error: ${res.status}`);
  return res.json();
}

export { isTauri };
