/** The notice the backend stamps into the snapshot after a dongle factory reset (issue #397). */
export interface ScheduleResetInfo {
  /** When the reset was confirmed, in unix seconds. */
  detected_at: number;
  /** When the schedule HEM would restore was taken, in unix seconds. */
  saved_at: number;
}

/** "14 Nov 18:02" in the browser's time zone, for the time of the saved schedule. */
export function formatSavedAt(unixSecs: number): string {
  const at = new Date(unixSecs * 1000);
  if (Number.isNaN(at.getTime())) return 'earlier';
  const date = at.toLocaleDateString('en-GB', { day: 'numeric', month: 'short' });
  const time = at.toLocaleTimeString('en-GB', { hour: '2-digit', minute: '2-digit', hour12: false });
  return `${date} ${time}`;
}
