/** Display helpers shared by the views. */

/** A duration as the two largest useful units: `3d 4h`, `12m 5s`, `0s`. */
export function duration(seconds: number | null | undefined): string {
    if (seconds === null || seconds === undefined) return '—'
    const units: Array<[string, number]> = [
        ['d', 86400],
        ['h', 3600],
        ['m', 60],
    ]
    let remaining = Math.max(0, Math.floor(seconds))
    const parts: string[] = []
    for (const [suffix, size] of units) {
        if (remaining >= size) {
            parts.push(`${Math.floor(remaining / size)}${suffix}`)
            remaining %= size
        }
        if (parts.length === 2) break
    }
    if (parts.length < 2) parts.push(`${remaining}s`)
    return parts.join(' ')
}

/** A byte count in the largest unit that keeps it readable. */
export function bytes(value: number | null | undefined): string {
    const units = ['B', 'KB', 'MB', 'GB', 'TB']
    let size = Number(value ?? 0)
    let unit = 0
    while (size >= 1024 && unit < units.length - 1) {
        size /= 1024
        unit += 1
    }
    return `${unit === 0 ? size : size.toFixed(1)} ${units[unit]}`
}

/** A count with the viewer's thousands separators. */
export function count(value: number | null | undefined): string {
    return Number(value ?? 0).toLocaleString()
}

/** A thumbprint shortened for a table cell; the full value goes in a title. */
export function shortThumbprint(thumbprint: string): string {
    return thumbprint.length > 16 ? `${thumbprint.slice(0, 16)}…` : thumbprint
}

/** A Unix timestamp as a local date and time. */
export function timestamp(seconds: number | null | undefined): string {
    if (!seconds) return '—'
    return new Date(seconds * 1000).toLocaleString()
}

/**
 * A certificate expiry as a date and how long is left: `2027-09-13 · 364 days`.
 *
 * The remaining days are what the reader is actually deciding on — a date alone
 * makes every row arithmetic — and an already-expired certificate says so
 * rather than showing a negative number, because "-3 days" is a sentence nobody
 * reads correctly at a glance.
 */
export function expiry(
    at: string | null | undefined,
    days: number | null | undefined,
): {text: string; state: 'unknown' | 'expired' | 'soon' | 'ok'} {
    // Authorized by thumbprint alone: the database never saw the certificate.
    if (!at) return {text: 'unknown', state: 'unknown'}
    const date = at.slice(0, 10)
    if (days === null || days === undefined) return {text: date, state: 'ok'}
    if (days < 0) return {text: `${date} · expired`, state: 'expired'}
    if (days === 0) return {text: `${date} · today`, state: 'soon'}
    // Thirty days is the usual outer edge of "arrange the reissue now"; it is a
    // display threshold only, and nothing in the server depends on it.
    return {text: `${date} · ${days} days`, state: days <= 30 ? 'soon' : 'ok'}
}
