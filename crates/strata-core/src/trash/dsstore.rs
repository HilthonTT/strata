//! Just enough of the Finder `.DS_Store` format to read the "put back"
//! records macOS keeps for items in `~/.Trash`: `ptbL` (the folder the item
//! was deleted from, relative to the volume root) and `ptbN` (its name
//! there, which differs when Finder renamed it to avoid a clash).
//!
//! The file is a buddy-allocated store holding one B-tree of records keyed
//! by file name. Anything unexpected ends the walk; the result is whatever
//! was read up to that point.

use std::collections::HashMap;

/// Where a trashed item came from, as far as Finder recorded it.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct PutBack {
    /// Folder relative to the volume root, e.g. `Users/me/Documents/`.
    pub parent: Option<String>,
    /// Original name.
    pub name: Option<String>,
}

/// The put-back records in `data`, by the item's name in the trash.
pub fn put_back(data: &[u8]) -> HashMap<String, PutBack> {
    let mut out = HashMap::new();
    let _ = parse(data, &mut out);
    out
}

// Bounds keeping a corrupt or hostile file from looping or recursing deeply.
const MAX_DEPTH: usize = 32;
const MAX_NODES: usize = 100_000;

fn parse(data: &[u8], out: &mut HashMap<String, PutBack>) -> Option<()> {
    // The allocator's offsets are counted from after the 4-byte prefix.
    let store = data.get(4..)?;
    let mut header = Reader::at(store, 0)?;
    if header.bytes(4)? != b"Bud1" {
        return None;
    }
    let root_offset = header.u32()? as usize;

    let mut root = Reader::at(store, root_offset)?;
    let count = root.u32()? as usize;
    root.u32()?;
    let mut blocks = Vec::with_capacity(count.min(4096));
    for _ in 0..count {
        blocks.push(root.u32()?);
    }
    // The address table is padded to a multiple of 256 entries.
    root.skip(((count + 255) & !255) * 4 - count * 4)?;
    let mut dsdb = None;
    for _ in 0..root.u32()? {
        let len = root.u8()? as usize;
        let name = root.bytes(len)?;
        let value = root.u32()?;
        if name == b"DSDB" {
            dsdb = Some(value);
        }
    }

    let mut walk = Walk { store, blocks: &blocks, nodes: 0, out };
    let mut master = walk.block(dsdb? as usize)?;
    let root_node = master.u32()?;
    walk.node(root_node as usize, 0)
}

struct Walk<'a, 'o> {
    store: &'a [u8],
    blocks: &'a [u32],
    nodes: usize,
    out: &'o mut HashMap<String, PutBack>,
}

impl<'a> Walk<'a, '_> {
    fn block(&self, id: usize) -> Option<Reader<'a>> {
        let addr = *self.blocks.get(id)? as usize;
        let offset = addr & !0x1f;
        let size = 1usize.checked_shl((addr & 0x1f) as u32)?;
        let end = offset.checked_add(size)?.min(self.store.len());
        Reader::at(self.store.get(..end)?, offset)
    }

    fn node(&mut self, id: usize, depth: usize) -> Option<()> {
        self.nodes += 1;
        if depth > MAX_DEPTH || self.nodes > MAX_NODES {
            return None;
        }
        let mut node = self.block(id)?;
        // Internal nodes name their rightmost child first; leaves have 0.
        let last_child = node.u32()?;
        let count = node.u32()?;
        for _ in 0..count {
            if last_child != 0 {
                let child = node.u32()?;
                self.node(child as usize, depth + 1)?;
            }
            self.record(&mut node)?;
        }
        if last_child != 0 {
            self.node(last_child as usize, depth + 1)?;
        }
        Some(())
    }

    fn record(&mut self, r: &mut Reader) -> Option<()> {
        let len = r.u32()? as usize;
        let file = r.utf16(len)?;
        let code = r.bytes(4)?;
        let kind = r.bytes(4)?;
        let value = match kind {
            b"ustr" => {
                let len = r.u32()? as usize;
                Some(r.utf16(len)?)
            }
            b"bool" => r.skip(1).map(|_| None)?,
            b"long" | b"shor" | b"type" => r.skip(4).map(|_| None)?,
            b"comp" | b"dutc" => r.skip(8).map(|_| None)?,
            b"blob" => {
                let len = r.u32()? as usize;
                r.skip(len).map(|_| None)?
            }
            // An unknown type has an unknown length: nothing after it can be read.
            _ => return None,
        };
        match code {
            b"ptbL" => self.out.entry(file).or_default().parent = value,
            b"ptbN" => self.out.entry(file).or_default().name = value,
            _ => {}
        }
        Some(())
    }
}

/// Big-endian reads over a byte slice.
struct Reader<'a> {
    data: &'a [u8],
    pos: usize,
}

impl<'a> Reader<'a> {
    fn at(data: &'a [u8], pos: usize) -> Option<Self> {
        (pos <= data.len()).then_some(Self { data, pos })
    }

