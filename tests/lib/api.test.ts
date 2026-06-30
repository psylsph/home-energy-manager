import { describe, it, expect, vi, beforeEach } from 'vitest';

// Mock fetch before importing the module
const mockFetch = vi.fn();
vi.stubGlobal('fetch', mockFetch);

// Set a non-dev port so getServerPort() returns it
Object.defineProperty(window, 'location', {
  value: { port: '7337', hostname: '127.0.0.1' },
  writable: true,
});

// Import after mocks are set up
const { apiGet, apiPost, apiPut, fetchHistory, getApiBase, getWsUrl } = await import('../../src/lib/api');

describe('api', () => {
  beforeEach(() => {
    mockFetch.mockReset();
  });

  describe('getApiBase', () => {
    it('returns URL with the current port', () => {
      const base = getApiBase();
      expect(base).toBe('http://127.0.0.1:7337');
    });
  });

  describe('getWsUrl', () => {
    it('returns ws URL with the current port', () => {
      const url = getWsUrl();
      expect(url).toBe('ws://127.0.0.1:7337/ws');
    });
  });

  describe('apiGet', () => {
    it('returns parsed JSON on success', async () => {
      mockFetch.mockResolvedValueOnce({
        ok: true,
        json: async () => ({ ok: true, data: 'hello' }),
      });
      const result = await apiGet<{ ok: boolean; data: string }>('/test');
      expect(result).toEqual({ ok: true, data: 'hello' });
    });

    it('throws with error message from JSON body on non-ok response', async () => {
      mockFetch.mockResolvedValueOnce({
        ok: false,
        status: 400,
        json: async () => ({ ok: false, error: 'Bad request' }),
      });
      await expect(apiGet('/test')).rejects.toThrow('Bad request');
    });

    it('throws with status code when non-ok response has no JSON body', async () => {
      mockFetch.mockResolvedValueOnce({
        ok: false,
        status: 500,
        json: async () => { throw new Error('not json'); },
      });
      await expect(apiGet('/test')).rejects.toThrow('API error: 500');
    });

    it('throws with error message when response has ok=false', async () => {
      mockFetch.mockResolvedValueOnce({
        ok: true,
        json: async () => ({ ok: false, error: 'Something went wrong' }),
      });
      await expect(apiGet('/test')).rejects.toThrow('Something went wrong');
    });

    it('throws generic message when response has ok=false without error field', async () => {
      mockFetch.mockResolvedValueOnce({
        ok: true,
        json: async () => ({ ok: false }),
      });
      await expect(apiGet('/test')).rejects.toThrow('API returned an error');
    });

    it('re-throws network errors', async () => {
      mockFetch.mockRejectedValueOnce(new Error('Network failure'));
      await expect(apiGet('/test')).rejects.toThrow('Network failure');
    });
  });

  describe('apiPost', () => {
    it('sends POST with JSON body', async () => {
      mockFetch.mockResolvedValueOnce({
        ok: true,
        json: async () => ({ ok: true }),
      });
      await apiPost('/test', { key: 'value' });
      expect(mockFetch).toHaveBeenCalledWith(
        'http://127.0.0.1:7337/test',
        expect.objectContaining({
          method: 'POST',
          headers: { 'Content-Type': 'application/json' },
          body: JSON.stringify({ key: 'value' }),
        }),
      );
    });

    it('sends POST without body when body is undefined', async () => {
      mockFetch.mockResolvedValueOnce({
        ok: true,
        json: async () => ({ ok: true }),
      });
      await apiPost('/test');
      expect(mockFetch).toHaveBeenCalledWith(
        'http://127.0.0.1:7337/test',
        expect.objectContaining({
          method: 'POST',
          body: undefined,
        }),
      );
    });
  });

  describe('apiPut', () => {
    it('sends PUT with JSON body', async () => {
      mockFetch.mockResolvedValueOnce({
        ok: true,
        json: async () => ({ ok: true }),
      });
      await apiPut('/test', { key: 'value' });
      expect(mockFetch).toHaveBeenCalledWith(
        'http://127.0.0.1:7337/test',
        expect.objectContaining({
          method: 'PUT',
          headers: { 'Content-Type': 'application/json' },
          body: JSON.stringify({ key: 'value' }),
        }),
      );
    });
  });

  describe('fetchHistory', () => {
    it('adds start_ms/end_ms for today range', async () => {
      mockFetch.mockResolvedValueOnce({
        ok: true,
        json: async () => ({ ok: true, data: {} }),
      });
      await fetchHistory('today', ['solar_power']);
      const url = mockFetch.mock.calls[0][0] as string;
      expect(url).toContain('range=today');
      expect(url).toContain('start_ms=');
      expect(url).toContain('end_ms=');
    });

    it('adds start_ms/end_ms for month range', async () => {
      mockFetch.mockResolvedValueOnce({
        ok: true,
        json: async () => ({ ok: true, data: {} }),
      });
      await fetchHistory('month', ['solar_power']);
      const url = mockFetch.mock.calls[0][0] as string;
      expect(url).toContain('range=month');
      expect(url).toContain('start_ms=');
      expect(url).toContain('end_ms=');
    });

    it('does not add window params for custom range', async () => {
      mockFetch.mockResolvedValueOnce({
        ok: true,
        json: async () => ({ ok: true, data: {} }),
      });
      await fetchHistory('7d', ['solar_power']);
      const url = mockFetch.mock.calls[0][0] as string;
      expect(url).toContain('range=7d');
      expect(url).not.toContain('start_ms=');
    });

    it('adds rolling=true param when rolling is true', async () => {
      mockFetch.mockResolvedValueOnce({
        ok: true,
        json: async () => ({ ok: true, data: {} }),
      });
      await fetchHistory('today', ['solar_power'], 0, true);
      const url = mockFetch.mock.calls[0][0] as string;
      expect(url).toContain('rolling=true');
    });

    it('joins multiple fields with comma', async () => {
      mockFetch.mockResolvedValueOnce({
        ok: true,
        json: async () => ({ ok: true, data: {} }),
      });
      await fetchHistory('today', ['solar_power', 'grid_power']);
      const url = mockFetch.mock.calls[0][0] as string;
      expect(url).toContain('fields=solar_power%2Cgrid_power');
    });
  });
});
