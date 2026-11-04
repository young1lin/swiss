# Third-party notices

swiss is licensed under the Apache License 2.0 (see [LICENSE](LICENSE)). It ships with, links
against, or reproduces the third-party material listed here, each under its own terms. Nothing
in this file changes those terms; it exists so that every notice reaches everyone who receives
a copy of swiss — source or binary.

The panel's engine badges (MySQL, MariaDB, Redis, PostgreSQL, MongoDB, Figma, Docker) are
redrawn monochrome from Simple Icons (CC0) and used nominatively; the names and marks belong
to their respective owners.

## 1. Vendored into the panel (served by the binary)

The admin panel is plain ES modules embedded in the executable. Six upstream projects are
vendored byte-identical under `crates/swiss-panel/src/admin_assets/js/vendor/`; each directory
carries the upstream license text, which is embedded and shipped with the copies.

### xterm.js 5.5.0 and addons — MIT

`@xterm/xterm` 5.5.0, `@xterm/addon-fit` 0.10.0, `@xterm/addon-search` 0.16.0,
`@xterm/addon-unicode11` 0.8.0, `@xterm/addon-web-links` 0.11.0, `@xterm/addon-webgl` 0.18.0.
https://github.com/xtermjs/xterm.js — license text: `js/vendor/xterm/LICENSE`.

    Copyright (c) 2017-2019, The xterm.js authors (https://github.com/xtermjs/xterm.js)
    Copyright (c) 2014-2016, SourceLair Private Company (https://www.sourcelair.com)
    Copyright (c) 2012-2013, Christopher Jeffrey (https://github.com/chjj/)

### cronstrue 2.52.0 — MIT

`cronstrue` 2.52.0 (`dist/cronstrue-i18n.js`). https://github.com/bradymholt/cronstrue —
license text: `js/vendor/cronstrue/LICENSE`.

    Copyright (c) 2017 Brady Holt

### shlex 3.0.0 — MIT

`shlex` 3.0.0 (`shlex.js`), the Redis console's command-line splitter.
https://github.com/rgov/node-shlex — license text: `js/vendor/shlex/LICENSE`.

    Copyright (c) 2018 Ryan Govostes

### cytoscape 3.34.3 — MIT

`cytoscape` 3.34.3 (`dist/cytoscape.min.js`), the memory page's graph renderer.
https://github.com/cytoscape/cytoscape.js — license text: `js/vendor/cytoscape/LICENSE`.

    Copyright (c) 2016-2026, The Cytoscape Consortium.

### markdown-it 15.0.2 — MIT

`markdown-it` 15.0.2 (`dist/browser/markdown-it.esm.min.mjs`), every Markdown the panel draws.
https://github.com/markdown-it/markdown-it — license text: `js/vendor/markdown-it/LICENSE`.

    Copyright (c) 2014 Vitaly Puzrin, Alex Kocharin.

The bundle carries its own dependencies, each under its license:

- `entities` 8 — BSD-2-Clause, https://github.com/fb55/entities. Copyright (c) Felix Böhm.
  Redistribution and use in source and binary forms, with or without modification, are
  permitted provided that the copyright notice, this list of conditions and the disclaimer
  in the package's `LICENSE` are retained in source and reproduced in binary distributions.
- `linkify-it` 6 — MIT, https://github.com/markdown-it/linkify-it. Copyright (c) 2015 Vitaly Puzrin.
- `mdurl` 2 — MIT, https://github.com/markdown-it/mdurl. Copyright (c) 2015 Vitaly Puzrin,
  Alex Kocharin; its `.parse()` is based on Node.js `url` code, Copyright Joyent, Inc. and
  other Node contributors.
- `punycode.js` 2 and `uc.micro` 3 — MIT, https://github.com/mathiasbynens/punycode.js and
  https://github.com/markdown-it/uc.micro. Copyright Mathias Bynens.

### mermaid 12.1.0 (the `@mermaid-js/tiny` build) — MIT

`@mermaid-js/tiny` 12.1.0 (`dist/mermaid.tiny.js`), the memory page's diagram renderer. It is
mermaid's own build without the mindmap and architecture diagrams, KaTeX and the Eclipse ELK
layout engine. The full build bundles ELK under EPL-2.0, and the panel does not ship that.
https://github.com/mermaid-js/mermaid — license text: `js/vendor/mermaid/LICENSE`.

    Copyright (c) 2014 - 2022 Knut Sveidqvist

The bundle carries its own dependencies, each under its license:

- MIT: `@braintree/sanitize-url`, `@iconify/utils`, `@mermaid-js/parser`, `@upsetjs/venn.js`,
  `dagre-d3-es`, `dayjs`, `es-toolkit`, `khroma`, `langium`, `lodash-es`, `marked`, `roughjs`,
  `stylis`, `ts-dedent`, `vscode-jsonrpc`, `vscode-languageserver`, `vscode-languageserver-types`,
  `vscode-uri`. Each package's `LICENSE` names its copyright holders.
- `d3` 7 — ISC, https://github.com/d3/d3. Copyright 2010-2023 Mike Bostock.
- `d3-sankey` 0.12 — BSD-3-Clause, https://github.com/d3/d3-sankey. Copyright 2015, Mike
  Bostock. Redistribution and use in source and binary forms, with or without modification,
  are permitted provided that the copyright notice, the conditions and the disclaimer in the
  package's `LICENSE` are retained, and that neither the name of the author nor the names of
  contributors are used to endorse or promote derived products without prior written
  permission.
- `chevrotain` 13 — Apache-2.0, https://github.com/Chevrotain/chevrotain.
- `dompurify` 3.4.12 — Apache-2.0 OR MPL-2.0 (used here under Apache-2.0),
  https://github.com/cure53/DOMPurify. (c) Cure53 and other contributors.
- `lodash-es` also carries Underscore.js: Copyright OpenJS Foundation and other contributors;
  Copyright Jeremy Ashkenas, DocumentCloud and Investigative Reporters & Editors.

### @bufbuild/protobuf 2.16.0 — Apache-2.0 AND BSD-3-Clause

The event-bus runtime (`crates/swiss-panel/src/admin_assets/js/vendor/protobuf/2.16.0/`),
the import closure of `@bufbuild/protobuf` 2.16.0 `dist/esm` that the generated codecs and
the bus client use. The npm tarball ships no separate LICENSE file; each vendored file
carries its own header, copied byte-identical.
https://github.com/bufbuild/protobuf-es (package `packages/protobuf`).

- Apache-2.0, Copyright 2021-2026 Buf Technologies, Inc. — every file in the closure except
  `wire/varint.js`.
- BSD-3-Clause, Copyright 2008 Google Inc. — `wire/varint.js`.

## 2. Reproduced in source

### Lucide icon path data — ISC

Several glyphs in the panel's icon sprite (`crates/swiss-panel/src/admin_assets/index.html`)
use 24x24 path data copied verbatim from Lucide (https://lucide.dev), in the Lucide idiom the
sprite follows. The full ISC text:

    Copyright (c) Lucide Icons and Contributors

    Permission to use, copy, modify, and/or distribute this software for any purpose with or
    without fee is hereby granted, provided that the above copyright notice and this
    permission notice appear in all copies.

    THE SOFTWARE IS PROVIDED "AS IS" AND THE AUTHOR DISCLAIMS ALL WARRANTIES WITH REGARD TO
    THIS SOFTWARE INCLUDING ALL IMPLIED WARRANTIES OF MERCHANTABILITY AND FITNESS. IN NO
    EVENT SHALL THE AUTHOR BE LIABLE FOR ANY SPECIAL, DIRECT, INDIRECT, OR CONSEQUENTIAL
    DAMAGES OR ANY DAMAGES WHATSOEVER RESULTING FROM LOSS OF USE, DATA OR PROFITS, WHETHER IN
    AN ACTION OF CONTRACT, NEGLIGENCE OR OTHER TORTIOUS ACTION, ARISING OUT OF OR IN
    CONNECTION WITH THE USE OR PERFORMANCE OF THIS SOFTWARE.

### @z_ai/mcp-server 0.1.5 — Apache-2.0

The zai-vision adapter (`crates/swiss-mcp/src/adapters/zai.rs`) is a native port of the
`@z_ai/mcp-server` npm package by Z.AI (https://www.npmjs.com/package/@z_ai/mcp-server,
https://docs.z.ai/). Its tool names, descriptions, JSON schemas and — verbatim — its system
prompts (`crates/swiss-mcp/src/adapters/zai_prompts.rs`) are reproduced from version 0.1.5
under the Apache License, Version 2.0.

    Copyright Z.AI. Licensed under the Apache License, Version 2.0.

### agent-browser skill — Apache-2.0

`.agents/skills/agent-browser/SKILL.md` — the agent instructions the live-verification
workflow loads before driving the panel in a real browser — is a condensed derivative of the
skill file shipped with `agent-browser` by Vercel Labs
(https://github.com/vercel-labs/agent-browser, `skill-data/core/SKILL.md`), under the Apache
License, Version 2.0. The tool itself is not vendored: the skill installs it from npm.

    Copyright Vercel, Inc. Licensed under the Apache License, Version 2.0.

## 3. Rust crates linked into the binary

The release binary statically links the crates below: every crate that reaches the `swiss`
package through a normal or build edge on any release target, for the committed `Cargo.lock`
(dev-only dependencies are excluded). Each crate's full license text is available in its source
repository and in the crate tarball on crates.io. The `cargo deny` gate checks the licenses of
this same graph against the `deny.toml` allowlist on every CI run; the table itself is not
checked, so regenerate it whenever `Cargo.lock` changes.

Regenerate: the crate set is `cargo tree -p swiss -e normal,build --target all --prefix none
--locked` minus the workspace's own `swiss*` crates; `license` and `repository` (else
crates.io) come from `cargo metadata --format-version 1 --locked`, a repository's trailing `/`
or `.git` dropped; rows are sorted by name, then by semver.

| Crate | Version | License | Source |
|---|---|---|---|
| adler2 | 2.0.1 | 0BSD OR MIT OR Apache-2.0 | https://github.com/oyvindln/adler2 |
| aead | 0.6.1 | MIT OR Apache-2.0 | https://github.com/RustCrypto/traits |
| aes | 0.9.3 | MIT OR Apache-2.0 | https://github.com/RustCrypto/block-ciphers |
| aes-gcm | 0.11.1 | Apache-2.0 OR MIT | https://github.com/RustCrypto/AEADs |
| ahash | 0.8.12 | MIT OR Apache-2.0 | https://github.com/tkaitchuck/ahash |
| aho-corasick | 1.1.5 | Unlicense OR MIT | https://github.com/BurntSushi/aho-corasick |
| allocator-api2 | 0.2.21 | MIT OR Apache-2.0 | https://github.com/zakarumych/allocator-api2 |
| android_system_properties | 0.1.6 | MIT OR Apache-2.0 | https://github.com/nical/android_system_properties |
| anyhow | 1.0.104 | MIT OR Apache-2.0 | https://github.com/dtolnay/anyhow |
| arc-swap | 1.9.2 | MIT OR Apache-2.0 | https://github.com/vorner/arc-swap |
| arcstr | 1.2.0 | Apache-2.0 OR MIT OR Zlib | https://github.com/thomcc/arcstr |
| argon2 | 0.6.0 | MIT OR Apache-2.0 | https://github.com/RustCrypto/password-hashes |
| async-compression | 0.4.50 | MIT OR Apache-2.0 | https://github.com/Nullus157/async-compression |
| async-lock | 3.4.2 | Apache-2.0 OR MIT | https://github.com/smol-rs/async-lock |
| async-trait | 0.1.92 | MIT OR Apache-2.0 | https://github.com/dtolnay/async-trait |
| atk | 0.18.2 | MIT | https://github.com/gtk-rs/gtk3-rs |
| atk-sys | 0.18.2 | MIT | https://github.com/gtk-rs/gtk3-rs |
| atoi | 2.0.0 | MIT | https://github.com/pacman82/atoi-rs |
| atomic-waker | 1.1.2 | Apache-2.0 OR MIT | https://github.com/smol-rs/atomic-waker |
| autocfg | 1.5.1 | Apache-2.0 OR MIT | https://github.com/cuviper/autocfg |
| aws-lc-rs | 1.18.1 | ISC AND (Apache-2.0 OR ISC) | https://github.com/aws/aws-lc-rs |
| aws-lc-sys | 0.45.0 | ISC AND (Apache-2.0 OR ISC) AND Apache-2.0 AND MIT AND BSD-3-Clause AND (Apache-2.0 OR ISC OR MIT) AND (Apache-2.0 OR ISC OR MIT-0) | https://github.com/aws/aws-lc-rs |
| axum | 0.8.9 | MIT | https://github.com/tokio-rs/axum |
| axum-core | 0.5.6 | MIT | https://github.com/tokio-rs/axum |
| backon | 1.6.0 | Apache-2.0 | https://github.com/Xuanwo/backon |
| base16ct | 1.0.0 | Apache-2.0 OR MIT | https://github.com/RustCrypto/formats |
| base64 | 0.22.1 | MIT OR Apache-2.0 | https://github.com/marshallpierce/rust-base64 |
| base64 | 0.23.1 | MIT OR Apache-2.0 | https://github.com/marshallpierce/rust-base64 |
| base64ct | 1.8.3 | Apache-2.0 OR MIT | https://github.com/RustCrypto/formats |
| bcrypt-pbkdf | 0.11.0 | MIT OR Apache-2.0 | https://github.com/RustCrypto/password-hashes |
| bigdecimal | 0.4.11 | MIT/Apache-2.0 | https://github.com/akubera/bigdecimal-rs |
| bit-set | 0.8.0 | Apache-2.0 OR MIT | https://github.com/contain-rs/bit-set |
| bit-vec | 0.8.0 | Apache-2.0 OR MIT | https://github.com/contain-rs/bit-vec |
| bitflags | 1.3.2 | MIT/Apache-2.0 | https://github.com/bitflags/bitflags |
| bitflags | 2.13.1 | MIT OR Apache-2.0 | https://github.com/bitflags/bitflags |
| bitvec | 1.1.1 | MIT | https://github.com/bitvecto-rs/bitvec |
| blake2 | 0.11.0 | MIT OR Apache-2.0 | https://github.com/RustCrypto/hashes |
| block-buffer | 0.10.4 | MIT OR Apache-2.0 | https://github.com/RustCrypto/utils |
| block-buffer | 0.12.1 | MIT OR Apache-2.0 | https://github.com/RustCrypto/utils |
| block-padding | 0.4.2 | MIT OR Apache-2.0 | https://github.com/RustCrypto/utils |
| block2 | 0.6.2 | MIT | https://github.com/madsmtm/objc2 |
| blowfish | 0.10.0 | MIT OR Apache-2.0 | https://github.com/RustCrypto/block-ciphers |
| bson | 3.1.0 | MIT | https://github.com/mongodb/bson-rust |
| bumpalo | 3.20.3 | MIT OR Apache-2.0 | https://github.com/fitzgen/bumpalo |
| byteorder | 1.5.0 | Unlicense OR MIT | https://github.com/BurntSushi/byteorder |
| bytes | 1.12.1 | MIT | https://github.com/tokio-rs/bytes |
| cairo-rs | 0.18.5 | MIT | https://github.com/gtk-rs/gtk-rs-core |
| cairo-sys-rs | 0.18.2 | MIT | https://github.com/gtk-rs/gtk-rs-core |
| cbc | 0.2.1 | MIT OR Apache-2.0 | https://github.com/RustCrypto/block-modes |
| cc | 1.4.5 | MIT OR Apache-2.0 | https://github.com/rust-lang/cc-rs |
| cesu8 | 1.1.0 | Apache-2.0/MIT | https://github.com/emk/cesu8-rs |
| cfg_aliases | 0.2.2 | MIT | https://github.com/katharostech/cfg_aliases |
| cfg-expr | 0.15.8 | MIT OR Apache-2.0 | https://github.com/EmbarkStudios/cfg-expr |
| cfg-if | 1.0.4 | MIT OR Apache-2.0 | https://github.com/rust-lang/cfg-if |
| chacha20 | 0.10.2 | MIT OR Apache-2.0 | https://github.com/RustCrypto/stream-ciphers |
| chrono | 0.4.45 | MIT OR Apache-2.0 | https://github.com/chronotope/chrono |
| cipher | 0.5.2 | MIT OR Apache-2.0 | https://github.com/RustCrypto/traits |
| cmake | 0.1.58 | MIT OR Apache-2.0 | https://github.com/rust-lang/cmake-rs |
| cmov | 0.5.4 | Apache-2.0 OR MIT | https://github.com/RustCrypto/utils |
| combine | 4.6.8 | MIT | https://github.com/Marwes/combine |
| compression-codecs | 0.4.45 | MIT OR Apache-2.0 | https://github.com/Nullus157/async-compression |
| compression-core | 0.4.33 | MIT OR Apache-2.0 | https://github.com/Nullus157/async-compression |
| const-oid | 0.10.2 | Apache-2.0 OR MIT | https://github.com/RustCrypto/formats |
| const-random | 0.1.18 | MIT OR Apache-2.0 | https://github.com/tkaitchuck/constrandom |
| const-random-macro | 0.1.16 | MIT OR Apache-2.0 | https://github.com/tkaitchuck/constrandom |
| convert_case | 0.10.0 | MIT | https://github.com/rutrum/convert-case |
| cookie | 0.18.2 | MIT OR Apache-2.0 | https://github.com/SergioBenitez/cookie-rs |
| core_detect | 1.0.0 | MIT/Apache-2.0 | https://github.com/thomcc/core_detect |
| core-foundation | 0.9.4 | MIT OR Apache-2.0 | https://github.com/servo/core-foundation-rs |
| core-foundation | 0.10.1 | MIT OR Apache-2.0 | https://github.com/servo/core-foundation-rs |
| core-foundation-sys | 0.8.7 | MIT OR Apache-2.0 | https://github.com/servo/core-foundation-rs |
| core-graphics | 0.25.0 | MIT OR Apache-2.0 | https://github.com/servo/core-foundation-rs |
| core-graphics-types | 0.2.0 | MIT OR Apache-2.0 | https://github.com/servo/core-foundation-rs |
| cpubits | 0.1.1 | MIT OR Apache-2.0 | https://github.com/RustCrypto/utils |
| cpufeatures | 0.2.17 | MIT OR Apache-2.0 | https://github.com/RustCrypto/utils |
| cpufeatures | 0.3.1 | MIT OR Apache-2.0 | https://github.com/RustCrypto/utils |
| crc | 3.4.0 | MIT OR Apache-2.0 | https://github.com/mrhooray/crc-rs |
| crc-catalog | 2.5.0 | MIT OR Apache-2.0 | https://github.com/akhilles/crc-catalog |
| crc32fast | 1.5.2 | MIT OR Apache-2.0 | https://github.com/srijs/rust-crc32fast |
| crossbeam-channel | 0.5.17 | MIT OR Apache-2.0 | https://github.com/crossbeam-rs/crossbeam |
| crossbeam-queue | 0.3.14 | MIT OR Apache-2.0 | https://github.com/crossbeam-rs/crossbeam |
| crossbeam-utils | 0.8.23 | MIT OR Apache-2.0 | https://github.com/crossbeam-rs/crossbeam |
| crunchy | 0.2.4 | MIT | https://github.com/eira-fransham/crunchy |
| crypto-bigint | 0.7.5 | Apache-2.0 OR MIT | https://github.com/RustCrypto/crypto-bigint |
| crypto-common | 0.1.7 | MIT OR Apache-2.0 | https://github.com/RustCrypto/traits |
| crypto-common | 0.2.2 | MIT OR Apache-2.0 | https://github.com/RustCrypto/traits |
| crypto-primes | 0.7.2 | Apache-2.0 OR MIT | https://github.com/entropyxyz/crypto-primes |
| cssparser | 0.37.0 | MPL-2.0 | https://github.com/servo/rust-cssparser |
| cssparser-macros | 0.7.1 | MPL-2.0 | https://github.com/servo/rust-cssparser |
| ctr | 0.10.1 | MIT OR Apache-2.0 | https://github.com/RustCrypto/block-modes |
| ctutils | 0.4.2 | Apache-2.0 OR MIT | https://github.com/RustCrypto/utils |
| curve25519-dalek | 5.0.0 | BSD-3-Clause | https://github.com/dalek-cryptography/curve25519-dalek/tree/main/curve25519-dalek |
| curve25519-dalek-derive | 0.1.1 | MIT/Apache-2.0 | https://github.com/dalek-cryptography/curve25519-dalek |
| darling | 0.24.1 | MIT | https://github.com/TedDriggs/darling |
| darling_core | 0.24.1 | MIT | https://github.com/TedDriggs/darling |
| darling_macro | 0.24.1 | MIT | https://github.com/TedDriggs/darling |
| dashmap | 6.2.1 | MIT | https://github.com/xacrimon/dashmap |
| data-encoding | 2.11.1 | MIT | https://github.com/ia0/data-encoding |
| delegate | 0.13.5 | MIT OR Apache-2.0 | https://github.com/kobzol/rust-delegate |
| der | 0.8.2 | Apache-2.0 OR MIT | https://github.com/RustCrypto/formats |
| deranged | 0.5.8 | MIT OR Apache-2.0 | https://github.com/jhpratt/deranged |
| derive_more | 2.1.1 | MIT | https://github.com/JelteF/derive_more |
| derive_more-impl | 2.1.1 | MIT | https://github.com/JelteF/derive_more |
| derive-syn-parse | 0.2.0 | MIT OR Apache-2.0 | https://github.com/sharnoff/derive-syn-parse |
| derive-where | 1.7.0 | MIT OR Apache-2.0 | https://github.com/ModProg/derive-where |
| des | 0.9.0 | MIT OR Apache-2.0 | https://github.com/RustCrypto/block-ciphers |
| digest | 0.10.7 | MIT OR Apache-2.0 | https://github.com/RustCrypto/traits |
| digest | 0.11.3 | MIT OR Apache-2.0 | https://github.com/RustCrypto/traits |
| dirs | 6.0.0 | MIT OR Apache-2.0 | https://github.com/soc/dirs-rs |
| dirs | 7.0.0 | MIT OR Apache-2.0 | https://codeberg.org/dirs/dirs-rs |
| dirs-sys | 0.5.0 | MIT OR Apache-2.0 | https://github.com/dirs-dev/dirs-sys-rs |
| dispatch2 | 0.3.1 | Zlib OR Apache-2.0 OR MIT | https://github.com/madsmtm/objc2 |
| displaydoc | 0.2.7 | MIT OR Apache-2.0 | https://github.com/yaahc/displaydoc |
| dlopen2 | 0.8.2 | MIT | https://github.com/OpenByteDev/dlopen2 |
| dlopen2_derive | 0.4.3 | MIT | https://github.com/OpenByteDev/dlopen2 |
| dom_query | 0.28.0 | MIT | https://github.com/niklak/dom_query |
| dotenvy | 0.15.7 | MIT | https://github.com/allan2/dotenvy |
| dpi | 0.1.2 | Apache-2.0 AND MIT | https://github.com/rust-windowing/winit |
| dtoa | 1.0.11 | MIT OR Apache-2.0 | https://github.com/dtolnay/dtoa |
| dtoa-short | 0.3.5 | MPL-2.0 | https://github.com/upsuper/dtoa-short |
| dunce | 1.0.5 | CC0-1.0 OR MIT-0 OR Apache-2.0 | https://gitlab.com/kornelski/dunce |
| dyn-clone | 1.0.20 | MIT OR Apache-2.0 | https://github.com/dtolnay/dyn-clone |
| ecdsa | 0.17.0 | Apache-2.0 OR MIT | https://github.com/RustCrypto/signatures |
| ed25519 | 3.0.0 | Apache-2.0 OR MIT | https://github.com/RustCrypto/signatures |
| ed25519-dalek | 3.0.0 | BSD-3-Clause | https://github.com/dalek-cryptography/curve25519-dalek/tree/main/ed25519-dalek |
| either | 1.18.0 | MIT OR Apache-2.0 | https://github.com/rayon-rs/either |
| elliptic-curve | 0.14.1 | Apache-2.0 OR MIT | https://github.com/RustCrypto/traits |
| encoding_rs | 0.8.42 | (Apache-2.0 OR MIT) AND BSD-3-Clause | https://github.com/hsivonen/encoding_rs |
| enum_dispatch | 0.3.13 | MIT OR Apache-2.0 | https://gitlab.com/antonok/enum_dispatch |
| equivalent | 1.0.2 | Apache-2.0 OR MIT | https://github.com/indexmap-rs/equivalent |
| errno | 0.3.14 | MIT OR Apache-2.0 | https://github.com/lambda-fairy/rust-errno |
| etcetera | 0.11.0 | MIT OR Apache-2.0 | https://github.com/lunacookies/etcetera |
| event-listener | 5.4.2 | Apache-2.0 OR MIT | https://github.com/smol-rs/event-listener |
| event-listener-strategy | 0.5.4 | Apache-2.0 OR MIT | https://github.com/smol-rs/event-listener-strategy |
| eventsource-stream | 0.2.3 | MIT OR Apache-2.0 | https://github.com/jpopesculian/eventsource-stream |
| fastrand | 2.5.0 | Apache-2.0 OR MIT | https://github.com/smol-rs/fastrand |
| fdeflate | 0.3.7 | MIT OR Apache-2.0 | https://github.com/image-rs/fdeflate |
| ff | 0.14.0 | MIT/Apache-2.0 | https://github.com/zkcrypto/ff |
| fiat-crypto | 0.3.0 | MIT OR Apache-2.0 OR BSD-1-Clause | https://github.com/mit-plv/fiat-crypto |
| field-offset | 0.3.6 | MIT OR Apache-2.0 | https://github.com/Diggsey/rust-field-offset |
| find-msvc-tools | 0.1.12 | MIT OR Apache-2.0 | https://github.com/rust-lang/cc-rs |
| flate2 | 1.1.10 | MIT OR Apache-2.0 | https://github.com/rust-lang/flate2-rs |
| flume | 0.12.0 | Apache-2.0/MIT | https://github.com/zesterer/flume |
| fnv | 1.0.7 | Apache-2.0 / MIT | https://github.com/servo/rust-fnv |
| foldhash | 0.2.0 | Zlib | https://github.com/orlp/foldhash |
| foreign-types | 0.3.2 | MIT/Apache-2.0 | https://github.com/sfackler/foreign-types |
| foreign-types | 0.5.0 | MIT/Apache-2.0 | https://github.com/sfackler/foreign-types |
| foreign-types-macros | 0.2.4 | MIT/Apache-2.0 | https://github.com/sfackler/foreign-types |
| foreign-types-shared | 0.1.1 | MIT/Apache-2.0 | https://github.com/sfackler/foreign-types |
| foreign-types-shared | 0.3.1 | MIT/Apache-2.0 | https://github.com/sfackler/foreign-types |
| form_urlencoded | 1.2.2 | MIT OR Apache-2.0 | https://github.com/servo/rust-url |
| fs_extra | 1.3.0 | MIT | https://github.com/webdesus/fs_extra |
| funty | 2.0.0 | MIT | https://github.com/myrrlyn/funty |
| futures | 0.3.34 | MIT OR Apache-2.0 | https://github.com/rust-lang/futures-rs |
| futures-channel | 0.3.34 | MIT OR Apache-2.0 | https://github.com/rust-lang/futures-rs |
| futures-core | 0.3.34 | MIT OR Apache-2.0 | https://github.com/rust-lang/futures-rs |
| futures-executor | 0.3.34 | MIT OR Apache-2.0 | https://github.com/rust-lang/futures-rs |
| futures-intrusive | 0.5.0 | MIT OR Apache-2.0 | https://github.com/Matthias247/futures-intrusive |
| futures-io | 0.3.34 | MIT OR Apache-2.0 | https://github.com/rust-lang/futures-rs |
| futures-macro | 0.3.34 | MIT OR Apache-2.0 | https://github.com/rust-lang/futures-rs |
| futures-sink | 0.3.34 | MIT OR Apache-2.0 | https://github.com/rust-lang/futures-rs |
| futures-task | 0.3.34 | MIT OR Apache-2.0 | https://github.com/rust-lang/futures-rs |
| futures-util | 0.3.34 | MIT OR Apache-2.0 | https://github.com/rust-lang/futures-rs |
| gdk | 0.18.2 | MIT | https://github.com/gtk-rs/gtk3-rs |
| gdk-pixbuf | 0.18.5 | MIT | https://github.com/gtk-rs/gtk-rs-core |
| gdk-pixbuf-sys | 0.18.0 | MIT | https://github.com/gtk-rs/gtk-rs-core |
| gdk-sys | 0.18.2 | MIT | https://github.com/gtk-rs/gtk3-rs |
| gdkwayland-sys | 0.18.2 | MIT | https://github.com/gtk-rs/gtk3-rs |
| genai | 0.6.5 | MIT OR Apache-2.0 | https://github.com/jeremychone/rust-genai |
| generic-array | 0.14.7 | MIT | https://github.com/fizyk20/generic-array |
| generic-array | 1.4.5 | MIT | https://github.com/fizyk20/generic-array |
| getrandom | 0.2.17 | MIT OR Apache-2.0 | https://github.com/rust-random/getrandom |
| getrandom | 0.3.4 | MIT OR Apache-2.0 | https://github.com/rust-random/getrandom |
| getrandom | 0.4.3 | MIT OR Apache-2.0 | https://github.com/rust-random/getrandom |
| ghash | 0.6.0 | Apache-2.0 OR MIT | https://github.com/RustCrypto/universal-hashes |
| gio | 0.18.4 | MIT | https://github.com/gtk-rs/gtk-rs-core |
| gio-sys | 0.18.1 | MIT | https://github.com/gtk-rs/gtk-rs-core |
| glib | 0.18.5 | MIT | https://github.com/gtk-rs/gtk-rs-core |
| glib-macros | 0.18.5 | MIT | https://github.com/gtk-rs/gtk-rs-core |
| glib-sys | 0.18.1 | MIT | https://github.com/gtk-rs/gtk-rs-core |
| gloo-timers | 0.4.0 | MIT OR Apache-2.0 | https://github.com/rustwasm/gloo/tree/master/crates/timers |
| gobject-sys | 0.18.0 | MIT | https://github.com/gtk-rs/gtk-rs-core |
| group | 0.14.0 | MIT/Apache-2.0 | https://github.com/zkcrypto/group |
| gtk | 0.18.2 | MIT | https://github.com/gtk-rs/gtk3-rs |
| gtk-sys | 0.18.2 | MIT | https://github.com/gtk-rs/gtk3-rs |
| gtk3-macros | 0.18.2 | MIT | https://github.com/gtk-rs/gtk3-rs |
| h2 | 0.4.19 | MIT | https://github.com/hyperium/h2 |
| hashbrown | 0.14.5 | MIT OR Apache-2.0 | https://github.com/rust-lang/hashbrown |
| hashbrown | 0.16.1 | MIT OR Apache-2.0 | https://github.com/rust-lang/hashbrown |
| hashbrown | 0.17.1 | MIT OR Apache-2.0 | https://github.com/rust-lang/hashbrown |
| hashlink | 0.11.1 | MIT OR Apache-2.0 | https://github.com/djc/hashlink |
| heck | 0.4.1 | MIT OR Apache-2.0 | https://github.com/withoutboats/heck |
| heck | 0.5.0 | MIT OR Apache-2.0 | https://github.com/withoutboats/heck |
| hex | 0.4.3 | MIT OR Apache-2.0 | https://github.com/KokaKiwi/rust-hex |
| hex-literal | 1.1.0 | MIT OR Apache-2.0 | https://github.com/RustCrypto/utils |
| hkdf | 0.13.0 | MIT OR Apache-2.0 | https://github.com/RustCrypto/KDFs |
| hmac | 0.13.0 | MIT OR Apache-2.0 | https://github.com/RustCrypto/MACs |
| html5ever | 0.39.0 | MIT OR Apache-2.0 | https://github.com/servo/html5ever |
| http | 1.5.0 | MIT OR Apache-2.0 | https://github.com/hyperium/http |
| http-body | 1.1.0 | MIT | https://github.com/hyperium/http-body |
| http-body-util | 0.1.5 | MIT | https://github.com/hyperium/http-body |
| httparse | 1.10.1 | MIT OR Apache-2.0 | https://github.com/seanmonstar/httparse |
| httpdate | 1.0.3 | MIT OR Apache-2.0 | https://github.com/pyfisch/httpdate |
| hybrid-array | 0.4.14 | MIT OR Apache-2.0 | https://github.com/RustCrypto/hybrid-array |
| hyper | 1.11.1 | MIT | https://github.com/hyperium/hyper |
| hyper-rustls | 0.27.9 | Apache-2.0 OR ISC OR MIT | https://github.com/rustls/hyper-rustls |
| hyper-tls | 0.6.0 | MIT/Apache-2.0 | https://github.com/hyperium/hyper-tls |
| hyper-util | 0.1.20 | MIT | https://github.com/hyperium/hyper-util |
| iana-time-zone | 0.1.65 | MIT OR Apache-2.0 | https://github.com/strawlab/iana-time-zone |
| iana-time-zone-haiku | 0.1.2 | MIT OR Apache-2.0 | https://github.com/strawlab/iana-time-zone |
| icu_collections | 2.3.0 | Unicode-3.0 | https://github.com/unicode-org/icu4x |
| icu_locale_core | 2.3.0 | Unicode-3.0 | https://github.com/unicode-org/icu4x |
| icu_normalizer | 2.3.0 | Unicode-3.0 | https://github.com/unicode-org/icu4x |
| icu_normalizer_data | 2.3.0 | Unicode-3.0 | https://github.com/unicode-org/icu4x |
| icu_properties | 2.3.0 | Unicode-3.0 | https://github.com/unicode-org/icu4x |
| icu_properties_data | 2.3.0 | Unicode-3.0 | https://github.com/unicode-org/icu4x |
| icu_provider | 2.3.1 | Unicode-3.0 | https://github.com/unicode-org/icu4x |
| ident_case | 1.0.1 | MIT/Apache-2.0 | https://github.com/TedDriggs/ident_case |
| idna | 1.1.0 | MIT OR Apache-2.0 | https://github.com/servo/rust-url |
| idna_adapter | 1.2.2 | Apache-2.0 OR MIT | https://github.com/hsivonen/idna_adapter |
| indexmap | 2.14.2 | Apache-2.0 OR MIT | https://github.com/indexmap-rs/indexmap |
| inout | 0.2.2 | MIT OR Apache-2.0 | https://github.com/RustCrypto/utils |
| ipnet | 2.12.2 | MIT OR Apache-2.0 | https://github.com/krisprice/ipnet |
| itertools | 0.14.0 | MIT OR Apache-2.0 | https://github.com/rust-itertools/itertools |
| itoa | 1.0.18 | MIT OR Apache-2.0 | https://github.com/dtolnay/itoa |
| javascriptcore-rs | 1.1.2 | MIT | https://github.com/tauri-apps/javascriptcore-rs |
| javascriptcore-rs-sys | 1.1.1 | MIT | https://github.com/tauri-apps/javascriptcore-rs |
| jni | 0.21.1 | MIT/Apache-2.0 | https://github.com/jni-rs/jni-rs |
| jni | 0.22.4 | MIT OR Apache-2.0 | https://github.com/jni-rs/jni-rs |
| jni-macros | 0.22.4 | MIT OR Apache-2.0 | https://github.com/jni-rs/jni-rs |
| jni-sys | 0.3.1 | MIT OR Apache-2.0 | https://github.com/jni-rs/jni-sys |
| jni-sys | 0.4.1 | MIT OR Apache-2.0 | https://github.com/jni-rs/jni-sys |
| jni-sys-macros | 0.4.1 | MIT OR Apache-2.0 | https://github.com/jni-rs/jni-sys |
| jobserver | 0.1.35 | MIT OR Apache-2.0 | https://github.com/rust-lang/jobserver-rs |
| js-sys | 0.3.105 | MIT OR Apache-2.0 | https://github.com/wasm-bindgen/wasm-bindgen/tree/master/crates/js-sys |
| keccak | 0.2.2 | Apache-2.0 OR MIT | https://github.com/RustCrypto/sponges |
| kem | 0.3.0 | Apache-2.0 OR MIT | https://github.com/RustCrypto/traits |
| keyboard-types | 0.8.3 | MIT OR Apache-2.0 | https://github.com/rust-windowing/keyboard-types |
| libc | 0.2.189 | MIT OR Apache-2.0 | https://github.com/rust-lang/libc |
| libm | 0.2.16 | MIT | https://github.com/rust-lang/compiler-builtins |
| libredox | 0.1.23 | MIT | https://gitlab.redox-os.org/redox-os/libredox |
| libsqlite3-sys | 0.37.0 | MIT | https://github.com/rusqlite/rusqlite |
| linux-raw-sys | 0.12.1 | Apache-2.0 WITH LLVM-exception OR Apache-2.0 OR MIT | https://github.com/sunfishcode/linux-raw-sys |
| litemap | 0.8.3 | Unicode-3.0 | https://github.com/unicode-org/icu4x |
| lock_api | 0.4.14 | MIT OR Apache-2.0 | https://github.com/Amanieu/parking_lot |
| log | 0.4.34 | MIT OR Apache-2.0 | https://github.com/rust-lang/log |
| lz4_flex | 0.14.0 | MIT | https://github.com/pseitz/lz4_flex |
| macro_magic | 0.5.1 | MIT | https://github.com/sam0x17/macro_magic |
| macro_magic_core | 0.5.1 | MIT | https://github.com/sam0x17/macro_magic |
| macro_magic_core_macros | 0.5.1 | MIT | https://github.com/sam0x17/macro_magic |
| macro_magic_macros | 0.5.1 | MIT | https://github.com/sam0x17/macro_magic |
| markup5ever | 0.39.0 | MIT OR Apache-2.0 | https://github.com/servo/html5ever |
| matchit | 0.8.4 | MIT AND BSD-3-Clause | https://github.com/ibraheemdev/matchit |
| md-5 | 0.11.0 | MIT OR Apache-2.0 | https://github.com/RustCrypto/hashes |
| md5 | 0.8.1 | Apache-2.0 OR MIT | https://github.com/stainless-steel/md5 |
| memchr | 2.8.3 | Unlicense OR MIT | https://github.com/BurntSushi/memchr |
| memoffset | 0.9.1 | MIT | https://github.com/Gilnaa/memoffset |
| mime | 0.3.17 | MIT OR Apache-2.0 | https://github.com/hyperium/mime |
| mime_guess | 2.0.5 | MIT | https://github.com/abonander/mime_guess |
| minimal-lexical | 0.2.1 | MIT/Apache-2.0 | https://github.com/Alexhuszagh/minimal-lexical |
| miniz_oxide | 0.8.9 | MIT OR Zlib OR Apache-2.0 | https://github.com/Frommi/miniz_oxide/tree/master/miniz_oxide |
| miniz_oxide | 0.9.1 | MIT OR Zlib OR Apache-2.0 | https://github.com/Frommi/miniz_oxide/tree/master/miniz_oxide |
| mio | 1.2.3 | MIT | https://github.com/tokio-rs/mio |
| ml-kem | 0.3.2 | Apache-2.0 OR MIT | https://github.com/RustCrypto/KEMs |
| module-lattice | 0.2.3 | Apache-2.0 OR MIT | https://github.com/RustCrypto/KEMs |
| mongodb | 3.9.1 | Apache-2.0 | https://github.com/mongodb/mongo-rust-driver |
| mongodb-internal-macros | 3.9.1 | Apache-2.0 | https://github.com/mongodb/mongo-rust-driver |
| muda | 0.21.2 | Apache-2.0 OR MIT | https://github.com/tauri-apps/muda |
| multiversion_no_op | 1.0.0 | Apache-2.0 OR MIT | https://github.com/hsivonen/multiversion_no_op |
| native-tls | 0.2.18 | MIT OR Apache-2.0 | https://github.com/rust-native-tls/rust-native-tls |
| ndk | 0.9.0 | MIT OR Apache-2.0 | https://github.com/rust-mobile/ndk |
| ndk-context | 0.1.1 | MIT OR Apache-2.0 | https://github.com/rust-windowing/android-ndk-rs |
| ndk-sys | 0.6.0+11769913 | MIT OR Apache-2.0 | https://github.com/rust-mobile/ndk |
| new_debug_unreachable | 1.0.6 | MIT | https://github.com/mbrubeck/rust-debug-unreachable |
| nix | 0.31.3 | MIT | https://github.com/nix-rust/nix |
| nom | 7.1.3 | MIT | https://github.com/Geal/nom |
| num_enum | 0.7.6 | BSD-3-Clause OR MIT OR Apache-2.0 | https://github.com/illicitonion/num_enum |
| num_enum_derive | 0.7.6 | BSD-3-Clause OR MIT OR Apache-2.0 | https://github.com/illicitonion/num_enum |
| num-bigint | 0.4.8 | MIT OR Apache-2.0 | https://github.com/rust-num/num-bigint |
| num-bigint | 0.5.1 | MIT OR Apache-2.0 | https://github.com/rust-num/num-bigint |
| num-conv | 0.2.2 | MIT OR Apache-2.0 | https://github.com/jhpratt/num-conv |
| num-integer | 0.1.47 | MIT OR Apache-2.0 | https://github.com/rust-num/num-integer |
| num-traits | 0.2.19 | MIT OR Apache-2.0 | https://github.com/rust-num/num-traits |
| objc2 | 0.6.5 | MIT | https://github.com/madsmtm/objc2 |
| objc2-app-kit | 0.3.2 | Zlib OR Apache-2.0 OR MIT | https://github.com/madsmtm/objc2 |
| objc2-cloud-kit | 0.3.2 | Zlib OR Apache-2.0 OR MIT | https://github.com/madsmtm/objc2 |
| objc2-core-data | 0.3.2 | Zlib OR Apache-2.0 OR MIT | https://github.com/madsmtm/objc2 |
| objc2-core-foundation | 0.3.2 | Zlib OR Apache-2.0 OR MIT | https://github.com/madsmtm/objc2 |
| objc2-core-graphics | 0.3.2 | Zlib OR Apache-2.0 OR MIT | https://github.com/madsmtm/objc2 |
| objc2-core-image | 0.3.2 | Zlib OR Apache-2.0 OR MIT | https://github.com/madsmtm/objc2 |
| objc2-core-location | 0.3.2 | Zlib OR Apache-2.0 OR MIT | https://github.com/madsmtm/objc2 |
| objc2-core-text | 0.3.2 | Zlib OR Apache-2.0 OR MIT | https://github.com/madsmtm/objc2 |
| objc2-encode | 4.1.0 | MIT | https://github.com/madsmtm/objc2 |
| objc2-exception-helper | 0.1.1 | Zlib OR Apache-2.0 OR MIT | https://github.com/madsmtm/objc2 |
| objc2-foundation | 0.3.2 | MIT | https://github.com/madsmtm/objc2 |
| objc2-io-surface | 0.3.2 | Zlib OR Apache-2.0 OR MIT | https://github.com/madsmtm/objc2 |
| objc2-quartz-core | 0.3.2 | Zlib OR Apache-2.0 OR MIT | https://github.com/madsmtm/objc2 |
| objc2-ui-kit | 0.3.2 | Zlib OR Apache-2.0 OR MIT | https://github.com/madsmtm/objc2 |
| objc2-user-notifications | 0.3.2 | Zlib OR Apache-2.0 OR MIT | https://github.com/madsmtm/objc2 |
| objc2-web-kit | 0.3.2 | Zlib OR Apache-2.0 OR MIT | https://github.com/madsmtm/objc2 |
| once_cell | 1.21.4 | MIT OR Apache-2.0 | https://github.com/matklad/once_cell |
| openssl | 0.10.81 | Apache-2.0 | https://github.com/rust-openssl/rust-openssl |
| openssl-macros | 0.1.1 | MIT/Apache-2.0 | https://crates.io/crates/openssl-macros |
| openssl-probe | 0.2.1 | MIT OR Apache-2.0 | https://github.com/rustls/openssl-probe |
| openssl-sys | 0.9.117 | MIT | https://github.com/rust-openssl/rust-openssl |
| option-ext | 0.2.0 | MPL-2.0 | https://github.com/soc/option-ext |
| p256 | 0.14.0 | Apache-2.0 OR MIT | https://github.com/RustCrypto/elliptic-curves |
| p384 | 0.14.0 | Apache-2.0 OR MIT | https://github.com/RustCrypto/elliptic-curves |
| p521 | 0.14.0 | Apache-2.0 OR MIT | https://github.com/RustCrypto/elliptic-curves |
| pageant | 0.2.3 | Apache-2.0 | https://github.com/warp-tech/russh |
| pango | 0.18.3 | MIT | https://github.com/gtk-rs/gtk-rs-core |
| pango-sys | 0.18.0 | MIT | https://github.com/gtk-rs/gtk-rs-core |
| parking | 2.2.1 | Apache-2.0 OR MIT | https://github.com/smol-rs/parking |
| parking_lot | 0.12.5 | MIT OR Apache-2.0 | https://github.com/Amanieu/parking_lot |
| parking_lot_core | 0.9.12 | MIT OR Apache-2.0 | https://github.com/Amanieu/parking_lot |
| paste | 1.0.15 | MIT OR Apache-2.0 | https://github.com/dtolnay/paste |
| pastey | 0.2.3 | MIT OR Apache-2.0 | https://github.com/as1100k/pastey |
| pbkdf2 | 0.13.0 | MIT OR Apache-2.0 | https://github.com/RustCrypto/password-hashes |
| pem-rfc7468 | 1.0.0 | Apache-2.0 OR MIT | https://github.com/RustCrypto/formats |
| percent-encoding | 2.3.2 | MIT OR Apache-2.0 | https://github.com/servo/rust-url |
| phf | 0.13.1 | MIT | https://github.com/rust-phf/rust-phf |
| phf_codegen | 0.13.1 | MIT | https://github.com/rust-phf/rust-phf |
| phf_generator | 0.13.1 | MIT | https://github.com/rust-phf/rust-phf |
| phf_macros | 0.13.1 | MIT | https://github.com/rust-phf/rust-phf |
| phf_shared | 0.13.1 | MIT | https://github.com/rust-phf/rust-phf |
| pin-project-lite | 0.2.17 | Apache-2.0 OR MIT | https://github.com/taiki-e/pin-project-lite |
| pkcs1 | 0.8.0-rc.4 | Apache-2.0 OR MIT | https://github.com/RustCrypto/formats |
| pkcs5 | 0.8.1 | Apache-2.0 OR MIT | https://github.com/RustCrypto/formats |
| pkcs8 | 0.11.0 | Apache-2.0 OR MIT | https://github.com/RustCrypto/formats |
| pkg-config | 0.3.34 | MIT OR Apache-2.0 | https://github.com/rust-lang/pkg-config-rs |
| png | 0.18.1 | MIT OR Apache-2.0 | https://github.com/image-rs/image-png |
| poly1305 | 0.9.1 | Apache-2.0 OR MIT | https://github.com/RustCrypto/universal-hashes |
| polyval | 0.7.3 | Apache-2.0 OR MIT | https://github.com/RustCrypto/universal-hashes |
| potential_utf | 0.1.6 | Unicode-3.0 | https://github.com/unicode-org/icu4x |
| powerfmt | 0.2.0 | MIT OR Apache-2.0 | https://github.com/jhpratt/powerfmt |
| ppv-lite86 | 0.2.21 | MIT OR Apache-2.0 | https://github.com/cryptocorrosion/cryptocorrosion |
| precomputed-hash | 0.1.1 | MIT | https://github.com/emilio/precomputed-hash |
| primefield | 0.14.0 | Apache-2.0 OR MIT | https://github.com/RustCrypto/elliptic-curves |
| primeorder | 0.14.0 | Apache-2.0 OR MIT | https://github.com/RustCrypto/elliptic-curves |
| proc-macro-crate | 1.3.1 | MIT OR Apache-2.0 | https://github.com/bkchr/proc-macro-crate |
| proc-macro-crate | 2.0.2 | MIT OR Apache-2.0 | https://github.com/bkchr/proc-macro-crate |
| proc-macro-crate | 3.5.0 | MIT OR Apache-2.0 | https://github.com/bkchr/proc-macro-crate |
| proc-macro-error | 1.0.4 | MIT OR Apache-2.0 | https://gitlab.com/CreepySkeleton/proc-macro-error |
| proc-macro-error-attr | 1.0.4 | MIT OR Apache-2.0 | https://gitlab.com/CreepySkeleton/proc-macro-error |
| proc-macro2 | 1.0.107 | MIT OR Apache-2.0 | https://github.com/dtolnay/proc-macro2 |
| process-wrap | 10.0.1 | Apache-2.0 OR MIT | https://github.com/watchexec/process-wrap |
| prost | 0.14.4 | Apache-2.0 | https://github.com/tokio-rs/prost |
| prost-derive | 0.14.4 | Apache-2.0 | https://github.com/tokio-rs/prost |
| quote | 1.0.47 | MIT OR Apache-2.0 | https://github.com/dtolnay/quote |
| r-efi | 5.3.0 | MIT OR Apache-2.0 OR LGPL-2.1-or-later | https://github.com/r-efi/r-efi |
| r-efi | 6.0.0 | MIT OR Apache-2.0 OR LGPL-2.1-or-later | https://github.com/r-efi/r-efi |
| radium | 0.7.0 | MIT | https://github.com/bitvecto-rs/radium |
| rand | 0.9.5 | MIT OR Apache-2.0 | https://github.com/rust-random/rand |
| rand | 0.10.3 | MIT OR Apache-2.0 | https://github.com/rust-random/rand |
| rand_chacha | 0.9.0 | MIT OR Apache-2.0 | https://github.com/rust-random/rand |
| rand_core | 0.9.5 | MIT OR Apache-2.0 | https://github.com/rust-random/rand |
| rand_core | 0.10.1 | MIT OR Apache-2.0 | https://github.com/rust-random/rand_core |
| raw-window-handle | 0.6.2 | MIT OR Apache-2.0 OR Zlib | https://github.com/rust-windowing/raw-window-handle |
| redis | 1.7.1 | BSD-3-Clause | https://github.com/redis-rs/redis-rs |
| redox_syscall | 0.5.18 | MIT | https://gitlab.redox-os.org/redox-os/syscall |
| redox_users | 0.5.2 | MIT | https://gitlab.redox-os.org/redox-os/users |
| ref-cast | 1.0.27 | MIT OR Apache-2.0 | https://github.com/dtolnay/ref-cast |
| ref-cast-impl | 1.0.27 | MIT OR Apache-2.0 | https://github.com/dtolnay/ref-cast |
| regex | 1.13.1 | MIT OR Apache-2.0 | https://github.com/rust-lang/regex |
| regex-automata | 0.4.18 | MIT OR Apache-2.0 | https://github.com/rust-lang/regex |
| regex-syntax | 0.8.11 | MIT OR Apache-2.0 | https://github.com/rust-lang/regex |
| reqwest | 0.13.5 | MIT OR Apache-2.0 | https://github.com/seanmonstar/reqwest |
| rfc6979 | 0.6.0 | Apache-2.0 OR MIT | https://github.com/RustCrypto/signatures |
| ring | 0.17.14 | Apache-2.0 AND ISC | https://github.com/briansmith/ring |
| rmcp | 3.5.0 | Apache-2.0 | https://github.com/modelcontextprotocol/rust-sdk |
| rsa | 0.10.0-rc.18 | MIT OR Apache-2.0 | https://github.com/RustCrypto/RSA |
| russh | 0.63.3 | Apache-2.0 | https://github.com/warp-tech/russh |
| russh-cryptovec | 0.62.0 | Apache-2.0 | https://github.com/warp-tech/russh |
| russh-sftp | 3.0.0 | Apache-2.0 | https://github.com/AspectUnk/russh-sftp |
| russh-util | 0.52.0 | Apache-2.0 | https://github.com/warp-tech/russh |
| rust-embed | 8.12.0 | MIT | https://pyrossh.dev/repos/rust-embed |
| rust-embed-impl | 8.12.0 | MIT | https://pyrossh.dev/repos/rust-embed |
| rust-embed-utils | 8.12.0 | MIT | https://pyrossh.dev/repos/rust-embed |
| rustc_version | 0.4.1 | MIT OR Apache-2.0 | https://github.com/djc/rustc-version-rs |
| rustc_version_runtime | 0.3.0 | MIT | https://github.com/seppo0010/rustc-version-runtime-rs |
| rustc-hash | 2.1.3 | Apache-2.0 OR MIT | https://github.com/rust-lang/rustc-hash |
| rustix | 1.1.4 | Apache-2.0 WITH LLVM-exception OR Apache-2.0 OR MIT | https://github.com/bytecodealliance/rustix |
| rustls | 0.23.45 | Apache-2.0 OR ISC OR MIT | https://github.com/rustls/rustls |
| rustls-native-certs | 0.8.4 | Apache-2.0 OR ISC OR MIT | https://github.com/rustls/rustls-native-certs |
| rustls-pki-types | 1.15.1 | MIT OR Apache-2.0 | https://github.com/rustls/pki-types |
| rustls-platform-verifier | 0.7.0 | MIT OR Apache-2.0 | https://github.com/rustls/rustls-platform-verifier |
| rustls-platform-verifier-android | 0.1.1 | MIT OR Apache-2.0 | https://github.com/rustls/rustls-platform-verifier |
| rustls-webpki | 0.103.15 | ISC | https://github.com/rustls/webpki |
| rustversion | 1.0.23 | MIT OR Apache-2.0 | https://github.com/dtolnay/rustversion |
| ryu | 1.0.23 | Apache-2.0 OR BSL-1.0 | https://github.com/dtolnay/ryu |
| salsa20 | 0.11.0 | MIT OR Apache-2.0 | https://github.com/RustCrypto/stream-ciphers |
| same-file | 1.0.6 | Unlicense/MIT | https://github.com/BurntSushi/same-file |
| schannel | 0.1.29 | MIT | https://github.com/steffengy/schannel-rs |
| schemars | 1.2.2 | MIT | https://github.com/GREsau/schemars |
| schemars_derive | 1.2.2 | MIT | https://github.com/GREsau/schemars |
| scopeguard | 1.2.0 | MIT OR Apache-2.0 | https://github.com/bluss/scopeguard |
| scrypt | 0.12.0 | MIT OR Apache-2.0 | https://github.com/RustCrypto/password-hashes |
| sec1 | 0.8.1 | Apache-2.0 OR MIT | https://github.com/RustCrypto/formats |
| security-framework | 3.7.0 | MIT OR Apache-2.0 | https://github.com/kornelski/rust-security-framework |
| security-framework-sys | 2.17.0 | MIT OR Apache-2.0 | https://github.com/kornelski/rust-security-framework |
| selectors | 0.38.0 | MPL-2.0 | https://github.com/servo/stylo |
| semver | 1.0.28 | MIT OR Apache-2.0 | https://github.com/dtolnay/semver |
| serde | 1.0.229 | MIT OR Apache-2.0 | https://github.com/serde-rs/serde |
| serde_bytes | 0.11.19 | MIT OR Apache-2.0 | https://github.com/serde-rs/bytes |
| serde_core | 1.0.229 | MIT OR Apache-2.0 | https://github.com/serde-rs/serde |
| serde_derive | 1.0.229 | MIT OR Apache-2.0 | https://github.com/serde-rs/serde |
| serde_derive_internals | 0.30.0 | MIT OR Apache-2.0 | https://github.com/serde-rs/serde |
| serde_json | 1.0.151 | MIT OR Apache-2.0 | https://github.com/serde-rs/json |
| serde_path_to_error | 0.1.20 | MIT OR Apache-2.0 | https://github.com/dtolnay/path-to-error |
| serde_spanned | 0.6.9 | MIT OR Apache-2.0 | https://github.com/toml-rs/toml |
| serde_urlencoded | 0.7.1 | MIT/Apache-2.0 | https://github.com/nox/serde_urlencoded |
| serde_with | 3.23.0 | MIT OR Apache-2.0 | https://github.com/jonasbb/serde_with |
| serde_with_macros | 3.23.0 | MIT OR Apache-2.0 | https://github.com/jonasbb/serde_with |
| servo_arc | 0.4.3 | MIT OR Apache-2.0 | https://github.com/servo/stylo |
| sha1 | 0.10.7 | MIT OR Apache-2.0 | https://github.com/RustCrypto/hashes |
| sha1 | 0.11.0 | MIT OR Apache-2.0 | https://github.com/RustCrypto/hashes |
| sha2 | 0.10.9 | MIT OR Apache-2.0 | https://github.com/RustCrypto/hashes |
| sha2 | 0.11.0 | MIT OR Apache-2.0 | https://github.com/RustCrypto/hashes |
| sha3 | 0.11.0 | MIT OR Apache-2.0 | https://github.com/RustCrypto/hashes |
| sha3 | 0.12.0 | MIT OR Apache-2.0 | https://github.com/RustCrypto/hashes |
| shellexpand | 3.1.2 | MIT/Apache-2.0 | https://gitlab.com/ijackson/rust-shellexpand |
| shlex | 2.0.1 | MIT OR Apache-2.0 | https://github.com/comex/rust-shlex |
| signal-hook-registry | 1.4.8 | MIT OR Apache-2.0 | https://github.com/vorner/signal-hook |
| signature | 3.0.0 | Apache-2.0 OR MIT | https://github.com/RustCrypto/traits |
| simd_cesu8 | 1.2.0 | Apache-2.0 OR MIT | https://github.com/seancroach/simd_cesu8 |
| simd-adler32 | 0.3.10 | MIT | https://github.com/mcountryman/simd-adler32 |
| simdutf8 | 0.1.5 | MIT OR Apache-2.0 | https://github.com/rusticstuff/simdutf8 |
| siphasher | 1.0.4 | MIT OR Apache-2.0 | https://github.com/jedisct1/rust-siphash |
| slab | 0.4.12 | MIT | https://github.com/tokio-rs/slab |
| smallvec | 1.16.0 | MIT OR Apache-2.0 | https://github.com/servo/rust-smallvec |
| socket2 | 0.6.5 | MIT OR Apache-2.0 | https://github.com/rust-lang/socket2 |
| soup3 | 0.5.0 | MIT | https://gitlab.gnome.org/World/Rust/soup3-rs |
| soup3-sys | 0.5.0 | MIT | https://gitlab.gnome.org/World/Rust/soup3-rs |
| spin | 0.9.9 | MIT | https://github.com/mvdnes/spin-rs |
| spki | 0.8.0 | Apache-2.0 OR MIT | https://github.com/RustCrypto/formats |
| sponge-cursor | 0.1.0 | MIT OR Apache-2.0 | https://github.com/RustCrypto/utils |
| sqlx | 0.9.0 | MIT OR Apache-2.0 | https://github.com/launchbadge/sqlx |
| sqlx-core | 0.9.0 | MIT OR Apache-2.0 | https://github.com/launchbadge/sqlx |
| sqlx-mysql | 0.9.0 | MIT OR Apache-2.0 | https://github.com/launchbadge/sqlx |
| sqlx-postgres | 0.9.0 | MIT OR Apache-2.0 | https://github.com/launchbadge/sqlx |
| sqlx-sqlite | 0.9.0 | MIT OR Apache-2.0 | https://github.com/launchbadge/sqlx |
| sse-stream | 0.2.6 | MIT OR Apache-2.0 | https://github.com/4t145/sse-stream |
| ssh-cipher | 0.3.0 | Apache-2.0 OR MIT | https://github.com/RustCrypto/SSH |
| ssh-encoding | 0.3.0 | Apache-2.0 OR MIT | https://github.com/RustCrypto/SSH |
| ssh-key | 0.7.0-rc.11 | Apache-2.0 OR MIT | https://github.com/RustCrypto/SSH |
| stable_deref_trait | 1.2.1 | MIT OR Apache-2.0 | https://github.com/storyyeller/stable_deref_trait |
| string_cache | 0.9.0 | MIT OR Apache-2.0 | https://github.com/servo/string-cache |
| string_cache_codegen | 0.6.1 | MIT OR Apache-2.0 | https://github.com/servo/string-cache |
| stringprep | 0.1.5 | MIT/Apache-2.0 | https://github.com/sfackler/rust-stringprep |
| strsim | 0.11.1 | MIT | https://github.com/rapidfuzz/strsim-rs |
| strum | 0.28.0 | MIT | https://github.com/Peternator7/strum |
| strum_macros | 0.28.0 | MIT | https://github.com/Peternator7/strum |
| subtle | 2.6.1 | BSD-3-Clause | https://github.com/dalek-cryptography/subtle |
| syn | 1.0.109 | MIT OR Apache-2.0 | https://github.com/dtolnay/syn |
| syn | 2.0.119 | MIT OR Apache-2.0 | https://github.com/dtolnay/syn |
| syn | 3.0.5 | MIT OR Apache-2.0 | https://github.com/dtolnay/syn |
| sync_wrapper | 1.0.2 | Apache-2.0 | https://github.com/Actyx/sync_wrapper |
| synstructure | 0.13.2 | MIT | https://github.com/mystor/synstructure |
| system-configuration | 0.7.0 | MIT OR Apache-2.0 | https://github.com/mullvad/system-configuration-rs |
| system-configuration-sys | 0.6.0 | MIT OR Apache-2.0 | https://github.com/mullvad/system-configuration-rs |
| system-deps | 6.2.2 | MIT OR Apache-2.0 | https://github.com/gdesmott/system-deps |
| take_mut | 0.2.2 | MIT | https://github.com/Sgeo/take_mut |
| tao | 0.37.2 | Apache-2.0 | https://github.com/tauri-apps/tao |
| tao-macros | 0.1.4 | MIT OR Apache-2.0 | https://github.com/tauri-apps/tao |
| tap | 1.0.1 | MIT | https://github.com/myrrlyn/tap |
| target-lexicon | 0.12.16 | Apache-2.0 WITH LLVM-exception | https://github.com/bytecodealliance/target-lexicon |
| tempfile | 3.27.0 | MIT OR Apache-2.0 | https://github.com/Stebalien/tempfile |
| tendril | 0.5.1 | MIT OR Apache-2.0 | https://github.com/servo/html5ever |
| thiserror | 1.0.69 | MIT OR Apache-2.0 | https://github.com/dtolnay/thiserror |
| thiserror | 2.0.21 | MIT OR Apache-2.0 | https://github.com/dtolnay/thiserror |
| thiserror-impl | 1.0.69 | MIT OR Apache-2.0 | https://github.com/dtolnay/thiserror |
| thiserror-impl | 2.0.21 | MIT OR Apache-2.0 | https://github.com/dtolnay/thiserror |
| time | 0.3.55 | MIT OR Apache-2.0 | https://github.com/time-rs/time |
| time-core | 0.1.9 | MIT OR Apache-2.0 | https://github.com/time-rs/time |
| time-macros | 0.2.32 | MIT OR Apache-2.0 | https://github.com/time-rs/time |
| tiny-keccak | 2.0.2 | CC0-1.0 | https://crates.io/crates/tiny-keccak |
| tinystr | 0.8.4 | Unicode-3.0 | https://github.com/unicode-org/icu4x |
| tinyvec | 1.13.2 | Zlib OR Apache-2.0 OR MIT | https://github.com/Lokathor/tinyvec |
| tinyvec_macros | 0.1.1 | MIT OR Apache-2.0 OR Zlib | https://github.com/Soveu/tinyvec_macros |
| tokio | 1.53.1 | MIT | https://github.com/tokio-rs/tokio |
| tokio-macros | 2.7.2 | MIT | https://github.com/tokio-rs/tokio |
| tokio-native-tls | 0.3.1 | MIT | https://github.com/tokio-rs/tls |
| tokio-rustls | 0.26.5 | MIT OR Apache-2.0 | https://github.com/rustls/tokio-rustls |
| tokio-stream | 0.1.19 | MIT | https://github.com/tokio-rs/tokio |
| tokio-tungstenite | 0.29.0 | MIT | https://github.com/snapview/tokio-tungstenite |
| tokio-util | 0.7.19 | MIT | https://github.com/tokio-rs/tokio |
| toml | 0.8.2 | MIT OR Apache-2.0 | https://github.com/toml-rs/toml |
| toml_datetime | 0.6.3 | MIT OR Apache-2.0 | https://github.com/toml-rs/toml |
| toml_datetime | 1.1.2+spec-1.1.0 | MIT OR Apache-2.0 | https://github.com/toml-rs/toml |
| toml_edit | 0.19.15 | MIT OR Apache-2.0 | https://github.com/toml-rs/toml |
| toml_edit | 0.20.2 | MIT OR Apache-2.0 | https://github.com/toml-rs/toml |
| toml_edit | 0.25.17+spec-1.1.0 | MIT OR Apache-2.0 | https://github.com/toml-rs/toml |
| toml_parser | 1.1.5+spec-1.1.0 | MIT OR Apache-2.0 | https://github.com/toml-rs/toml |
| tower | 0.5.3 | MIT | https://github.com/tower-rs/tower |
| tower-http | 0.6.11 | MIT | https://github.com/tower-rs/tower-http |
| tower-layer | 0.3.3 | MIT | https://github.com/tower-rs/tower |
| tower-service | 0.3.3 | MIT | https://github.com/tower-rs/tower |
| tracing | 0.1.44 | MIT | https://github.com/tokio-rs/tracing |
| tracing-attributes | 0.1.31 | MIT | https://github.com/tokio-rs/tracing |
| tracing-core | 0.1.36 | MIT | https://github.com/tokio-rs/tracing |
| tray-icon | 0.26.1 | MIT OR Apache-2.0 | https://github.com/tauri-apps/tray-icon |
| try-lock | 0.2.5 | MIT | https://github.com/seanmonstar/try-lock |
| tungstenite | 0.29.0 | MIT OR Apache-2.0 | https://github.com/snapview/tungstenite-rs |
| typed-builder | 0.22.0 | MIT OR Apache-2.0 | https://github.com/idanarye/rust-typed-builder |
| typed-builder-macro | 0.22.0 | MIT OR Apache-2.0 | https://github.com/idanarye/rust-typed-builder |
| typenum | 1.20.1 | MIT OR Apache-2.0 | https://github.com/paholg/typenum |
| unicase | 2.9.0 | MIT OR Apache-2.0 | https://github.com/seanmonstar/unicase |
| unicode-bidi | 0.3.18 | MIT OR Apache-2.0 | https://github.com/servo/unicode-bidi |
| unicode-ident | 1.0.24 | (MIT OR Apache-2.0) AND Unicode-3.0 | https://github.com/dtolnay/unicode-ident |
| unicode-normalization | 0.1.25 | MIT OR Apache-2.0 | https://github.com/unicode-rs/unicode-normalization |
| unicode-properties | 0.1.4 | MIT/Apache-2.0 | https://github.com/unicode-rs/unicode-properties |
| unicode-segmentation | 1.13.3 | MIT OR Apache-2.0 | https://github.com/unicode-rs/unicode-segmentation |
| unicode-xid | 0.2.6 | MIT OR Apache-2.0 | https://github.com/unicode-rs/unicode-xid |
| universal-hash | 0.6.1 | MIT OR Apache-2.0 | https://github.com/RustCrypto/traits |
| untrusted | 0.9.0 | ISC | https://github.com/briansmith/untrusted |
| url | 2.5.8 | MIT OR Apache-2.0 | https://github.com/servo/rust-url |
| utf8_iter | 1.0.4 | Apache-2.0 OR MIT | https://github.com/hsivonen/utf8_iter |
| uuid | 1.26.0 | Apache-2.0 OR MIT | https://github.com/uuid-rs/uuid |
| value-ext | 0.1.5 | MIT OR Apache-2.0 | https://github.com/jeremychone/rust-value-ext |
| vcpkg | 0.2.15 | MIT/Apache-2.0 | https://github.com/mcgoo/vcpkg-rs |
| version_check | 0.9.5 | MIT/Apache-2.0 | https://github.com/SergioBenitez/version_check |
| version-compare | 0.2.1 | MIT | https://gitlab.com/timvisee/version-compare |
| walkdir | 2.5.0 | Unlicense/MIT | https://github.com/BurntSushi/walkdir |
| want | 0.3.1 | MIT | https://github.com/seanmonstar/want |
| wasi | 0.11.1+wasi-snapshot-preview1 | Apache-2.0 WITH LLVM-exception OR Apache-2.0 OR MIT | https://github.com/bytecodealliance/wasi |
| wasip2 | 1.0.4+wasi-0.2.12 | Apache-2.0 WITH LLVM-exception OR Apache-2.0 OR MIT | https://github.com/bytecodealliance/wasi-rs |
| wasm-bindgen | 0.2.128 | MIT OR Apache-2.0 | https://github.com/wasm-bindgen/wasm-bindgen |
| wasm-bindgen-futures | 0.4.78 | MIT OR Apache-2.0 | https://github.com/wasm-bindgen/wasm-bindgen/tree/master/crates/futures |
| wasm-bindgen-macro | 0.2.128 | MIT OR Apache-2.0 | https://github.com/wasm-bindgen/wasm-bindgen/tree/master/crates/macro |
| wasm-bindgen-macro-support | 0.2.128 | MIT OR Apache-2.0 | https://github.com/wasm-bindgen/wasm-bindgen/tree/main/crates/macro-support |
| wasm-bindgen-shared | 0.2.128 | MIT OR Apache-2.0 | https://github.com/wasm-bindgen/wasm-bindgen/tree/master/crates/shared |
| wasm-streams | 0.5.0 | MIT OR Apache-2.0 | https://github.com/MattiasBuelens/wasm-streams |
| web_atoms | 0.2.6 | MIT OR Apache-2.0 | https://github.com/servo/html5ever |
| web-sys | 0.3.105 | MIT OR Apache-2.0 | https://github.com/wasm-bindgen/wasm-bindgen/tree/master/crates/web-sys |
| webkit2gtk | 2.0.2 | MIT | https://github.com/tauri-apps/webkit2gtk-rs |
| webkit2gtk-sys | 2.0.2 | MIT | https://github.com/tauri-apps/webkit2gtk-rs |
| webpki-root-certs | 1.0.9 | CDLA-Permissive-2.0 | https://github.com/rustls/webpki-roots |
| webpki-roots | 1.0.9 | CDLA-Permissive-2.0 | https://github.com/rustls/webpki-roots |
| webview2-com | 0.39.1 | MIT | https://github.com/wravery/webview2-rs |
| webview2-com-macros | 0.8.1 | MIT | https://github.com/wravery/webview2-rs |
| webview2-com-sys | 0.39.1 | MIT | https://github.com/wravery/webview2-rs |
| whoami | 2.1.3 | Apache-2.0 OR BSL-1.0 OR MIT | https://github.com/ardaku/whoami |
| winapi | 0.3.9 | MIT/Apache-2.0 | https://github.com/retep998/winapi-rs |
| winapi-i686-pc-windows-gnu | 0.4.0 | MIT/Apache-2.0 | https://github.com/retep998/winapi-rs |
| winapi-util | 0.1.11 | Unlicense OR MIT | https://github.com/BurntSushi/winapi-util |
| winapi-x86_64-pc-windows-gnu | 0.4.0 | MIT/Apache-2.0 | https://github.com/retep998/winapi-rs |
| windows | 0.62.2 | MIT OR Apache-2.0 | https://github.com/microsoft/windows-rs |
| windows_aarch64_gnullvm | 0.42.2 | MIT OR Apache-2.0 | https://github.com/microsoft/windows-rs |
| windows_aarch64_gnullvm | 0.52.6 | MIT OR Apache-2.0 | https://github.com/microsoft/windows-rs |
| windows_aarch64_msvc | 0.42.2 | MIT OR Apache-2.0 | https://github.com/microsoft/windows-rs |
| windows_aarch64_msvc | 0.52.6 | MIT OR Apache-2.0 | https://github.com/microsoft/windows-rs |
| windows_i686_gnu | 0.42.2 | MIT OR Apache-2.0 | https://github.com/microsoft/windows-rs |
| windows_i686_gnu | 0.52.6 | MIT OR Apache-2.0 | https://github.com/microsoft/windows-rs |
| windows_i686_gnullvm | 0.52.6 | MIT OR Apache-2.0 | https://github.com/microsoft/windows-rs |
| windows_i686_msvc | 0.42.2 | MIT OR Apache-2.0 | https://github.com/microsoft/windows-rs |
| windows_i686_msvc | 0.52.6 | MIT OR Apache-2.0 | https://github.com/microsoft/windows-rs |
| windows_x86_64_gnu | 0.42.2 | MIT OR Apache-2.0 | https://github.com/microsoft/windows-rs |
| windows_x86_64_gnu | 0.52.6 | MIT OR Apache-2.0 | https://github.com/microsoft/windows-rs |
| windows_x86_64_gnullvm | 0.42.2 | MIT OR Apache-2.0 | https://github.com/microsoft/windows-rs |
| windows_x86_64_gnullvm | 0.52.6 | MIT OR Apache-2.0 | https://github.com/microsoft/windows-rs |
| windows_x86_64_msvc | 0.42.2 | MIT OR Apache-2.0 | https://github.com/microsoft/windows-rs |
| windows_x86_64_msvc | 0.52.6 | MIT OR Apache-2.0 | https://github.com/microsoft/windows-rs |
| windows-collections | 0.3.2 | MIT OR Apache-2.0 | https://github.com/microsoft/windows-rs |
| windows-core | 0.62.2 | MIT OR Apache-2.0 | https://github.com/microsoft/windows-rs |
| windows-future | 0.3.2 | MIT OR Apache-2.0 | https://github.com/microsoft/windows-rs |
| windows-implement | 0.60.2 | MIT OR Apache-2.0 | https://github.com/microsoft/windows-rs |
| windows-interface | 0.59.3 | MIT OR Apache-2.0 | https://github.com/microsoft/windows-rs |
| windows-link | 0.2.1 | MIT OR Apache-2.0 | https://github.com/microsoft/windows-rs |
| windows-numerics | 0.3.1 | MIT OR Apache-2.0 | https://github.com/microsoft/windows-rs |
| windows-registry | 0.6.1 | MIT OR Apache-2.0 | https://github.com/microsoft/windows-rs |
| windows-result | 0.4.1 | MIT OR Apache-2.0 | https://github.com/microsoft/windows-rs |
| windows-strings | 0.5.1 | MIT OR Apache-2.0 | https://github.com/microsoft/windows-rs |
| windows-sys | 0.45.0 | MIT OR Apache-2.0 | https://github.com/microsoft/windows-rs |
| windows-sys | 0.52.0 | MIT OR Apache-2.0 | https://github.com/microsoft/windows-rs |
| windows-sys | 0.61.2 | MIT OR Apache-2.0 | https://github.com/microsoft/windows-rs |
| windows-targets | 0.42.2 | MIT OR Apache-2.0 | https://github.com/microsoft/windows-rs |
| windows-targets | 0.52.6 | MIT OR Apache-2.0 | https://github.com/microsoft/windows-rs |
| windows-threading | 0.2.1 | MIT OR Apache-2.0 | https://github.com/microsoft/windows-rs |
| windows-version | 0.1.7 | MIT OR Apache-2.0 | https://github.com/microsoft/windows-rs |
| winnow | 0.5.40 | MIT | https://github.com/winnow-rs/winnow |
| winnow | 1.0.4 | MIT | https://github.com/winnow-rs/winnow |
| wit-bindgen | 0.57.1 | Apache-2.0 WITH LLVM-exception OR Apache-2.0 OR MIT | https://github.com/bytecodealliance/wit-bindgen |
| wnaf | 0.14.1 | Apache-2.0 OR MIT | https://github.com/RustCrypto/elliptic-curves |
| writeable | 0.6.4 | Unicode-3.0 | https://github.com/unicode-org/icu4x |
| wry | 0.57.1 | Apache-2.0 OR MIT | https://github.com/tauri-apps/wry |
| wyz | 0.5.1 | MIT | https://github.com/myrrlyn/wyz |
| xxhash-rust | 0.8.19 | BSL-1.0 | https://github.com/DoumanAsh/xxhash-rust |
| yoke | 0.8.3 | Unicode-3.0 | https://github.com/unicode-org/icu4x |
| yoke-derive | 0.8.2 | Unicode-3.0 | https://github.com/unicode-org/icu4x |
| zerocopy | 0.8.56 | BSD-2-Clause OR Apache-2.0 OR MIT | https://github.com/google/zerocopy |
| zerocopy-derive | 0.8.56 | BSD-2-Clause OR Apache-2.0 OR MIT | https://github.com/google/zerocopy |
| zerofrom | 0.1.8 | Unicode-3.0 | https://github.com/unicode-org/icu4x |
| zerofrom-derive | 0.1.7 | Unicode-3.0 | https://github.com/unicode-org/icu4x |
| zeroize | 1.9.0 | Apache-2.0 OR MIT | https://github.com/RustCrypto/utils |
| zerotrie | 0.2.5 | Unicode-3.0 | https://github.com/unicode-org/icu4x |
| zerovec | 0.11.8 | Unicode-3.0 | https://github.com/unicode-org/icu4x |
| zerovec-derive | 0.11.6 | Unicode-3.0 | https://github.com/unicode-org/icu4x |
| zmij | 1.0.23 | MIT | https://github.com/dtolnay/zmij |
| zstd | 0.13.3 | MIT | https://github.com/gyscos/zstd-rs |
| zstd-safe | 7.3.0 | BSD-3-Clause | https://github.com/gyscos/zstd-rs |
| zstd-sys | 2.1.2+zstd.1.5.7 | BSD-3-Clause | https://github.com/gyscos/zstd-rs |
