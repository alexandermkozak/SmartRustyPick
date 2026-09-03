/**
 * The health objects the database hands back, and the small amount of reading
 * a page does of them.
 *
 * Deliberately no thresholds here. The verdicts are decided by the server, in
 * one place, so the CLI, the remote protocol and this page cannot disagree
 * about where the line is - and a page that re-derived them would be a second
 * rule able to drift from the first. What is here is presentation: which
 * verdict sorts first, what it is called, what it is coloured.
 *
 * A page branches on `id` and `verdict`. `label`, `threshold` and `detail` are
 * prose written by the server for a person to read, exactly as an error
 * message is, and are rendered rather than matched on.
 */

/** What a measure says to do about itself. */
export type Verdict = 'good' | 'watch' | 'act'

/** One thing measured, and what it means. */
export interface Measure {
    /** Stable identifier - `skew`, `load_factor`, `dominant_value`, … */
    id: string
    label: string
    /** Already formatted by the server, `—` when there is nothing to report. */
    value: string
    verdict: Verdict
    /** The rule that produced the verdict, so a reader can argue with it. */
    threshold: string
    /** What it means, and for anything but `good`, what to do about it. */
    detail: string
}

export interface Health {
    verdict: Verdict
    measures: Measure[]
}

/**
 * The cheaper form a *listing* carries: a verdict and short phrases, with no
 * measures behind them. `LIST.FILES` and `LIST.ACCOUNTS` answer "which of these
 * is worth opening", and answering that must not cost what opening one costs.
 */
export interface HealthSummary {
    verdict: Verdict
    reasons: string[]
}

/** Worst last, so a maximum over this ordering is the roll-up. */
const ORDER: Record<Verdict, number> = {good: 0, watch: 1, act: 2}

export const NO_HEALTH: Health = {verdict: 'good', measures: []}

/** A verdict from an older server, or from a reply that carries none. */
export function verdictOf(value: unknown): Verdict {
    return value === 'watch' || value === 'act' ? value : 'good'
}

export function worse(a: Verdict, b: Verdict): Verdict {
    return ORDER[a] >= ORDER[b] ? a : b
}

/** The worst verdict over a set of things that each carry one. */
export function rollUp(verdicts: Verdict[]): Verdict {
    return verdicts.reduce(worse, 'good')
}

/** What the verdict is called on screen. */
export function verdictLabel(verdict: Verdict): string {
    return verdict === 'act' ? 'needs attention' : verdict === 'watch' ? 'watch' : 'healthy'
}

/**
 * The measures worth reading first: anything that is not `good`, worst first.
 * A stable sort, so two measures of the same verdict keep the server's order -
 * which is the order they were judged in and reads as a sequence.
 */
export function concerns(health: Health | null | undefined): Measure[] {
    if (!health) return []
    return health.measures
        .filter((measure) => measure.verdict !== 'good')
        .sort((a, b) => ORDER[b.verdict] - ORDER[a.verdict])
}

/** One sentence for a panel that has no room for the whole table. */
export function summarise(health: Health | null | undefined): string {
    const worrying = concerns(health)
    if (!worrying.length) return 'Nothing to do.'
    return worrying.map((measure) => `${measure.label}: ${measure.value}`).join('; ')
}
