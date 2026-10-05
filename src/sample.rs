//! Sample tables: locating and timing a track's samples.
//!
//! # Why a chapter track needs all four boxes
//!
//! A QuickTime chapter list stores one text sample per chapter. To recover the
//! titles *and* their positions you need four boxes, and each answers a question no
//! other one can:
//!
//! | Box | Question it answers |
//! |---|---|
//! | `stts` | when does each sample appear? `DT(i) = Σ delta(j)` over `j < i`, in the track's timescale |
//! | `stsz` | how long is each sample? |
//! | `stco`/`co64` | where does each **chunk** start in the file? |
//! | `stsc` | how many samples are in each chunk? |
//!
//! # The mistake this module exists to prevent
//!
//! `stco` gives **chunk** offsets, not sample offsets. A reader that treats the first
//! chunk offset as the first *sample* offset, then adds sizes in sequence, is correct
//! only when every chunk holds the same number of samples and there is exactly one
//! chunk — which is true of a one-chunk chapter track and false of almost everything
//! else.
//!
//! Worse, the failure is silent. A wrong offset yields bytes that look like text, so
//! a reader reports *garbage titles* rather than an error. That is why `stsc` is
//! consulted here even though the common case would work without it.
//!
//! # `co64` is not optional
//!
//! `co64` is the 64-bit form of `stco`, needed past 4 GB — which is a 9-hour CD-quality
//! audiobook, well inside the range real libraries hold. A reader that only knows
//! `stco` silently reads the wrong bytes on exactly the largest files.

use alloc::vec::Vec;

use crate::error::Mp4Error;

/// Ceiling on table entries read from any one box.
///
/// A declared count in an untrusted file must never become an allocation. 65 536
/// entries covers any real track — a 30-hour audiobook at 44.1 kHz has fewer samples
/// than that in its chapter track by orders of magnitude — while bounding the work.
const MAX_TABLE_ENTRIES: u32 = 65_536;

/// A track's decoded sample table.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct SampleTable {
    /// How many samples the track has, from `stsz`.
    pub sample_count: u32,
    /// Per-sample durations in `timescale` units, expanded from `stts`.
    ///
    /// Empty when there is no `stts`, in which case no sample has a known time and
    /// the caller must not invent one.
    pub durations: Vec<u32>,
    /// Per-sample sizes in bytes, from `stsz`.
    pub sizes: Vec<u32>,
    /// Per-sample absolute file offsets, from `stco`/`co64` via `stsc`.
    ///
    /// Shorter than `sizes` when the tables disagree, which happens in real files: the
    /// sample count and the chunk count are independent fields and a muxer that wrote
    /// three samples into one chunk declared the chunk as holding one.
    pub offsets: Vec<u64>,
}

impl SampleTable {
    /// Whether every sample has a size, a time and a location.
    ///
    /// A table that fails this cannot place a single sample, and a caller that
    /// proceeds anyway produces plausible wrong output rather than an error.
    #[must_use]
    pub fn is_complete(&self) -> bool {
        let n = self.sample_count as usize;
        n > 0 && self.sizes.len() >= n && self.durations.len() >= n && self.offsets.len() >= n
    }

    /// The start time of sample `i` in `timescale` units.
    #[must_use]
    pub fn start_time(&self, i: usize, timescale: u32) -> Option<u64> {
        if timescale == 0 {
            return None;
        }
        let mut units = 0u64;
        for d in self.durations.iter().take(i) {
            units = units.saturating_add(u64::from(*d));
        }
        Some(units.saturating_mul(1_000) / u64::from(timescale))
    }

    /// The byte range of sample `i` within `file`.
    #[must_use]
    pub fn sample_bytes<'a>(&self, i: usize, file: &'a [u8]) -> Option<&'a [u8]> {
        let start = usize::try_from(*self.offsets.get(i)?).ok()?;
        let size = usize::try_from(*self.sizes.get(i)?).ok()?;
        file.get(start..start.checked_add(size)?)
    }
}

