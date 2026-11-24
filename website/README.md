# Termlyx website

Static marketing site: hand-written HTML in `src/`, Tailwind v4 for CSS, no runtime JS framework.
Fonts (Geist, JetBrains Mono) are self-hosted in `src/fonts/` under the SIL OFL; shipped pages make no third-party requests.

```sh
pnpm install
pnpm build   # src/ -> dist/ (styles.css compiled and minified)
pnpm check   # rules over dist/: no-external-assets, no-version, no-competitor, anchors, media-budget, video-attrs
pnpm test    # self-test of the check script
pnpm dev     # build, then serve dist/ on http://localhost:8080
```

`pnpm check` prints each failure as `file: rule: detail` and exits non-zero.

## Clips

Product clips are rendered into `clips/**/renders/` (git-ignored) and the published
mp4/webm/jpg files are copied into `src/media/` (budgets: video 3 MiB, poster 200 KiB).
The rendering steps are added with the clip tooling.

## Cloudflare Pages

- Root directory: `website`
- Build command: `pnpm install --frozen-lockfile && pnpm build`
- Output directory: `dist`
- Environment variable: `NODE_VERSION=22`
