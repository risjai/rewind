/**
 * Minimal client-side auth token holder.
 *
 * The Rewind web server is fail-closed on non-loopback binds: it requires
 * `Authorization: Bearer <token>` on REST and either that header or
 * `?token=<token>` on the WebSocket upgrade (browsers can't set headers on WS).
 *
 * Persistence: localStorage under `rewind_auth_token`.
 * Provisioning: if a request returns 401 we prompt once and retry. The user
 * copies the token from the server's startup banner or from `~/.rewind/auth_token`.
 *
 * Loopback deployments don't configure a token, so `getToken()` returns
 * `undefined` and the API client sends no Authorization header (current
 * backward-compatible behavior).
 */

const KEY = 'rewind_auth_token'

export function getToken(): string | undefined {
  try {
    return window.localStorage.getItem(KEY) ?? undefined
  } catch {
    return undefined
  }
}

export function setToken(token: string): void {
  try {
    window.localStorage.setItem(KEY, token)
  } catch {
    // Private browsing / storage disabled — silently skip.
  }
}

export function clearToken(): void {
  try {
    window.localStorage.removeItem(KEY)
  } catch {
    // no-op
  }
}

/**
 * Prompt the user for a token when a request returned 401. Called at most
 * once per failed request path. Returns the entered token or `undefined` if
 * the user cancelled (in which case the caller should surface the 401).
 */
export function promptForToken(message?: string): string | undefined {
  const msg =
    message ??
    'This Rewind server requires an auth token.\n\n' +
      'Find it in the server\'s startup banner or at ~/.rewind/auth_token on the host.'
  // eslint-disable-next-line no-alert
  const entered = window.prompt(msg)
  if (entered && entered.trim()) {
    const tok = entered.trim()
    setToken(tok)
    return tok
  }
  return undefined
}