/// Read a track's sample table from its `stbl`.
///
/// # Errors
///
/// [`Mp4Error::MissingBox`] when `stsz` is absent, which leaves the sample count
/// unknowable. The other boxes are optional: each one that is missing removes one
/// capability (timing, location) rather than failing the read, so a partial table is
/// returned and [`SampleTable::is_complete`] reports what is missing.
pub fn read_sample_table(stbl: &crate::container::Box_) -> Result<SampleTable, Mp4Error> {
    let stsz = stbl
        .child(*b"stsz")
        .ok_or(Mp4Error::MissingBox { box_type: *b"stsz" })?;
    let sp = stsz.payload.as_slice();
    let uniform = u32_at(sp, 4, "stsz uniform sample size")?;
    let sample_count = u32_at(sp, 8, "stsz sample count")?;

    let mut sizes = Vec::new();
    if uniform == 0 {
        // One size per sample, bounded by `sample_count` so a corrupt count cannot make
        // this allocate against the file's whole length.
        //
        // A count larger than the payload actually holds is read as far as the bytes go
        // and no further, rather than erroring. Refusing the whole table would lose the
        // samples a muxer *did* write — which for a chapter track means losing the whole
        // list over one wrong field — and `is_complete` reports the shortfall rather
        // than hiding it.
        let declared = sample_count.min(MAX_TABLE_ENTRIES) as usize;
        let present = sp.len().saturating_sub(12) / 4;
        sizes.reserve(declared.min(present));
        for i in 0..declared.min(present) {
            sizes.push(u32_at(sp, 12 + i * 4, "stsz entry")?);
        }
    } else {
        sizes.resize(sample_count.min(MAX_TABLE_ENTRIES) as usize, uniform);
    }

    let durations = stbl.child(*b"stts").map(read_stts).transpose()?;
    let offsets = read_offsets(stbl, sample_count as usize, &sizes)?;

    Ok(SampleTable {
        sample_count,
        durations: durations.unwrap_or_default(),
        sizes,
        offsets,
    })
}

/// Expand `stts`'s run-length form into one duration per sample.
///
/// The box stores `(sample_count, sample_delta)` pairs, which is a compression: a
/// track of 300 000 identical AAC frames declares one entry. Expanding it is what
/// makes `start_time` a prefix sum, at the cost of memory proportional to the frame
/// count — bounded here, because an untrusted count must not become an allocation.
fn read_stts(stts: &crate::container::Box_) -> Result<Vec<u32>, Mp4Error> {
    /// Ceiling on expanded samples, ~74 minutes of 44.1 kHz AAC.
    const MAX_EXPANDED: usize = 8_000_000;

    let sp = stts.payload.as_slice();
    let entry_count = u32_at(sp, 4, "stts entry count")?;
    let mut out: Vec<u32> = Vec::new();
    for i in 0..entry_count.min(MAX_TABLE_ENTRIES) as usize {
        let count = u32_at(sp, 8 + i * 8, "stts sample count")?;
        let delta = u32_at(sp, 12 + i * 8, "stts sample delta")?;
        if out.len().saturating_add(count as usize) > MAX_EXPANDED {
            // Refuse to grow without bound; a caller asking for more than this is
            // looking at a corrupt table, and truncating is better than aborting.
            break;
        }
        out.resize(out.len() + count as usize, delta);
    }
    Ok(out)
}

