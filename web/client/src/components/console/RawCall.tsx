import type { RpcTrace } from '../../chain/rpc';

/** JSON with two-space indent, but a list of plain numbers (a byte array) stays on one line. */
export function prettyJson(value: unknown): string {
  return JSON.stringify(value, null, 2).replace(/\[\s+((?:-?\d+,?\s+)+)\]/g, (_, items: string) => `[${items.trim().replace(/\s+/g, ' ')}]`);
}

/** What went over the wire, for a visitor who wants to see the call is real. */
export function RawCall({ trace }: { trace: RpcTrace & { args?: unknown; method?: string } }) {
  return (
    <details className="raw">
      <summary>Raw RPC request and response</summary>
      <p className="raw__meta">
        POST <code>{trace.endpoint}</code>, answered in {trace.milliseconds} ms
      </p>
      <p className="raw__title">Request</p>
      <pre className="raw__code">{prettyJson(trace.request)}</pre>
      {trace.args !== undefined && (
        <>
          <p className="raw__title">
            Arguments of <code>{trace.method}</code>, before base64
          </p>
          <pre className="raw__code">{prettyJson(trace.args)}</pre>
        </>
      )}
      <p className="raw__title">Response, as the node sent it</p>
      <pre className="raw__code">{prettyJson(trace.response)}</pre>
    </details>
  );
}
