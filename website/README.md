# Boon Website

Static assets for https://boon.run are built into `website/content`. Only the files inside that directory are published.

## Regenerate content

1. From the repository root run `python3 website/generate_site.py` (or `./website/generate_site.py`).
2. The script copies `docs/` into `website/content/docs/`, converts `README.md` to HTML, applies the site chrome defined in `website/index.html.template`, and writes the result to `website/content/index.html`. The companion stylesheet stays at `website/content/styles.css`.
3. Review the changes and commit when satisfied.

Requirements: Python 3 and the `markdown` package. Install with `python3 -m pip install --user markdown` if it is not already available.

## Local preview / smoke test

Serve the generated bundle to ensure everything renders as expected:

```bash
# install miniserve once (requires Rust toolchain)
cargo install miniserve

# serve the generated bundle
(cd website/content && miniserve --port 8079 --index index.html --spa)
```

Open http://localhost:8079/ in a browser.

## Template tweaks

Update `website/index.html.template` for layout chrome (header/footer/buttons) and re-run the generator. Styling lives in `website/content/styles.css`.
