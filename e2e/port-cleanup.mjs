import { execFileSync } from 'child_process';
import { readdirSync, readFileSync, readlinkSync } from 'fs';

/**
 * Kill processes listening on a test port without requiring a system utility.
 *
 * lsof is used when available; Linux's /proc tables provide a dependency-free
 * fallback for minimal test containers. The current runner is always excluded
 * because the mock global setup owns listeners in that process.
 *
 * @param {number} port
 * @returns {void}
 */
export function killPort(port) {
  if (!Number.isInteger(port) || port < 1 || port > 65535) return;

  const pids = new Set();
  try {
    const output = execFileSync('lsof', ['-ti', `tcp:${port}`], {
      encoding: 'utf8',
      stdio: ['ignore', 'pipe', 'ignore'],
    });
    for (const value of output.trim().split(/\s+/)) {
      const pid = Number(value);
      if (Number.isSafeInteger(pid) && pid > 0) pids.add(pid);
    }
  } catch {
    // lsof is optional; use the Linux fallback below.
  }

  for (const pid of listeningPidsFromProc(port)) pids.add(pid);
  pids.delete(process.pid);
  for (const pid of pids) {
    try {
      process.kill(pid, 'SIGKILL');
    } catch {
      // The process may have exited between discovery and cleanup.
    }
  }
}

/**
 * Find listening PIDs from Linux's TCP tables and file descriptors.
 *
 * @param {number} port
 * @returns {number[]}
 */
function listeningPidsFromProc(port) {
  const inodes = new Set();
  const hexPort = port.toString(16).toUpperCase().padStart(4, '0');
  for (const netFile of ['/proc/net/tcp', '/proc/net/tcp6']) {
    let content;
    try {
      content = readFileSync(netFile, 'utf8');
    } catch {
      continue;
    }
    for (const line of content.split('\n').slice(1)) {
      const fields = line.trim().split(/\s+/);
      if (fields.length < 10 || fields[3] !== '0A') continue;
      if (fields[1]?.endsWith(`:${hexPort}`)) inodes.add(fields[9]);
    }
  }
  if (inodes.size === 0) return [];

  const pids = [];
  let entries;
  try {
    entries = readdirSync('/proc');
  } catch {
    return [];
  }
  for (const entry of entries) {
    if (!/^\d+$/.test(entry)) continue;
    const fdDir = `/proc/${entry}/fd`;
    let fds;
    try {
      fds = readdirSync(fdDir);
    } catch {
      continue;
    }
    for (const fd of fds) {
      let link;
      try {
        link = readlinkSync(`${fdDir}/${fd}`);
      } catch {
        continue;
      }
      const match = /^socket:\[(\d+)\]$/.exec(link);
      if (match && inodes.has(match[1])) {
        pids.push(Number(entry));
        break;
      }
    }
  }
  return pids;
}
