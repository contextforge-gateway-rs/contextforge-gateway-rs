# Publishing This Book

> 📚 **Publishing path:** mdBook renders `docs/book/src/` into static HTML,
> and the GitHub Pages workflow deploys that HTML from `main`.

## Local Build And Preview

The Pages workflow uses mdBook `0.5.3`; use the same version locally:

```bash
cargo install mdbook --version 0.5.3 --locked
```

Build and preview:

```bash
mdbook build docs/book
mdbook serve docs/book --hostname 127.0.0.1 --port 3000 --open
```

The generated HTML lands in `docs/book/book/`, which is ignored by git. Never
edit files there; edit `docs/book/src/` instead.

## Validation Before Pushing

```bash
mdbook build docs/book
mdbook test docs/book
git diff --check
```

`mdbook test` runs Rust code blocks as tests and confirms that every chapter
in `SUMMARY.md` parses.

## Workflow Behavior

`.github/workflows/pages.yml` runs when a change touches `docs/book/**` or the
workflow file itself:

| Trigger | Jobs |
| --- | --- |
| Pull request | `build` only: install mdBook, build the book, upload `docs/book/book` as the Pages artifact. |
| Push to `main` | `build`, then `deploy` publishes the artifact to GitHub Pages. |
| Manual `workflow_dispatch` | Same as a pull request run. |

Builds are cancelled when a newer run starts on the same ref; deploys are
serialized and never cancelled mid-flight.

## Repository Settings

Publishing requires the repository's GitHub Pages source to be set to
`GitHub Actions`. Without that setting, the deploy job cannot publish the
uploaded artifact.

## Adding Or Renaming Chapters

1. Add or rename the Markdown file under `docs/book/src/`.
2. Update `docs/book/src/SUMMARY.md`; its order is the reader's path through
   the book.
3. Run the validation commands above.

Draft chapters use the `> Status: draft. To be implemented.` marker followed
by a `## To implement` list, so unfinished pages stay visible and navigable.
