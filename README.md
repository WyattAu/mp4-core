# mp4-core

ISO-BMFF (MP4/M4A/M4B) box reader and writer for Rust — `ftyp`, `moov`/`trak`/`stbl`,
iTunes `ilst` metadata, Nero `chpl` chapters, `no_std`.

**Layer:** L0 — leaf · **Estate deps:** none · **Runtime deps:** `thiserror`

Part of the WyattAu estate's audio path: `id3-core` reads the tags in an MP3,
`mp4-core` reads the box structure of an MP4, and an M4B is an MP4 with a
chapter track. See
[WyattAu/engineering-standards](https://github.com/WyattAu/engineering-standards).

## Why

An audiobook library needs two things from a file: the chapters, and enough
structure to serve or transcode it. Both live in different formats — `CHAP`/
`CTOC` for MP3, `chpl` or a text track for MP4 — and neither is a library's job
to reimplement. The MP4 side is a box tree, which is small but has sharp edges:
three size encodings, full-box containers, metadata atoms whose names are
arbitrary four-byte codes, and durations in per-box timescales.

## Install

```toml
[dependencies]
mp4-core = "0.1"
```

## What it covers

| Area | Notes |
|---|---|
| Box tree | 32-bit size, 64-bit `largesize`, and `size == 0` to end of container |
| `uuid` boxes | 16-byte extended type parsed |
| Containers | explicit descent set, plus `ilst` atoms and full-box `meta` |
| `ftyp` | major brand, minor version, compatible brands |
| `mvhd` / `tkhd` / `mdhd` | both version-0 and version-1 field layouts |
| `hdlr` | `soun` / `text` / `vide` |
| iTunes metadata | `udta` → `meta` → `ilst`, `data` type flag modelled explicitly |
| `stik` | 9 = audiobook |
| Chapters | Nero `chpl` read + write; QuickTime text-track titles read |

## What it deliberately does not do

- **No muxing.** Boxes can be built and assembled, but samples are not
  interleaved into `mdat` and a conformant chapter *text track* is not produced.
  It reads one if given.
- **No fragmented-MP4 streaming rules.** `moof`/`traf` parse as containers; the
  segment-sequence logic is the caller's.
- **No codec work.** An `mp4a` entry is read for its sample rate; the audio is
  `dsp-core`'s and a decoder's.

## Example

Chapters out of an M4B:

```rust
use mp4_core::{Mp4File};

let data = std::fs::read("book.m4b")?;
let file = Mp4File::parse(&data)?;

if file.is_audiobook() {
    for ch in file.chapters.sorted() {
        println!("{:>10} ms  {}", ch.start_ms, ch.title);
    }
}
```

Writing one:

```rust
use mp4_core::chapter::{Chapter, ChapterList};
use mp4_core::{Mp4File};

let chapters = ChapterList {
    chapters: vec![
        Chapter::new(0, "Opening"),
        Chapter::new(900_000, "Chapter One"),
    ],
};

let box_ = chapters.to_chpl_box();
let payload = box_.payload;
// The payload round-trips through the parser on its own.
let back = ChapterList::parse_chpl(&payload)?;
```

Metadata:

```rust
use mp4_core::atoms;

let file = Mp4File::parse(&std::fs::read("book.m4b")?)?;
println!("{:?}", file.metadata.text(atoms::TITLE));
if let Some(cover) = file.metadata.cover() {
    // Raw image bytes; validating them is `media-kit`'s job.
    let _ = cover.len();
}
```

## Guarantees

- **No panic on malformed input.** Every parse path returns `Result`; `deny(panic)`
  is on crate-wide.
- **No indexing in library code** (`deny(indexing_slicing)`).
- **Durations are converted with the timescale that governs them.** `mvhd` and
  `mdhd` timescales need not match, and a test pins the case where conflating
  them is wrong by 44.1×.
- **Unknown boxes are preserved**, so a file written by a newer muxer survives a
  read.
- **`no_std` + `alloc`**, verified on `thumbv7em-none-eabihf`.
- **Malformed chapters are rejected, not guessed** — a wrong `chpl` version, a
  non-zero reserved field, or a title running past the box is an error.

## Gates

- `cargo test` — 25 tests (7 unit, 17 integration, 1 doc)
- `cargo clippy --all-features --all-targets -- -D warnings`
- `cargo fmt --check`
- `cargo check --target thumbv7em-none-eabihf --no-default-features`
- estate `feature-compat` gate

## License

Dual-licensed under [MIT](LICENSE-MIT) or [Apache-2.0](LICENSE-APACHE), at your
option.