/// Locate every sample, walking chunks through `stsc`.
///
/// `stsc` entries are `(first_chunk, samples_per_chunk, description_index)` and each
/// entry governs chunks from `first_chunk` until the next entry's. `first_chunk` is
/// **one-based**, which is the kind of detail that silently shifts a table by one
/// chunk when forgotten.
fn read_offsets(
    stbl: &crate::container::Box_,
    sample_count: usize,
    sizes: &[u32],
) -> Result<Vec<u64>, Mp4Error> {
    let (chunk_offsets, width) = match stbl.child(*b"stco") {
        Some(stco) => {
            let sp = stco.payload.as_slice();
            let n = u32_at(sp, 4, "stco entry count")? as usize;
            let mut v = Vec::with_capacity(n.min(MAX_TABLE_ENTRIES as usize));
            for i in 0..n.min(MAX_TABLE_ENTRIES as usize) {
                v.push(u64::from(u32_at(sp, 8 + i * 4, "stco offset")?));
            }
            (v, 4usize)
        }
        None => match stbl.child(*b"co64") {
            Some(co64) => {
                let sp = co64.payload.as_slice();
                let n = u32_at(sp, 4, "co64 entry count")? as usize;
                let mut v = Vec::with_capacity(n.min(MAX_TABLE_ENTRIES as usize));
                for i in 0..n.min(MAX_TABLE_ENTRIES as usize) {
                    v.push(u64_at(sp, 8 + i * 8, "co64 offset")?);
                }
                (v, 8usize)
            }
            None => return Ok(Vec::new()),
        },
    };
    let _ = width;

    // `stsc` is optional. Without it every chunk is assumed to hold one sample, which
    // is the degenerate case and the only one a chapter track usually needs.
    let runs: Vec<(u32, u32)> = match stbl.child(*b"stsc") {
        Some(stsc) => {
            let sp = stsc.payload.as_slice();
            let n = u32_at(sp, 4, "stsc entry count")? as usize;
            let mut v = Vec::with_capacity(n.min(MAX_TABLE_ENTRIES as usize));
            for i in 0..n.min(MAX_TABLE_ENTRIES as usize) {
                let first_chunk = u32_at(sp, 8 + i * 12, "stsc first chunk")?;
                let samples = u32_at(sp, 12 + i * 12, "stsc samples per chunk")?;
                v.push((first_chunk, samples));
            }
            v
        }
        None => Vec::new(),
    };

    let mut offsets: Vec<u64> = Vec::new();
    let mut sample = 0usize;

    for (chunk_index, base) in chunk_offsets.iter().enumerate() {
        if sample >= sample_count {
            break;
        }
        // `first_chunk` is one-based, so the governing entry is the last one whose
        // first_chunk is at or below this chunk's one-based number.
        let one_based = chunk_index as u32 + 1;
        let per_chunk = runs
            .iter()
            .rev()
            .find(|(first, _)| *first <= one_based)
            .map_or(1u32, |(_, samples)| *samples);

        let mut cursor = *base;
        for _ in 0..per_chunk {
            if sample >= sample_count {
                break;
            }
            let Some(size) = sizes.get(sample) else {
                // The tables disagree about how many samples exist; stop rather than
                // walking with an unknown stride.
                return Ok(offsets);
            };
            offsets.push(cursor);
            cursor = cursor.saturating_add(u64::from(*size));
            sample += 1;
        }
    }

    Ok(offsets)
}

use crate::fields::{u32_at, u64_at};

