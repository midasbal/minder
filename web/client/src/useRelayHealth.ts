import { useEffect, useState } from 'react';

export type RelayStatus = 'checking' | 'online' | 'offline';

/** Asks the relay, once, whether it is up. The page and the relay share an origin. */
export function useRelayHealth(): RelayStatus {
  const [status, setStatus] = useState<RelayStatus>('checking');

  useEffect(() => {
    const controller = new AbortController();
    fetch('/health', { signal: controller.signal })
      .then((response) => (response.ok ? response.json() : Promise.reject(new Error(`${response.status}`))))
      .then((body: { status?: string }) => setStatus(body.status === 'ok' ? 'online' : 'offline'))
      .catch(() => {
        if (!controller.signal.aborted) setStatus('offline');
      });
    return () => controller.abort();
  }, []);

  return status;
}
