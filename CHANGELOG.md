# Changelog

All notable changes to this project are documented here. Format: [Keep a
Changelog](https://keepachangelog.com/) — versions follow [semver](https://semver.org).

## [0.2.0] - 2026-10-05

Chapter handling corrected against Apple's QuickTime File Format documentation,
Apple Technical Note TN2174, and inspection of real Nero-written M4Bs. Four of
these are defects that made the crate silently wrong on real files.

### Fixed

1. **Real `chpl` boxes are version 0, not version 1.** The Adobe FLV specification
   the format derives from describes version 1, but inspecting a real 20-hour M4B
   gives `chpl: version: 0, flags: 0, chapters: 54`. This crate required version 1
   and therefore **discarded the chapter list of every Nero-written audiobook** —
   and did so invisibly, because the file parsed fine and simply had no chapters.
   Both 0 and 1 are now accepted; only a version newer than either is refused,
   since its layout is unknown rather than merely uncommon.
   `to_chpl_payload` now emits version 0, matching the observed world: the
   version-1 population is hypothetical while version 0 is what players have been
   tested against.
2. **Chapter text samples begin with a two-byte length prefix.** Apple's QTFF and
   TN2174 both specify the sample as a 16-bit big-endian length followed by the
   text, with no null terminator — "Chapter 1" is ten bytes, `00 09` then the text.
   Reading the whole sample as text returned a title beginning with two NULs, which
   every UI renders as `\ufffd\ufffdChapter 1`. The prefix is now stripped when it
   is consistent with the sample and ignored when it is not, so the encoders that
   write bare text still read.
3. **A text track is not a chapter track.** Apple's QTFF is explicit: "It is the
   'chap' track reference that makes the text track into a chapter list." An M4B
   with subtitles, captions, or lyrics has a text track that is not one, and
   `Mp4File::chapter_track` returned it. `chapter::is_chapter_list` now resolves
   the reference properly, including the asymmetry that is easy to get backwards:
   the **referencing** track must be enabled, while the chapter track itself
   "does not need to be enabled, and normally is not".
   `Mp4File::chapter_track_id` exposes the resolved id.
4. **`tref` was not in the container set**, so no track reference was ever
   reachable and the fix above had nothing to read.

### Added

- `chapter::is_chapter_list` — resolves a `chap` reference against the movie,
  accepting both the `tref` form and a bare `chap` under `udta` that some muxers
  emit.
- `Mp4File::chapter_track_id` — the resolved chapter track, or `None`.
- Tests for all of the above, including one that had previously **asserted the
  bug**: `a_chapter_track_marks_a_file_as_an_audiobook` treated any text track as
  chapters and now pins the correct behaviour in both directions.

### Known limitations

- **Chapter samples are assumed contiguous** from the first chunk offset. True for
  the single-chunk table a chapter track almost always has, not for one
  interleaved across several chunks. Reading `stsc` is the next step; until then
  this yields garbage titles rather than an error, so it is documented rather than
  hidden.
- **`nmhd` vs `gmhd` is not distinguished.** TN2174's chapter marker track uses a
  Null Media Header where a QuickTime text track uses a Generic one; both parse.

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