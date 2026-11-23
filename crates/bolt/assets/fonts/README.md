# IBM Plex Sans Thai

The four faces come from `@ibm/plex-sans-thai` 1.1.0:
<https://registry.npmjs.org/@ibm/plex-sans-thai/-/plex-sans-thai-1.1.0.tgz>
(SHA-256 `9ddc70a54f7992646989fbc981f0bfeca32b3adceaa9bdf425ece7f0fbfd6565`).
The package's `fonts/complete/woff/*.woff` files were converted losslessly
back to TTF by decompressing their original sfnt tables. The font version is 1.2.
The license is `IBMPlexSansThai-OFL.txt`, copied from the package's `LICENSE.txt`.

| File | Bytes | SHA-256 |
|---|---:|---|
| `IBMPlexSansThai-Regular.ttf` | 116632 | `daa1e3024598fc137aa2b96716cce8607923ae80a3a83687174034f36bdd6a4f` |
| `IBMPlexSansThai-Medium.ttf` | 117068 | `8607b3f7dc0ed2d695b3dfdaf344ebc27a0e6d75e2818fe4c21ed7d14b0369e8` |
| `IBMPlexSansThai-SemiBold.ttf` | 117248 | `c501de893df441814eb3014bf95a13ed14d41baab8353e93f1bc34021531af9d` |
| `IBMPlexSansThai-Bold.ttf` | 117292 | `a0f2bf9b9eacc5b3e8dc1d780bdccdd0a771b99b3e56507e8eb1c3b62294f09b` |
| `IBMPlexSansThai-OFL.txt` | 4363 | `37784b44044a4ffd9256702b7c0982c37e5c8887ba90c6dca0479aea93dc898d` |

The upstream CRLF license is 4456 bytes with SHA-256 `7e6b2818edbd8f6a01ae80641cc8f16a51080d08fb4e532be3a0b6f74adb07da`.

# Noto Sans Thai

These four faces are the fallback under the data face (`AppFonts::MONO_FALLBACK`),
because no Noto Sans Mono carries Thai and JetBrains Mono covers no Thai code
point. The data face itself stays JetBrains Mono.

They come from the Noto project's published hinted static TTFs, font version
2.002:
<https://github.com/notofonts/notofonts.github.io/tree/main/fonts/NotoSansThai/hinted/ttf>
The license is `NotoSansThai-OFL.txt`, from the same upstream:
<https://github.com/google/fonts/blob/main/ofl/notosansthai/OFL.txt>
Copyright 2022 The Noto Project Authors.

| File | Weight | Bytes | SHA-256 |
|---|---:|---:|---|
| `NotoSansThai-Regular.ttf` | 400 | 37780 | `61cf814eec46b294d6ea4401ac295d0cecd5207bd2331dcc5a15e7301d30ee44` |
| `NotoSansThai-Medium.ttf` | 500 | 38296 | `11bc8f6cf3523241995f07225fcc7d8e25eb55064fbfa39281a5e44dcd59f5f5` |
| `NotoSansThai-SemiBold.ttf` | 600 | 38520 | `d19ec702c2ddc26f74ccf934342147afd8bce43eb1435fa46a06ab93e368011b` |
| `NotoSansThai-Bold.ttf` | 700 | 37824 | `2ac6c6e8a478e23b15f76e4894af1fa2210f8f350e4e6e54aad530bec03efbfb` |
| `NotoSansThai-OFL.txt` | — | 4380 | `2e98fd23a52d253db8612cd5942c8f2ff4111b21d2367050fdca91d8ccc374a0` |

The four faces add 152420 bytes. `crates/bolt/tests/typography_contract.rs`
checks each one's family, weight and coverage of U+0E01..U+0E3A.
