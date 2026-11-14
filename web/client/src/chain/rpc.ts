/*
  A small JSON-RPC client, used straight from the browser: for NEAR's `query` method, and for reading a transaction
  receipt from an Ethereum node.

  It tries the endpoints in order, remembers the one that last worked, and moves to the next on a network
  failure, a timeout, a non-2xx answer or a node-side error that another node may not share. An error that
  is the chain's own answer (an unknown account, say) is final: asking again elsewhere would not change it.
  At most a few requests are in flight at once, so a page load does not hammer a public node.
*/

export type RpcFailureKind = 'unreachable' | 'timeout' | 'rpc' | 'contract';

export class RpcFailure extends Error {
  readonly kind: RpcFailureKind;

  constructor(kind: RpcFailureKind, message: string) {
    super(message);
    this.name = 'RpcFailure';
    this.kind = kind;
  }
}

/** What went over the wire for one call, kept so a visitor can inspect it. */
export interface RpcTrace {
  endpoint: string;
  request: unknown;
  response: unknown;
  milliseconds: number;
}

export interface Rpc {
  /** One `query` call. Resolves with the node's `result`; rejects with an `RpcFailure`. */
  query(params: Record<string, unknown>): Promise<{ result: unknown; trace: RpcTrace }>;
}

/** Any JSON-RPC method, for nodes that are not NEAR's. */
export interface JsonRpc {
  call(method: string, params: unknown): Promise<{ result: unknown; trace: RpcTrace }>;
}

export interface RpcOptions {
  endpoints: readonly string[];
  fetchImpl?: typeof fetch;
  timeoutMs?: number;
  maxConcurrent?: number;
  /** What the nodes are called in an error message. */
  label?: string;
}

/** Node-side conditions that say "this node could not answer", as opposed to "the chain says no". */
const RETRYABLE_CAUSES = new Set(['TIMEOUT_ERROR', 'INTERNAL_ERROR', 'UNAVAILABLE_SHARD', 'NO_SYNCED_YET']);

function describeRpcError(error: unknown): { message: string; retryable: boolean } {
  const body = error as { name?: string; cause?: { name?: string; info?: unknown }; message?: string; data?: unknown } | null;
  const cause = body?.cause?.name;
  const detail = typeof body?.data === 'string' ? body.data : undefined;
  const message = [cause ?? body?.name ?? 'RPC error', detail ?? body?.message].filter(Boolean).join(': ');
  return { message, retryable: cause !== undefined && RETRYABLE_CAUSES.has(cause) };
}

/** A client for any JSON-RPC node, trying the endpoints in order. */
export function createJsonRpc(options: RpcOptions): JsonRpc {
  const { endpoints, timeoutMs = 8000, maxConcurrent = 4, label = 'RPC' } = options;
  if (endpoints.length === 0) throw new Error('at least one RPC endpoint is required');
  const fetchImpl = options.fetchImpl ?? ((...args: Parameters<typeof fetch>) => fetch(...args));

  let preferred = 0;
  let inFlight = 0;
  const waiting: Array<() => void> = [];

  const acquire = async () => {
    if (inFlight >= maxConcurrent) await new Promise<void>((resolve) => waiting.push(resolve));
    inFlight += 1;
  };
  const release = () => {
    inFlight -= 1;
    waiting.shift()?.();
  };

  async function attempt(
    endpoint: string,
    request: unknown,
  ): Promise<{ ok: true; result: unknown; trace: RpcTrace } | { ok: false; kind: RpcFailureKind; message: string; final: boolean }> {
    const started = Date.now();
    const controller = new AbortController();
    const timer = setTimeout(() => controller.abort(), timeoutMs);
    try {
      let response: Response;
      try {
        response = await fetchImpl(endpoint, {
          method: 'POST',
          headers: { 'content-type': 'application/json' },
          body: JSON.stringify(request),
          signal: controller.signal,
        });
      } catch (error) {
        const aborted = controller.signal.aborted;
        return {
          ok: false,
          kind: aborted ? 'timeout' : 'unreachable',
          message: aborted ? `no answer within ${timeoutMs / 1000} s` : (error as Error).message || 'network error',
          final: false,
        };
      }
      if (!response.ok) return { ok: false, kind: 'unreachable', message: `HTTP ${response.status}`, final: false };
      let json: { result?: unknown; error?: unknown };
      try {
        json = (await response.json()) as typeof json;
      } catch {
        return { ok: false, kind: 'unreachable', message: 'the answer was not JSON', final: false };
      }
      const milliseconds = Date.now() - started;
      if (json.error !== undefined) {
        const { message, retryable } = describeRpcError(json.error);
        return { ok: false, kind: 'rpc', message, final: !retryable };
      }
      return { ok: true, result: json.result, trace: { endpoint, request, response: json, milliseconds } };
    } finally {
      clearTimeout(timer);
    }
  }

  return {
    async call(method, params) {
      const request = { jsonrpc: '2.0', id: 'minder', method, params };
      await acquire();
      try {
        const failures: string[] = [];
        let lastKind: RpcFailureKind = 'unreachable';
        for (let step = 0; step < endpoints.length; step += 1) {
          const index = (preferred + step) % endpoints.length;
          const endpoint = endpoints[index]!;
          const outcome = await attempt(endpoint, request);
          if (outcome.ok) {
            preferred = index;
            return { result: outcome.result, trace: outcome.trace };
          }
          if (outcome.final) throw new RpcFailure('rpc', outcome.message);
          lastKind = outcome.kind;
          failures.push(`${new URL(endpoint).host}: ${outcome.message}`);
        }
        throw new RpcFailure(lastKind, `Could not reach the ${label} (${failures.join('; ')})`);
      } finally {
        release();
      }
    },
  };
}

/** A client for NEAR's `query` method. */
export function createRpc(options: RpcOptions): Rpc {
  const rpc = createJsonRpc({ label: 'NEAR RPC', ...options });
  return { query: (params) => rpc.call('query', params) };
}
