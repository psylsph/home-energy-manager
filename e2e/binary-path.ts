/** Return Cargo's platform-specific executable filename for the backend. */
export function backendExecutableName(platform: NodeJS.Platform = process.platform): string {
  return platform === 'win32' ? 'givenergy-local.exe' : 'givenergy-local';
}