#[cfg(test)]
mod tests {
    #![allow(
        clippy::unwrap_used,
        clippy::expect_used,
        clippy::indexing_slicing,
        clippy::panic
    )]
    use super::*;
    use crate::container::{container, leaf};
    use alloc::vec;

    /// `stts` with the given `(count, delta)` pairs.
    fn stts(entries: &[(u32, u32)]) -> crate::container::Box_ {
        let mut p = vec![0u8, 0, 0, 0];
        p.extend_from_slice(&(entries.len() as u32).to_be_bytes());
        for (c, d) in entries {
            p.extend_from_slice(&c.to_be_bytes());
            p.extend_from_slice(&d.to_be_bytes());
        }
        leaf(*b"stts", p).expect("infallible")
    }

    /// `stsz` with an explicit size per sample.
    fn stsz(sizes: &[u32]) -> crate::container::Box_ {
        let mut p = vec![0u8, 0, 0, 0];
        p.extend_from_slice(&0u32.to_be_bytes()); // not uniform
        p.extend_from_slice(&(sizes.len() as u32).to_be_bytes());
        for s in sizes {
            p.extend_from_slice(&s.to_be_bytes());
        }
        leaf(*b"stsz", p).expect("infallible")
    }

    /// `stco` with the given chunk offsets.
    fn stco(offsets: &[u32]) -> crate::container::Box_ {
        let mut p = vec![0u8, 0, 0, 0];
        p.extend_from_slice(&(offsets.len() as u32).to_be_bytes());
        for o in offsets {
            p.extend_from_slice(&o.to_be_bytes());
        }
        leaf(*b"stco", p).expect("infallible")
    }

    /// `stsc` with the given `(first_chunk, samples_per_chunk)` pairs.
    fn stsc(runs: &[(u32, u32)]) -> crate::container::Box_ {
        let mut p = vec![0u8, 0, 0, 0];
        p.extend_from_slice(&(runs.len() as u32).to_be_bytes());
        for (first, samples) in runs {
            p.extend_from_slice(&first.to_be_bytes());
            p.extend_from_slice(&samples.to_be_bytes());
            p.extend_from_slice(&1u32.to_be_bytes()); // description index
        }
        leaf(*b"stsc", p).expect("infallible")
    }

    #[test]
    fn stts_is_expanded_from_its_run_length_form() {
        // One entry declaring 100 000 identical frames — the compression that makes
        // this box small.
        let t = read_stts(&stts(&[(100_000, 1024)])).expect("reads");
        assert_eq!(t.len(), 100_000);
        assert!(t.iter().all(|d| *d == 1024));
    }

    #[test]
    fn start_time_is_a_prefix_sum_of_the_deltas() {
        let table = SampleTable {
            sample_count: 3,
            durations: vec![600_000, 600_000, 600_000],
            sizes: vec![10, 10, 10],
            offsets: vec![100, 110, 120],
        };
        // Timescale 10 000 means 600_000 units is 60 s.
        assert_eq!(table.start_time(0, 10_000), Some(0));
        assert_eq!(table.start_time(1, 10_000), Some(60_000));
        assert_eq!(table.start_time(2, 10_000), Some(120_000));
        assert_eq!(table.start_time(9, 10_000), Some(180_000), "beyond the end");
    }

    #[test]
    fn a_zero_timescale_yields_no_time_rather_than_a_division_by_zero() {
        let table = SampleTable {
            sample_count: 1,
            durations: vec![1],
            sizes: vec![1],
            offsets: vec![0],
        };
        assert_eq!(table.start_time(0, 0), None);
    }

    #[test]
    fn one_chunk_of_three_samples_locates_all_three() {
        // The shape ffmpeg writes for a chapter track: one chunk, three samples.
        let stbl = container(
            *b"stbl",
            vec![
                stts(&[(3, 600_000)]),
                stsz(&[25, 25, 27]),
                stco(&[44]),
                stsc(&[(1, 3)]),
            ],
        );
        let table = read_sample_table(&stbl).expect("reads");
        assert_eq!(table.sample_count, 3);
        assert_eq!(table.offsets, vec![44, 69, 94]);
        assert!(table.is_complete());
    }

    #[test]
    fn samples_are_located_across_several_chunks() {
        // Two samples in chunk 1, one in chunk 2. A reader that assumed one sample
        // per chunk would place sample 1 at the start of chunk 2 and read garbage.
        let stbl = container(
            *b"stbl",
            vec![
                stts(&[(3, 100)]),
                stsz(&[10, 20, 30]),
                stco(&[1000, 5000]),
                stsc(&[(1, 2)]),
            ],
        );
        let table = read_sample_table(&stbl).expect("reads");
        assert_eq!(table.offsets, vec![1000, 1010, 5000]);
    }

    #[test]
    fn stsc_entries_change_the_run_partway_through() {
        // Chunk 1 holds 1 sample, chunks 2..=3 hold 2 each.
        let stbl = container(
            *b"stbl",
            vec![
                stts(&[(5, 10)]),
                stsz(&[1, 2, 3, 4, 5]),
                stco(&[100, 200, 300]),
                stsc(&[(1, 1), (2, 2)]),
            ],
        );
        let table = read_sample_table(&stbl).expect("reads");
        // Chunk 1 holds one sample of 1 byte, chunk 2 two of 2 and 3, chunk 3 two of
        // 4 and 5 — so each chunk's cursor advances by its own samples' sizes.
        assert_eq!(
            table.offsets,
            vec![100, 200, 202, 300, 304],
            "chunk 1: one sample; chunk 2: two; chunk 3: two"
        );
    }

    #[test]
    fn co64_is_read_for_files_past_four_gigabytes() {
        // A 9-hour CD-quality audiobook crosses 4 GB, so `co64` is the normal case
        // for the largest files rather than an exotic one.
        let mut p = vec![0u8, 0, 0, 0];
        p.extend_from_slice(&1u32.to_be_bytes());
        p.extend_from_slice(&5_000_000_000u64.to_be_bytes());
        let stbl = container(
            *b"stbl",
            vec![
                stts(&[(1, 100)]),
                stsz(&[16]),
                leaf(*b"co64", p).expect("infallible"),
            ],
        );
        let table = read_sample_table(&stbl).expect("reads");
        assert_eq!(table.offsets, vec![5_000_000_000]);
    }

    #[test]
    fn an_absent_stsc_assumes_one_sample_per_chunk() {
        let stbl = container(
            *b"stbl",
            vec![stts(&[(2, 10)]), stsz(&[4, 4]), stco(&[10, 20])],
        );
        let table = read_sample_table(&stbl).expect("reads");
        assert_eq!(table.offsets, vec![10, 20]);
    }

    #[test]
    fn an_incomplete_table_says_so_rather_than_proceeding() {
        // Three samples declared, two chunks present. A caller that ignored this
        // would read sample 2's bytes from wherever the cursor happened to land.
        let stbl = container(
            *b"stbl",
            vec![
                stts(&[(3, 10)]),
                stsz(&[4, 4, 4]),
                stco(&[10, 20]),
                stsc(&[(1, 1)]),
            ],
        );
        let table = read_sample_table(&stbl).expect("reads");
        assert_eq!(table.sample_count, 3);
        assert_eq!(table.offsets.len(), 2);
        assert!(!table.is_complete(), "so a caller knows not to trust it");
    }

    #[test]
    fn a_missing_stsz_is_an_error_because_the_count_is_unknown() {
        let stbl = container(*b"stbl", vec![stts(&[(1, 10)])]);
        assert!(matches!(
            read_sample_table(&stbl),
            Err(Mp4Error::MissingBox { box_type }) if box_type == *b"stsz"
        ));
    }

    #[test]
    fn a_uniform_sample_size_is_expanded() {
        let mut p = vec![0u8, 0, 0, 0];
        p.extend_from_slice(&7u32.to_be_bytes()); // uniform
        p.extend_from_slice(&4u32.to_be_bytes()); // count
        let stbl = container(
            *b"stbl",
            vec![
                leaf(*b"stsz", p).expect("infallible"),
                stts(&[(4, 10)]),
                stco(&[0]),
                stsc(&[(1, 4)]),
            ],
        );
        let table = read_sample_table(&stbl).expect("reads");
        assert_eq!(table.sizes, vec![7, 7, 7, 7]);
        assert_eq!(table.offsets, vec![0, 7, 14, 21]);
    }

    #[test]
    fn a_count_larger_than_the_payload_yields_the_sizes_that_are_there() {
        // A count of 4 billion with four real sizes. Erroring would lose all four over
        // one wrong field — and for a chapter track that is losing the whole list.
        let mut p = vec![0u8, 0, 0, 0];
        p.extend_from_slice(&0u32.to_be_bytes());
        p.extend_from_slice(&u32::MAX.to_be_bytes());
        p.extend_from_slice(&[0u8; 16]); // four sizes

        let stbl = container(
            *b"stbl",
            vec![
                leaf(*b"stsz", p).expect("infallible"),
                stts(&[(4, 10)]),
                stco(&[0]),
                stsc(&[(1, 4)]),
            ],
        );
        let table = read_sample_table(&stbl).expect("reads");
        assert_eq!(table.sizes.len(), 4, "the sizes that were written are kept");
        assert!(
            !table.is_complete(),
            "and the shortfall is reported rather than hidden"
        );
        assert!(
            table.sizes.len() <= MAX_TABLE_ENTRIES as usize,
            "however absurd the declared count"
        );
    }

    #[test]
    fn sample_bytes_are_bounded_by_the_file() {
        let table = SampleTable {
            sample_count: 2,
            durations: vec![0, 0],
            sizes: vec![4, 4],
            offsets: vec![0, 100],
        };
        let file = [0u8; 10];
        assert_eq!(table.sample_bytes(0, &file), Some(&file[..4]));
        assert_eq!(table.sample_bytes(1, &file), None, "offset past the end");
        assert_eq!(table.sample_bytes(9, &file), None);
    }

    #[test]
    fn an_empty_table_is_not_complete() {
        // Otherwise a track with no sample table reads as "nothing to do" rather than
        // "nothing was found", which are different problems.
        assert!(!SampleTable::default().is_complete());
    }
}
