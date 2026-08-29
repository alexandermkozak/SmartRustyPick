/**
 * The rules that keep the slices vertical.
 *
 * Vertical slicing survives exactly as long as nobody takes the small shortcut
 * of importing straight into a neighbouring feature. That shortcut is invisible
 * in review and fatal over time: two slices become one tangle, and moving or
 * deleting a feature stops being a local change. So the boundaries are asserted
 * here rather than described in a document nobody re-reads.
 *
 * The rules:
 *
 * 1. A feature may import its own files and `@shared/...`. It may not reach
 *    into another feature - that is what a slice's `index.ts` is for, and only
 *    the registry may use it.
 * 2. `shared/` may not import a feature. The kernel cannot depend on the things
 *    built on top of it.
 * 3. Every slice publishes an `index.ts`, and the registry lists every slice
 *    that exists - so a feature cannot be half-wired and silently absent.
 */

import {describe, expect, it} from 'vitest'
import {readdirSync, readFileSync, statSync} from 'node:fs'
import {join} from 'node:path'

// Vitest runs from the project root; `import.meta.url` is not a file URL under
// the jsdom environment, so the tree is located from the working directory.
const SRC = join(process.cwd(), 'src')
const FEATURES = join(SRC, 'features')

/** Every source file under `directory`, recursively. */
function sourceFiles(directory: string): string[] {
    return readdirSync(directory, {recursive: true, encoding: 'utf8'})
        .map((entry) => join(directory, entry))
        .filter((path) => statSync(path).isFile())
        .filter((path) => /\.(ts|vue)$/.test(path))
}

/** The module specifiers a file imports from. */
function imports(path: string): string[] {
    const source = readFileSync(path, 'utf8')
    return [...source.matchAll(/(?:from|import)\s*\(?\s*['"]([^'"]+)['"]/g)].map(
        (match) => match[1],
    )
}

const featureNames = readdirSync(FEATURES, {withFileTypes: true})
    .filter((entry) => entry.isDirectory())
    .map((entry) => entry.name)

describe('the feature slices', () => {
    it('are all registered', () => {
        expect(featureNames.length).toBeGreaterThan(0)
        const registry = readFileSync(join(FEATURES, 'index.ts'), 'utf8')
        for (const name of featureNames) {
            expect(registry, `feature "${name}" exists but is not in the registry`).toContain(
                `from './${name}'`,
            )
        }
    })

    it('each publish a public surface', () => {
        for (const name of featureNames) {
            const barrel = join(FEATURES, name, 'index.ts')
            expect(() => statSync(barrel), `feature "${name}" has no index.ts`).not.toThrow()
        }
    })

    it('never import each other', () => {
        const offences: string[] = []

        for (const name of featureNames) {
            for (const path of sourceFiles(join(FEATURES, name))) {
                for (const specifier of imports(path)) {
                    const reachesAnotherFeature =
                        (specifier.startsWith('@features/') &&
                            !specifier.startsWith(`@features/${name}`)) ||
                        // A relative path that climbs out of this slice, e.g. '../accounts/api'.
                        (specifier.startsWith('../') &&
                            featureNames.some(
                                (other) => other !== name && specifier.includes(`/${other}/`),
                            ))
                    if (reachesAnotherFeature) {
                        offences.push(`${path.slice(SRC.length + 1)} imports ${specifier}`)
                    }
                }
            }
        }

        expect(offences, 'features must talk through shared/, not to each other').toEqual([])
    })
})

describe('the shared kernel', () => {
    it('does not depend on any feature', () => {
        const offences: string[] = []
        for (const path of sourceFiles(join(SRC, 'shared'))) {
            for (const specifier of imports(path)) {
                if (specifier.includes('features/')) {
                    offences.push(`${path.slice(SRC.length + 1)} imports ${specifier}`)
                }
            }
        }
        expect(offences, 'shared/ is the kernel: nothing built on it may be imported back').toEqual(
            [],
        )
    })
})

describe('the shell', () => {
    it('reaches features only through their public surface', () => {
        const offences: string[] = []
        for (const specifier of imports(join(SRC, 'App.vue'))) {
            // './features', './features/overview' - fine. Anything deeper is not.
            if (/^\.\/features\/[^/]+\/./.test(specifier)) {
                offences.push(`App.vue imports ${specifier}`)
            }
        }
        expect(offences, 'the shell composes slices; it does not reach inside them').toEqual([])
    })
})
