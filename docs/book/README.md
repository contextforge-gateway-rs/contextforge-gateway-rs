# Developing The ContextForge Data Plane Book

This directory contains the mdBook source for The ContextForge Data Plane Book.
The rendered book also documents its own publishing path in
[Publishing This Book](src/publishing-this-book.md); keep the two in sync when
the workflow or mdBook version changes.

## Layout

```text
docs/book/
  book.toml        mdBook configuration
  README.md        contributor notes for this book
  src/
    SUMMARY.md     chapter order and sidebar structure
    *.md           rendered book chapters
  book/            generated HTML output, ignored by git
```

Keep book source in `src/`. Do not edit generated files under `docs/book/book/`.

## Install mdBook

The GitHub Pages workflow installs `mdbook v0.5.3`, so local development should
use the same version:

```bash
cargo install mdbook --version 0.5.3 --locked
```

Check the installed version:

```bash
mdbook --version
```

## Render Locally

Build the static HTML:

```bash
mdbook build docs/book
```

The output is written to:

```text
docs/book/book/
```

Serve the book with live rebuilds:

```bash
mdbook serve docs/book --hostname 127.0.0.1 --port 3000
```

Then open:

```text
http://127.0.0.1:3000
```

Use `--open` if you want mdBook to open the browser:

```bash
mdbook serve docs/book --hostname 127.0.0.1 --port 3000 --open
```

## Validate Changes

Run these before pushing book changes:

```bash
mdbook build docs/book
mdbook test docs/book
git diff --check
```

`mdbook test` runs Rust code blocks as tests. For prose-only pages, it still
checks that mdBook can parse and walk every chapter in `SUMMARY.md`.

## Add Or Rename A Chapter

1. Add the Markdown file under `docs/book/src/`.
2. Add it to `docs/book/src/SUMMARY.md` in the intended reading order.
3. Run `mdbook build docs/book`.
4. Run `mdbook test docs/book`.

The chapter order in `SUMMARY.md` is the reader's numbered path through the
book. Keep that order intentional.

## Draft Chapters

Use this marker for pages that are intentionally present but not implemented:

```markdown
> Status: draft. To be implemented.
```

Follow it with a `## To implement` section and concrete bullets. That keeps the
book navigable while making unfinished work obvious.

## Publishing

The workflow at `.github/workflows/pages.yml` builds the book on pull requests
that touch book files and deploys on pushes to `main`.

Publishing expects GitHub Pages to use `GitHub Actions` as the repository's
Pages source. The workflow uploads `docs/book/book` as the Pages artifact.
