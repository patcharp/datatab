# Upstream

DataTab is derived from [DBFlux](https://github.com/0xErwin1/dbflux). It is not the official DBFlux, and the DBFlux project does not endorse or support it.

## Baseline

| Field | Value |
| --- | --- |
| Project | DBFlux |
| Repository | https://github.com/0xErwin1/dbflux |
| Commit | `76b46a00378627dc6fa6f3dda2d54191f79be780` |
| Commit date | 2026-11-05 |
| License | MIT OR Apache-2.0 |
| Copyright | Copyright (c) 2025 Ignacio Perez, see `LICENSE-MIT` |

The first commit of this repository is that upstream tree without `.github/`, plus this file and one `.gitignore` line. It does not carry the upstream history.

## Keep

- Keep `LICENSE-MIT` and `LICENSE-APACHE`.
- Keep this file and the upstream copyright notices, including the license files under `vendor/`.

## Name and logo

The upstream [`TRADEMARK.md`](https://github.com/0xErwin1/dbflux/blob/76b46a00378627dc6fa6f3dda2d54191f79be780/TRADEMARK.md) says that the DBFlux name, logo and domain are not covered by the code license. A modified build must use a different name and logo. An edited or recolored upstream logo is not allowed either.

## Taking changes from upstream

DataTab renames the whole repository, so upstream changes cannot be merged. Choose each change, then apply it by hand or with `git cherry-pick`, and record the upstream commit in the commit message.
