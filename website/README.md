# Termlyx website

Static marketing site: hand-written HTML in `src/`, Tailwind v4 for CSS, no runtime JS framework.
Fonts (Geist, JetBrains Mono) are self-hosted in `src/fonts/` under the SIL OFL; shipped pages make no third-party requests.

```sh
pnpm install
pnpm build   # src/ -> dist/ (styles.css compiled and minified)
pnpm check   # rules over dist/: no-external-assets, no-version, no-competitor, anchors, media-budget, video-attrs
pnpm test    # self-test of the check script
pnpm e2e     # Playwright: downloads, reduced motion, CSP violations (needs apps/tauri deps installed)
pnpm og      # re-render src/og.png (1200x630) from og/card.html
pnpm dev     # build, then serve dist/ on http://localhost:8080
```

`pnpm dev` uses python's plain static server, so there are no pretty URLs: open `/privacy.html` locally (Cloudflare serves `/privacy`).

`pnpm check` prints each failure as `file: rule: detail` and exits non-zero.

## Clips

Product clips are rendered into `clips/**/renders/` (git-ignored) and the published
mp4/webm/jpg files are copied into `src/media/` (budgets: video 3 MiB, poster 200 KiB).
Render one clip on the devvm with `website/clips/render.sh <clip>` (e.g. `hero`); it copies the
web encodes into `src/media/`. It renders what is committed, so commit the composition first.

## Cloudflare Pages

- Root directory: `website`
- Build command: `pnpm install --frozen-lockfile && pnpm build`
- Output directory: `dist`
- Environment variable: `NODE_VERSION=22` and `PNPM_VERSION=12.4.1`

`src/_headers` (CSP, referrer, nosniff, permissions, cache rules) is copied into `dist/` and read by Pages.
`/media/*` is cached one day (files have no content hash), so a re-rendered clip with the same name can take up to a day to reach returning visitors; `/fonts/*` one year, immutable.

## Open items

- Live at https://termlyx.pages.dev (Cloudflare Pages project `termlyx`, production branch `main`; PRs get preview URLs).
- Custom domain: when added, update the absolute `og:image` host in both pages.
