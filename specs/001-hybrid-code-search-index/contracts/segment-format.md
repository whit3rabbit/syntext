# Segment File Format Contract

**Date**: 2026-03-25
**Type**: Binary file format (on-disk)

## File Extension

`.seg`

## Layout

```
Offset 0:
+-------------------------------+
| Header (40 bytes, fixed)      |
+-------------------------------+
| Document Table                |
|   doc_count x DocEntry        |
+-------------------------------+
| Postings Section              |
|   Variable-length lists       |
+-------------------------------+
| Dictionary Section            |
|   gram_count x DictEntry      |
|   (page-aligned start)        |
+-------------------------------+
| TOC Footer (48 bytes, fixed)  |
+-------------------------------+
```

## Header (40 bytes)

| Offset | Size | Type | Field | Description |
|---|---|---|---|---|
| 0 | 4 | [u8; 4] | magic | `b"SNTX"` |
| 4 | 4 | u32 LE | version | Format version (currently 1) |
| 8 | 4 | u32 LE | doc_count | Number of documents |
| 12 | 4 | u32 LE | gram_count | Number of dictionary entries |
| 16 | 8 | u64 LE | doc_table_offset | Byte offset to document table |
| 24 | 8 | u64 LE | postings_offset | Byte offset to postings section |
| 32 | 8 | u64 LE | dict_offset | Byte offset to dictionary section |

## Document Table

Array of `DocEntry`, one per document. Variable-length due to path strings.

Each `DocEntry`:

| Size | Type | Field |
|---|---|---|
| 4 | u32 LE | doc_id |
| 8 | u64 LE | content_hash |
| 8 | u64 LE | size_bytes |
| 2 | u16 LE | path_len |
| path_len | [u8] | path (UTF-8 bytes) |

An index of doc_id -> DocEntry offset is stored at the start of the document table section (array of u64 offsets, one per doc_id) for O(1) lookup.

## Postings Section

Sequential posting lists, each prefixed by a small header.

Each posting list:

| Size | Type | Field |
|---|---|---|
| 1 | u8 | encoding (0 = delta-varint, 1 = roaring) |
| 4 | u32 LE | entry_count |
| 4 | u32 LE | byte_length |
| byte_length | [u8] | encoded data |

**Encoding 0 (delta-varint)**: Doc IDs are delta-encoded then varint-compressed. First value is absolute, subsequent values are deltas from the previous.

**Encoding 1 (roaring)**: Serialized `RoaringBitmap` using the standard Roaring serialization format.

## Dictionary Section

Sorted array of `DictEntry`, page-aligned start (4096-byte boundary).

Each `DictEntry` (20 bytes):

| Size | Type | Field |
|---|---|---|
| 8 | u64 LE | gram_hash |
| 8 | u64 LE | postings_offset (absolute byte offset in file) |
| 4 | u32 LE | entry_count (posting list length for cardinality estimates) |

Sorted by `gram_hash` ascending. Lookup via binary search. Page alignment enables mmap with `madvise(MADV_WILLNEED)` on the dictionary only.

## TOC Footer (48 bytes)

| Offset from EOF | Size | Type | Field |
|---|---|---|---|
| -48 | 8 | u64 LE | doc_table_offset |
| -40 | 8 | u64 LE | postings_offset |
| -32 | 8 | u64 LE | dict_offset |
| -24 | 4 | u32 LE | doc_count |
| -20 | 4 | u32 LE | gram_count |
| -16 | 8 | u64 LE | checksum (xxhash64 of all preceding bytes) |
| -8 | 4 | u32 LE | version |
| -4 | 4 | [u8; 4] | magic (`b"SNTX"`) |

The TOC footer duplicates critical offsets so readers can verify the file by reading only the last 48 bytes. The trailing magic enables file type detection by reading the end of the file.

## Integrity

- The checksum covers all bytes from offset 0 to (file_size - 48).
- On open, verify: magic matches at both header and footer, version matches, checksum matches. If any check fails, report `CorruptIndex` and request rebuild.
- All multi-byte integers are little-endian.

## Versioning

- Version 1: initial format as described here.
- Future versions increment the version field. Readers that encounter an unknown version must refuse to open the segment (not silently ignore).
