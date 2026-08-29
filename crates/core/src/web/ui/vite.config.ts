/// <reference types="vitest/config" />
import {defineConfig} from 'vite'
import vue from '@vitejs/plugin-vue'

// The built bundle is embedded in the Rust binary with `include_str!`, which
// needs paths known at compile time - so the output filenames are fixed rather
// than content-hashed. Nothing is lost: the server sends `Cache-Control:
// no-store`, so there is no cache to bust.
export default defineConfig(({command}) => ({
    plugins: [vue()],
    // Built assets are served from /dist/; the dev server serves from the root.
    base: command === 'build' ? '/dist/' : '/',
    build: {
        outDir: '../assets/dist',
        emptyOutDir: true,
        cssCodeSplit: false,
        // A single self-contained bundle: no dynamic imports to embed, and no
        // inline styles for the page's strict Content-Security-Policy to refuse.
        rollupOptions: {
            output: {
                entryFileNames: 'app.js',
                chunkFileNames: 'app-[name].js',
                assetFileNames: 'app.[ext]',
            },
        },
    },
    test: {
        environment: 'jsdom',
        include: ['src/**/*.test.ts'],
    },
    server: {
        port: 5173,
        strictPort: true,
        // `npm run dev` serves the page while a real database server answers the
        // API, so the browser sees one origin and the cookie/token flow behaves.
        proxy: {
            '/api': 'http://127.0.0.1:8080',
            '/health': 'http://127.0.0.1:8080',
        },
    },
}))
