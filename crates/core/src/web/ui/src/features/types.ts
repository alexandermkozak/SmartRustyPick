import type {Component} from 'vue'

/**
 * What a slice publishes so the shell can offer it as a tab.
 *
 * The shell knows a feature by this and nothing else: it never imports a view,
 * a composable or an API from inside a slice.
 */
export interface FeatureTab {
    /** Stable identifier, used as the tab's key. */
    id: string
    /** What the tab is called on screen. */
    label: string
    /** The view mounted when the tab is selected. */
    component: Component
}
