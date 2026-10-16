# Embedded WASI WIT sources

These files are the direct-child `.wit` sources from the `wit/` directory of
the pinned WASI 0.2.3 repositories below. Files under upstream `wit/deps/` are
not included. Trailing horizontal whitespace was removed from the vendored
text; no WIT tokens or line content were changed.

| Package | Upstream revision | Files |
| --- | --- | ---: |
| `wasi:cli` | [`d4fddec89fb9354509dbfa29a5557c58983f327a`](https://github.com/WebAssembly/wasi-cli/tree/d4fddec89fb9354509dbfa29a5557c58983f327a) | 7 |
| `wasi:clocks` | [`e9ef266c368fd7bfb3442e6cc9e7a546d4b029ff`](https://github.com/WebAssembly/wasi-clocks/tree/e9ef266c368fd7bfb3442e6cc9e7a546d4b029ff) | 4 |
| `wasi:filesystem` | [`70096e90b70ca876a9656bfebd59e3dd8686e661`](https://github.com/WebAssembly/wasi-filesystem/tree/70096e90b70ca876a9656bfebd59e3dd8686e661) | 3 |
| `wasi:http` | [`3c1734c55239ccb700a1f61443047ab4cfa5deb5`](https://github.com/WebAssembly/wasi-http/tree/3c1734c55239ccb700a1f61443047ab4cfa5deb5) | 3 |
| `wasi:io` | [`f73bfb43c3b31d0afab7167c7f7f0515bebba704`](https://github.com/WebAssembly/wasi-io/tree/f73bfb43c3b31d0afab7167c7f7f0515bebba704) | 4 |
| `wasi:random` | [`bf93507b67b3d1e03bb9330cdaf71aafe432557f`](https://github.com/WebAssembly/wasi-random/tree/bf93507b67b3d1e03bb9330cdaf71aafe432557f) | 4 |
| `wasi:sockets` | [`a584c2bfb4e8e4f30f7ba7a49b09b3ab2cd08730`](https://github.com/WebAssembly/wasi-sockets/tree/a584c2bfb4e8e4f30f7ba7a49b09b3ab2cd08730) | 8 |

At the pinned revisions, the six repositories other than `wasi-clocks` include
a `LICENSE.md` notice. Five of them use the following copyright years:

> Copyright © 2019-2024 the Contributors to the WASI Specification, published
> by the WebAssembly Community Group under the W3C Community Contributor
> License Agreement (CLA).

The `wasi-io` notice uses `2019-2023` in that copyright line. The other five
use `2019-2024`.

The upstream notice links to the [WebAssembly Community Group](https://www.w3.org/community/webassembly/),
the [W3C Community Contributor License Agreement](https://www.w3.org/community/about/agreements/cla/),
and its [human-readable summary](https://www.w3.org/community/about/agreements/cla-deed/).
This attribution is recorded here because the WIT files are vendored into this
repository. **Review required:** confirm the upstream terms are sufficient for
redistributing these embedded sources.

The pinned `wasi-clocks` revision has no `LICENSE.md` file. Its repository and
commit are recorded above, but no license is inferred for those four files.
**Review required:** determine the appropriate treatment of the `wasi-clocks`
sources before redistribution.
