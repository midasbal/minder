/** `2026-11-14T16:07:19.062Z` as `16:07:19`. */
export const utcClock = (iso: string): string => iso.slice(11, 19);

/** The time between two instants, as `2 min 15 s`. */
export function elapsed(fromIso: string, toIso: string): string {
  const seconds = Math.round((Date.parse(toIso) - Date.parse(fromIso)) / 1000);
  return seconds >= 60 ? `${Math.floor(seconds / 60)} min ${seconds % 60} s` : `${seconds} s`;
}
