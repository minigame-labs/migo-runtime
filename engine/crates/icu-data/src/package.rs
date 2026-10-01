//! Reading and rewriting an ICU common-data package (`icudtl.dat`, data format `CmnD`).
//!
//! The package is a header, a table of contents and the items' bytes, all little-endian here:
//!
//! ```text
//! header        headerSize:u16  magic 0xDA 0x27  UDataInfo ...  (copyright text up to headerSize)
//! toc (at headerSize)
//!               count:u32
//!               count x { nameOffset:u32, dataOffset:u32 }   offsets are from the start of the toc
//!               names, NUL-terminated, in toc order
//!               items, each 16-byte aligned, in toc order; the last one ends at the end of the file
//! ```
//!
//! This reads that layout and writes it again with some items left out. It does not interpret the items: what an
//! item is for, and whether another one needs it, is `policy.rs`'s business and the Intl tests' to confirm.

use std::fmt;

const MAGIC: [u8; 2] = [0xDA, 0x27];
/// Items start on 16-byte boundaries (ICU maps the package and reads tables out of it in place).
const ITEM_ALIGN: usize = 16;

#[derive(Debug, PartialEq, Eq)]
pub enum PackageError {
    TooShort,
    BadMagic,
    BadTable(&'static str),
}

impl fmt::Display for PackageError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            PackageError::TooShort => f.write_str("not an ICU data package: too short"),
            PackageError::BadMagic => f.write_str("not an ICU data package: bad magic"),
            PackageError::BadTable(why) => write!(f, "ICU data package table of contents: {why}"),
        }
    }
}

impl std::error::Error for PackageError {}

/// One item: its name as the table of contents spells it (`icudt77l/af.res`) and its bytes.
pub struct Item<'a> {
    pub name: &'a str,
    pub bytes: &'a [u8],
}

/// A parsed package: the header bytes and the items, in table order.
pub struct Package<'a> {
    header: &'a [u8],
    pub items: Vec<Item<'a>>,
}

fn word(bytes: &[u8], at: usize) -> Result<u32, PackageError> {
    bytes
        .get(at..at + 4)
        .map(|b| u32::from_le_bytes([b[0], b[1], b[2], b[3]]))
        .ok_or(PackageError::TooShort)
}

impl<'a> Package<'a> {
    pub fn parse(file: &'a [u8]) -> Result<Self, PackageError> {
        if file.len() < 4 {
            return Err(PackageError::TooShort);
        }
        if file[2..4] != MAGIC {
            return Err(PackageError::BadMagic);
        }
        let header_size = usize::from(u16::from_le_bytes([file[0], file[1]]));
        if header_size < 4 || header_size >= file.len() {
            return Err(PackageError::BadTable("header size is outside the file"));
        }
        let toc = &file[header_size..];
        let count = word(toc, 0)? as usize;
        let entries_end = 4usize
            .checked_add(
                count
                    .checked_mul(8)
                    .ok_or(PackageError::BadTable("count overflows"))?,
            )
            .ok_or(PackageError::BadTable("count overflows"))?;
        if entries_end > toc.len() {
            return Err(PackageError::BadTable("entries run past the file"));
        }

        let mut starts = Vec::with_capacity(count);
        let mut names = Vec::with_capacity(count);
        for index in 0..count {
            let name_offset = word(toc, 4 + index * 8)? as usize;
            let data_offset = word(toc, 8 + index * 8)? as usize;
            let tail = toc
                .get(name_offset..)
                .ok_or(PackageError::BadTable("a name offset is outside the file"))?;
            let end = tail
                .iter()
                .position(|byte| *byte == 0)
                .ok_or(PackageError::BadTable("a name is not terminated"))?;
            let name = std::str::from_utf8(&tail[..end])
                .map_err(|_| PackageError::BadTable("a name is not UTF-8"))?;
            if data_offset > toc.len() {
                return Err(PackageError::BadTable("an item starts outside the file"));
            }
            starts.push(data_offset);
            names.push(name);
        }
        // An item ends where the next one starts, and the last at the end of the file.
        let mut items = Vec::with_capacity(count);
        for index in 0..count {
            let end = starts.get(index + 1).copied().unwrap_or(toc.len());
            if end < starts[index] {
                return Err(PackageError::BadTable("items are not in offset order"));
            }
            items.push(Item {
                name: names[index],
                bytes: &toc[starts[index]..end],
            });
        }
        Ok(Self {
            header: &file[..header_size],
            items,
        })
    }

    /// The same package without the items `keep` refuses. The order of the rest is the order they had: the table of
    /// contents is searched by name, in order, and ICU wrote it sorted.
    pub fn filtered(&self, keep: impl Fn(&str) -> bool) -> Vec<u8> {
        let kept: Vec<&Item<'_>> = self.items.iter().filter(|item| keep(item.name)).collect();
        let count = kept.len();
        let entries_end = 4 + count * 8;
        let names_len: usize = kept.iter().map(|item| item.name.len() + 1).sum();
        let mut data_at = (entries_end + names_len).next_multiple_of(ITEM_ALIGN);

        let mut toc =
            Vec::with_capacity(data_at + kept.iter().map(|i| i.bytes.len()).sum::<usize>());
        toc.extend_from_slice(&(count as u32).to_le_bytes());
        let mut name_at = entries_end;
        let mut offsets = Vec::with_capacity(count);
        for item in &kept {
            offsets.push((name_at, data_at));
            name_at += item.name.len() + 1;
            // An item's bytes as they were stored: they carry their own padding to the next boundary, except the
            // last, which is padded here so the next start stays aligned in whatever is written after it.
            data_at += item.bytes.len().next_multiple_of(ITEM_ALIGN);
        }
        for (name, data) in &offsets {
            toc.extend_from_slice(&(*name as u32).to_le_bytes());
            toc.extend_from_slice(&(*data as u32).to_le_bytes());
        }
        for item in &kept {
            toc.extend_from_slice(item.name.as_bytes());
            toc.push(0);
        }
        toc.resize(offsets.first().map_or(toc.len(), |(_, data)| *data), 0);
        for item in &kept {
            toc.extend_from_slice(item.bytes);
            toc.resize(toc.len().next_multiple_of(ITEM_ALIGN), 0);
        }

        let mut out = Vec::with_capacity(self.header.len() + toc.len());
        out.extend_from_slice(self.header);
        out.extend_from_slice(&toc);
        out
    }
}