    fn bytes(&mut self, n: usize) -> Option<&'a [u8]> {
        let slice = self.data.get(self.pos..self.pos.checked_add(n)?)?;
        self.pos += n;
        Some(slice)
    }

    fn skip(&mut self, n: usize) -> Option<()> {
        self.bytes(n).map(|_| ())
    }

    fn u8(&mut self) -> Option<u8> {
        Some(self.bytes(1)?[0])
    }

    fn u32(&mut self) -> Option<u32> {
        Some(u32::from_be_bytes(self.bytes(4)?.try_into().ok()?))
    }

    fn utf16(&mut self, units: usize) -> Option<String> {
        let raw = self.bytes(units.checked_mul(2)?)?;
        let units: Vec<u16> = raw.as_chunks::<2>().0.iter().map(|&c| u16::from_be_bytes(c)).collect();
        Some(String::from_utf16_lossy(&units))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn utf16(s: &str) -> Vec<u8> {
        let units: Vec<u16> = s.encode_utf16().collect();
        let mut out = (units.len() as u32).to_be_bytes().to_vec();
        out.extend(units.iter().flat_map(|u| u.to_be_bytes()));
        out
    }

    fn record(file: &str, code: &[u8; 4], kind: &[u8; 4], value: &[u8]) -> Vec<u8> {
        let mut out = utf16(file);
        out.extend_from_slice(code);
        out.extend_from_slice(kind);
        out.extend_from_slice(value);
        out
    }

    /// A store laid out like Finder's: blocks of 256 bytes, the allocator
    /// root in block 0, the DSDB header in block 1 and the tree after it.
    /// `nodes[0]` is the tree root.
    fn store(nodes: &[Vec<u8>]) -> Vec<u8> {
        const BLOCK: usize = 256;
        let count = 2 + nodes.len();
        let addr = |i: usize| ((BLOCK * (i + 1)) | 8) as u32;
        let mut root = (count as u32).to_be_bytes().to_vec();
        root.extend(0u32.to_be_bytes());
        for i in 0..256 {
            root.extend(if i < count { addr(i) } else { 0 }.to_be_bytes());
        }
        root.extend(1u32.to_be_bytes());
        root.push(4);
        root.extend(b"DSDB");
        root.extend(1u32.to_be_bytes());
        let mut master = 2u32.to_be_bytes().to_vec();
        master.extend([0u8; 16]);

        let blocks: Vec<&[u8]> =
            [root.as_slice(), master.as_slice()].into_iter().chain(nodes.iter().map(|n| n.as_slice())).collect();
        // The allocator root is larger than one block; give it its own area at the end.
        let root_offset = BLOCK * (count + 1);
        let mut out = vec![0u8; 4 + root_offset + root.len()];
        out[..4].copy_from_slice(&1u32.to_be_bytes());
        out[4..8].copy_from_slice(b"Bud1");
        out[8..12].copy_from_slice(&(root_offset as u32).to_be_bytes());
        for (i, block) in blocks.iter().enumerate().skip(1) {
            assert!(block.len() <= BLOCK);
            let at = 4 + BLOCK * (i + 1);
            out[at..at + block.len()].copy_from_slice(block);
        }
        out[4 + root_offset..4 + root_offset + root.len()].copy_from_slice(&root);
        out
    }

    fn node(last_child: u32, entries: &[(Option<u32>, Vec<u8>)]) -> Vec<u8> {
        let mut out = last_child.to_be_bytes().to_vec();
        out.extend((entries.len() as u32).to_be_bytes());
        for (child, record) in entries {
            if let Some(child) = child {
                out.extend(child.to_be_bytes());
            }
            out.extend(record);
        }
        out
    }

    #[test]
    fn reads_put_back_records_from_a_leaf() {
        let leaf = node(
            0,
            &[
                (None, record("a 10-11-12.txt", b"ptbL", b"ustr", &utf16("Users/me/Documents/"))),
                (None, record("a 10-11-12.txt", b"ptbN", b"ustr", &utf16("a.txt"))),
                (None, record("b", b"Iloc", b"blob", &[0, 0, 0, 2, 1, 2])),
                (None, record("b", b"ptbL", b"ustr", &utf16("Users/me/"))),
            ],
        );
        let map = put_back(&store(&[leaf]));
        assert_eq!(
            map["a 10-11-12.txt"],
            PutBack { parent: Some("Users/me/Documents/".into()), name: Some("a.txt".into()) }
        );
        assert_eq!(map["b"], PutBack { parent: Some("Users/me/".into()), name: None });
        assert_eq!(map.len(), 2);
    }

    #[test]
    fn walks_internal_nodes() {
        // Block 2 is the root, pointing at leaves in blocks 3 and 4.
        let root = node(4, &[(Some(3), record("m", b"ptbL", b"ustr", &utf16("Users/m/")))]);
        let left = node(0, &[(None, record("a", b"ptbL", b"ustr", &utf16("Users/a/")))]);
        let right = node(0, &[(None, record("z", b"ptbN", b"ustr", &utf16("zed")))]);
        let map = put_back(&store(&[root, left, right]));
        assert_eq!(map["a"].parent.as_deref(), Some("Users/a/"));
        assert_eq!(map["m"].parent.as_deref(), Some("Users/m/"));
        assert_eq!(map["z"].name.as_deref(), Some("zed"));
    }

    #[test]
    fn tolerates_garbage() {
        assert!(put_back(b"").is_empty());
        assert!(put_back(b"\0\0\0\x01Bud1\xff\xff\xff\xff").is_empty());
        let mut data = store(&[node(0, &[(None, record("a", b"ptbL", b"ustr", &utf16("Users/a/")))])]);
        data.truncate(data.len() / 2);
        assert!(put_back(&data).is_empty());
        // A node pointing at itself stops at the depth limit.
        assert!(put_back(&store(&[node(2, &[(Some(2), record("a", b"ptbL", b"ustr", &utf16("x")))])])).is_empty());
    }
}
