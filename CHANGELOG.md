# Changelog

All notable changes to this project are documented here. Format: [Keep a
Changelog](https://keepachangelog.com/) — versions follow [semver](https://semver.org).

## [0.1.0] - 2026-10-05

First release. `mp4-core` is the estate's L0 MP4 container layer, the ISO-BMFF
half of what `id3-core` does for MP3: an audiobook's M4B is an MP4 with a
chapter track, so a library needs both.

### Added

- **The box tree** (`container`) — all three size encodings (32-bit, 64-bit
  `largesize`, and `size == 0` running to the end of the container), the `uuid`
  extended type, container descent through the known container set, recursive
  serialization that recomputes sizes, and a `walk` helper for deep lookups.
- **File structure** (`moov`) — `ftyp` with its brand list, `mvhd`/`tkhd`/`mdhd`
  headers with both version-0 and version-1 field layouts, `hdlr` handler types,
  and a `Track` summary with durations already converted to milliseconds.
- **iTunes metadata** (`metadata`) — `udta` → `meta` → `ilst`, with the `data`
  type flag modelled explicitly (UTF-8, UTF-16, JPEG, PNG, signed and unsigned
  integers), the well-known atom names, `stik` = 9 for audiobook detection, and
  `trkn` track/total extraction.
- **Chapters** (`chapter`) — the Nero `chpl` box, read and write, with
  `sorted`, `chapter_at` (half-open intervals, last chapter open-ended), and
  `span_ms`; plus reading chapter titles out of a QuickTime text track's
  samples.

### Fixed during development

Four bugs the tests caught, each worth recording because they are the classic
MP4-reader failures:

1. **`meta` is a full box, not a plain container.** Its payload begins with
   version+flags before the child boxes, so a descent that read the first four
   bytes as a box header saw `size == 0` — the "runs to end of file" form — and
   swallowed every sibling as one opaque blob. `full_box_container` now marks
   it, and the descent strips those four bytes.
2. **`ilst` atoms are containers with arbitrary names.** `©nam`, `covr`, `trkn`
   and friends cannot appear in a fixed `matches!` list, so every atom was being
   treated as opaque and read back empty. `is_ilst_atom` recognises both the
   `©`-prefixed reserved namespace and the free-form one.
3. **The `meta` full-box header, present or not, changes where the first child
   type sits.** Apple's writer omitted it for years, so detecting the
   header-less form at offset zero was wrong — it has to be detected at offset
   four, because the payload then begins with a child's *size*.
4. **Durations are per-box timescales.** `mvhd` and `mdhd` each carry a duration
   in their own timescale, which need not match. The conversion is explicit for
   each, and a test pins the case where a conflated reader would be off by 44.1×.

### Known limitations

- **No muxing.** Boxes can be built and assembled, but this crate does not
  interleave samples into `mdat`, nor produce a conformant chapter *text track*.
  It reads one if given.
- **No fragmented-MP4 streaming logic.** `moof`/`traf` parse as containers, but
  the segment-sequence rules are the caller's.
- **No codec work.** An `mp4a` sample entry is read for its sample rate; the
  audio itself is `dsp-core`'s and a decoder's business.
- **Version-1 headers are parsed but not written.** `to_payload` emits version 0,
  which every reader accepts.