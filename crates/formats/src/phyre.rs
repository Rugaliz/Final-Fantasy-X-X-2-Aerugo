//! PhyreEngine clusters (`*.phyre`, D3D11 builds) used by the HD remaster for models, textures, effects and
//! animations (see `docs/formats/phyre.md`).
//!
//! A cluster is a serialized object graph. It carries its own type description (the namespace), so objects
//! are read by class and member name. Pointers are stored as zero and restored from packed fixup tables.

use std::collections::HashMap;
use std::ops::Range;

const MAGIC: &[u8; 4] = b"RYHP";
const NAMESPACE_MARKER: u32 = 0x0102_0304;
const HEADER_WORDS: usize = 21;

#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum PhyreError {
    #[error("not a Phyre cluster")]
    NotACluster,
    #[error("unsupported platform {0:?}")]
    Platform([u8; 4]),
    #[error("file ends early ({0})")]
    Truncated(&'static str),
    #[error("bad {0}")]
    Bad(&'static str),
    #[error("{section} fixups use {used} bytes, header says {declared}")]
    FixupSize { section: &'static str, used: usize, declared: usize },
}

type Result<T> = std::result::Result<T, PhyreError>;

/// The fixed header (`PClusterHeaderD3D11`).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Header {
    pub header_size: u32,
    pub namespace_size: u32,
    pub platform: [u8; 4],
    pub instance_list_count: u32,
    pub array_fixup_size: u32,
    pub array_fixup_count: u32,
    pub pointer_fixup_size: u32,
    pub pointer_fixup_count: u32,
    pub pointer_array_fixup_size: u32,
    pub pointer_array_fixup_count: u32,
    pub pointers_in_arrays_count: u32,
    pub user_fixup_count: u32,
    pub user_fixup_data_size: u32,
    pub total_data_size: u32,
    pub header_class_instance_count: u32,
    pub header_class_child_count: u32,
    pub physics_engine_id: u32,
    pub index_buffer_size: u32,
    pub vertex_buffer_size: u32,
    pub max_texture_buffer_size: u32,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ClassDesc {
    pub name: String,
    /// Index into [`Namespace::classes`].
    pub super_class: Option<usize>,
    pub size: u32,
    pub members: Range<usize>,
    /// Where the base class's part starts inside this class (4 when a vtable precedes it).
    pub base_offset: u32,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MemberDesc {
    pub name: String,
    pub type_id: u32,
    /// Byte offset of the data in an object of the class asked about (base-class parts may be shifted).
    pub offset: u32,
    /// Offset as declared, which fixups use to name the member.
    pub slot: u32,
    pub size: u32,
    pub flags: u32,
    pub fixed_array_size: u32,
    /// Index of the class that declares it.
    pub class: usize,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Namespace {
    pub types: Vec<String>,
    pub classes: Vec<ClassDesc>,
    pub members: Vec<MemberDesc>,
}

/// What a member's type id names.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TypeRef<'a> {
    Primitive(&'a str),
    Class(usize),
    Unknown(u32),
}

impl Namespace {
    pub fn class_index(&self, name: &str) -> Option<usize> {
        self.classes.iter().position(|c| c.name == name)
    }

    pub fn type_of(&self, type_id: u32) -> TypeRef<'_> {
        let id = type_id as usize;
        // Ids count the primitive types, then one reserved id, then the classes.
        if let Some(t) = self.types.get(id) {
            TypeRef::Primitive(t)
        } else if let Some(c) = (id - self.types.len()).checked_sub(1).filter(|&c| c < self.classes.len()) {
            TypeRef::Class(c)
        } else {
            TypeRef::Unknown(type_id)
        }
    }

    pub fn type_name(&self, type_id: u32) -> &str {
        match self.type_of(type_id) {
            TypeRef::Primitive(t) => t,
            TypeRef::Class(c) => &self.classes[c].name,
            TypeRef::Unknown(_) => "?",
        }
    }

    /// The class and its bases, most derived first.
    pub fn lineage(&self, class: usize) -> impl Iterator<Item = usize> + '_ {
        let mut next = Some(class);
        let mut guard = 0;
        std::iter::from_fn(move || {
            let c = next?;
            guard += 1;
            next = if guard > 64 { None } else { self.classes.get(c)?.super_class };
            Some(c)
        })
    }

    /// Shift of members declared in `declaring` when they are part of an object of class `class`: the sum of
    /// base offsets on the way up.
    pub fn data_shift(&self, class: usize, declaring: usize) -> u32 {
        let mut shift = 0;
        for c in self.lineage(class) {
            if c == declaring {
                return shift;
            }
            shift += self.classes[c].base_offset;
        }
        0
    }

    pub fn is_a(&self, class: usize, base: &str) -> bool {
        self.lineage(class).any(|c| self.classes[c].name == base)
    }

    /// A member declared by `class` or one of its bases.
    pub fn member(&self, class: usize, name: &str) -> Option<&MemberDesc> {
        self.lineage(class)
            .find_map(|c| self.members[self.classes[c].members.clone()].iter().find(|m| m.name == name))
    }

    /// All members of a class, bases first.
    pub fn all_members(&self, class: usize) -> Vec<&MemberDesc> {
        let chain: Vec<usize> = self.lineage(class).collect();
        chain.iter().rev().flat_map(|&c| self.members[self.classes[c].members.clone()].iter()).collect()
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InstanceList {
    pub class: usize,
    pub count: u32,
    pub object_size: usize,
    /// Absolute byte ranges in the file.
    pub objects: Range<usize>,
    pub arrays: Range<usize>,
}

/// An object: instance list and index within it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct ObjectId {
    pub list: u32,
    pub index: u32,
}

/// Where a pointer field points.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Pointer {
    /// Into an object (possibly past its start: `offset` bytes in). `count` is the element count for pointers
    /// to arrays of objects, 0 otherwise.
    Object { target: ObjectId, offset: u32, count: u32 },
    /// A user fixup (an external reference such as a class or asset name).
    User(usize),
}

/// An array stored in a list's array region.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ArrayRef {
    /// Element count; 0 when the file does not record it (strings).
    pub count: u32,
    /// Absolute byte offset of the first element.
    pub start: usize,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UserFixup {
    pub type_id: u32,
    pub data: Range<usize>,
}

/// A pointer or array slot: object, byte offset in it, element index for arrays of pointers.
type Slot = (ObjectId, u32, u32);

#[derive(Debug, Clone)]
pub struct Cluster {
    data: Vec<u8>,
    pub header: Header,
    pub namespace: Namespace,
    pub lists: Vec<InstanceList>,
    pub user_fixups: Vec<UserFixup>,
    pointers: HashMap<Slot, Pointer>,
    arrays: HashMap<(ObjectId, u32), ArrayRef>,
    pointer_arrays: HashMap<(ObjectId, u32), ArrayRef>,
    /// Bytes after the fixups: index buffer, vertex buffer, texture data.
    pub index_buffer: Range<usize>,
    pub vertex_buffer: Range<usize>,
    pub texture_data: Range<usize>,
}

struct Reader<'a> {
    d: &'a [u8],
    p: usize,
}

impl<'a> Reader<'a> {
    fn new(d: &'a [u8], p: usize) -> Self {
        Reader { d, p }
    }

    fn u8(&mut self) -> Result<u8> {
        let b = *self.d.get(self.p).ok_or(PhyreError::Truncated("fixups"))?;
        self.p += 1;
        Ok(b)
    }

    fn u32(&mut self, what: &'static str) -> Result<u32> {
        let b = self.d.get(self.p..self.p + 4).ok_or(PhyreError::Truncated(what))?;
        self.p += 4;
        Ok(u32::from_le_bytes([b[0], b[1], b[2], b[3]]))
    }

    /// LEB128.
    fn var(&mut self) -> Result<u32> {
        let mut v: u64 = 0;
        let mut shift = 0;
        loop {
            let b = self.u8()?;
            if shift < 35 {
                v |= u64::from(b & 0x7F) << shift;
            }
            shift += 7;
            if b & 0x80 == 0 {
                return Ok(v as u32);
            }
            if shift > 70 {
                return Err(PhyreError::Bad("varint"));
            }
        }
    }

    fn index(&mut self, n: u32) -> Result<u32> {
        if n < 0x100 { self.u8().map(u32::from) } else { self.var() }
    }

    fn skip(&mut self, n: usize, what: &'static str) -> Result<Range<usize>> {
        let end = self.p.checked_add(n).filter(|&e| e <= self.d.len()).ok_or(PhyreError::Truncated(what))?;
        let r = self.p..end;
        self.p = end;
        Ok(r)
    }
}

fn cstr(d: &[u8], at: usize) -> String {
    let rest = d.get(at..).unwrap_or(&[]);
    let end = rest.iter().position(|&b| b == 0).unwrap_or(rest.len());
    String::from_utf8_lossy(&rest[..end]).into_owned()
}

/// A member reference in a fixup: global member index or byte offset.
#[derive(Debug, Clone, Copy)]
enum MemberRef {
    Index(u32),
    Offset(u32),
}

fn member_ref(v: u32) -> MemberRef {
    if v & 1 == 0 { MemberRef::Index(v >> 1) } else { MemberRef::Offset(v >> 1) }
}

/// Object indices chosen by a selector mode.
fn select(r: &mut Reader<'_>, mode: u8, n: u32) -> Result<Vec<u32>> {
    let cap = |k: u32| (k as usize).min(n as usize + 1);
    match mode {
        0 => Ok((0..n).collect()),
        2 => {
            let k = r.var()?;
            let mut v = Vec::with_capacity(cap(k));
            for _ in 0..k {
                v.push(r.index(n)?);
            }
            Ok(v)
        }
        3 => {
            let k = r.var()?;
            let mut excluded = Vec::with_capacity(cap(k));
            for _ in 0..k {
                excluded.push(r.index(n)?);
            }
            let mut next = 0;
            let mut v = Vec::new();
            for e in excluded {
                v.extend(next..e.min(n));
                next = next.max(e.saturating_add(1));
            }
            v.extend(next..n);
            Ok(v)
        }
        4 => {
            let bits = r.skip((n as usize).div_ceil(8), "fixup bitmask")?;
            let bits = &r.d[bits];
            Ok((0..n).filter(|&i| bits[(i >> 3) as usize] >> (i & 7) & 1 != 0).collect())
        }
        6 => {
            let (start, stride, k) = (r.var()?, r.var()?, r.var()?);
            if k > n {
                return Err(PhyreError::Bad("fixup sequence"));
            }
            Ok((0..k).map(|j| start.wrapping_add(j.wrapping_mul(stride))).collect())
        }
        _ => Err(PhyreError::Bad("fixup mode")),
    }
}

/// Array records: (member, object, count, offset).
fn unpack_arrays(r: &mut Reader<'_>, records: u32, n: u32) -> Result<Vec<(MemberRef, u32, u32, u32)>> {
    let mut out = Vec::with_capacity((records as usize).min(1 << 16));
    while out.len() < records as usize {
        let h = r.u8()?;
        let mode = h & 7;
        let flags = (h & 0xF9) | if n == 1 { 3 } else { 1 };
        let member = member_ref(r.var()?);
        let inline = |r: &mut Reader<'_>| -> Result<(u32, u32)> {
            let count = if flags & 8 == 0 { r.var()? } else { 0 };
            Ok((count, r.var()?))
        };
        match mode {
            0 | 2 | 3 | 4 | 6 => {
                for i in select(r, mode, n)? {
                    let (count, offset) = inline(r)?;
                    out.push((member, i, count, offset));
                }
            }
            5 => {
                for _ in 0..r.var()? {
                    let i = if flags & 2 == 0 { r.var()? } else { 0 };
                    let (count, offset) = inline(r)?;
                    out.push((member, i, count, offset));
                    if out.len() > records as usize {
                        break;
                    }
                }
            }
            1 => {
                let mut got = 0usize;
                while got < n as usize {
                    let sub = r.u8()?;
                    let (count, offset) = inline(r)?;
                    // Every group consumes at least its sub-mode byte, so corrupt data ends in `Truncated`.
                    let idx = select(r, sub, n)?;
                    got += idx.len();
                    out.extend(idx.into_iter().map(|i| (member, i, count, offset)));
                    if got > n as usize {
                        return Err(PhyreError::Bad("fixup group"));
                    }
                }
            }
            _ => return Err(PhyreError::Bad("fixup mode")),
        }
        if out.len() > records as usize {
            return Err(PhyreError::Bad("fixup record count"));
        }
    }
    Ok(out)
}

#[derive(Debug, Clone, Copy)]
struct PointerRecord {
    member: MemberRef,
    object: u32,
    target_object: u32,
    target_list: u32,
    target_offset: u32,
    element: u32,
    user: Option<u32>,
}

fn pointer_tail(r: &mut Reader<'_>, flags: u8, rec: &mut PointerRecord) -> Result<()> {
    let mut read_target = flags & 0x10 != 0;
    if flags & 0x10 == 0 {
        let u = r.var()?;
        rec.user = u.checked_sub(1);
        read_target = u == 0;
    }
    if read_target {
        rec.target_object = r.var()?;
        if flags & 0x20 == 0 {
            rec.target_list = r.var()?;
        }
        if flags & 0x40 == 0 {
            rec.target_offset = r.var()?;
        }
    }
    if flags & 8 == 0 {
        rec.element = r.var()?;
    }
    Ok(())
}

fn unpack_pointers(r: &mut Reader<'_>, records: u32, n: u32) -> Result<Vec<PointerRecord>> {
    let mut out = Vec::with_capacity((records as usize).min(1 << 16));
    while out.len() < records as usize {
        let h = r.u8()?;
        let mode = h & 7;
        let flags = (h & 0xF9) | if n == 1 { 3 } else { 1 };
        let member = member_ref(r.var()?);
        let mut template = PointerRecord {
            member,
            object: 0,
            target_object: 0,
            target_list: 0,
            target_offset: 0,
            element: 0,
            user: None,
        };
        if flags & 0x20 != 0 {
            template.target_list = r.var()?;
        }
        match mode {
            0 | 2 | 3 | 4 | 6 => {
                for i in select(r, mode, n)? {
                    let mut rec = PointerRecord { object: i, ..template };
                    pointer_tail(r, flags, &mut rec)?;
                    out.push(rec);
                }
            }
            5 => {
                for _ in 0..r.var()? {
                    let mut rec = template;
                    if flags & 2 == 0 {
                        rec.object = r.var()?;
                    }
                    pointer_tail(r, flags, &mut rec)?;
                    out.push(rec);
                    if out.len() > records as usize {
                        break;
                    }
                }
            }
            1 => {
                let mut got = 0usize;
                while got < n as usize {
                    let sub = r.u8()?;
                    let mut group = template;
                    pointer_tail(r, flags, &mut group)?;
                    let idx = select(r, sub, n)?;
                    got += idx.len();
                    out.extend(idx.into_iter().map(|i| PointerRecord { object: i, ..group }));
                    if got > n as usize {
                        return Err(PhyreError::Bad("fixup group"));
                    }
                }
            }
            _ => return Err(PhyreError::Bad("fixup mode")),
        }
        if out.len() > records as usize {
            return Err(PhyreError::Bad("fixup record count"));
        }
    }
    Ok(out)
}

impl Cluster {
    pub fn parse(data: Vec<u8>) -> Result<Cluster> {
        if data.len() >= 16 && &data[0..4] == b"PHYR" {
            // Big-endian PS3 build left in a few folders; the PC game does not load these either.
            return Err(PhyreError::Platform(data[12..16].try_into().expect("4 bytes")));
        }
        if data.len() < HEADER_WORDS * 4 || &data[0..4] != MAGIC {
            return Err(PhyreError::NotACluster);
        }
        let mut r = Reader::new(&data, 4);
        let mut w = [0u32; HEADER_WORDS];
        for v in w.iter_mut().skip(1) {
            *v = r.u32("header")?;
        }
        let platform = data[12..16].try_into().expect("4 bytes");
        if &platform != b"11XD" {
            return Err(PhyreError::Platform(platform));
        }
        let header = Header {
            header_size: w[1],
            namespace_size: w[2],
            platform,
            instance_list_count: w[4],
            array_fixup_size: w[5],
            array_fixup_count: w[6],
            pointer_fixup_size: w[7],
            pointer_fixup_count: w[8],
            pointer_array_fixup_size: w[9],
            pointer_array_fixup_count: w[10],
            pointers_in_arrays_count: w[11],
            user_fixup_count: w[12],
            user_fixup_data_size: w[13],
            total_data_size: w[14],
            header_class_instance_count: w[15],
            header_class_child_count: w[16],
            physics_engine_id: w[17],
            index_buffer_size: w[18],
            vertex_buffer_size: w[19],
            max_texture_buffer_size: w[20],
        };

        let ns_start = header.header_size as usize;
        let namespace = Self::parse_namespace(&data, ns_start)?;
        let mut r = Reader::new(&data, ns_start + header.namespace_size as usize);

        // Instance list headers, then their data.
        let n_lists = header.instance_list_count as usize;
        if n_lists > data.len() / 36 {
            return Err(PhyreError::Truncated("instance lists"));
        }
        let mut raw = Vec::with_capacity(n_lists);
        for _ in 0..n_lists {
            let mut h = [0u32; 9];
            for v in &mut h {
                *v = r.u32("instance lists")?;
            }
            raw.push(h);
        }
        let mut lists = Vec::with_capacity(n_lists);
        for h in &raw {
            let class = (h[0] as usize).checked_sub(1).filter(|&c| c < namespace.classes.len());
            let class = class.ok_or(PhyreError::Bad("instance list class"))?;
            let (count, size, objects_size) = (h[1], h[2] as usize, h[3] as usize);
            if objects_size > size || count == 0 && objects_size != 0 || count as usize > data.len() {
                return Err(PhyreError::Bad("instance list sizes"));
            }
            let all = r.skip(size, "instance data")?;
            let object_size = if count == 0 { 0 } else { objects_size / count as usize };
            lists.push(InstanceList {
                class,
                count,
                object_size,
                objects: all.start..all.start + objects_size,
                arrays: all.start + objects_size..all.end,
            });
        }

        // User fixups.
        let pool = r.skip(header.user_fixup_data_size as usize, "user fixup data")?;
        let mut user_fixups = Vec::new();
        for _ in 0..header.user_fixup_count.min(1 << 20) {
            let type_id = r.u32("user fixups")?;
            let size = r.u32("user fixups")? as usize;
            let offset = r.u32("user fixups")? as usize;
            let start = pool.start + offset;
            let end =
                start.checked_add(size).filter(|&e| e <= pool.end).ok_or(PhyreError::Bad("user fixup"))?;
            user_fixups.push(UserFixup { type_id, data: start..end });
        }
        // Header classes are not needed to read objects.
        r.skip(header.header_class_instance_count as usize * 4, "header classes")?;
        r.skip(header.header_class_child_count as usize * 16, "header classes")?;

        let mut cluster = Cluster {
            header,
            namespace,
            lists,
            user_fixups,
            pointers: HashMap::new(),
            arrays: HashMap::new(),
            pointer_arrays: HashMap::new(),
            index_buffer: 0..0,
            vertex_buffer: 0..0,
            texture_data: 0..0,
            data: Vec::new(),
        };

        // Fixup sections, stored as pointer arrays, pointers, arrays.
        let mut sections = Vec::new();
        for (name, size) in [
            ("pointer array", header.pointer_array_fixup_size),
            ("pointer", header.pointer_fixup_size),
            ("array", header.array_fixup_size),
        ] {
            sections.push((name, r.skip(size as usize, "fixups")?));
        }
        for (name, range) in sections {
            let mut fr = Reader::new(&data[..range.end], range.start);
            for (li, h) in raw.iter().enumerate() {
                let n = h[1];
                match name {
                    "pointer" => {
                        for rec in unpack_pointers(&mut fr, h[7], n)? {
                            cluster.add_pointer(li, rec)?;
                        }
                    }
                    _ => {
                        let records = if name == "array" { h[6] } else { h[8] };
                        for (m, i, count, offset) in unpack_arrays(&mut fr, records, n)? {
                            cluster.add_array(li, name == "array", m, i, count, offset)?;
                        }
                    }
                }
            }
            if fr.p != range.end {
                return Err(PhyreError::FixupSize {
                    section: name,
                    used: fr.p - range.start,
                    declared: range.len(),
                });
            }
        }

        let rest = r.p..data.len();
        let ib = header.index_buffer_size as usize;
        let vb = header.vertex_buffer_size as usize;
        if ib + vb > rest.len() {
            return Err(PhyreError::Truncated("buffers"));
        }
        cluster.index_buffer = rest.start..rest.start + ib;
        cluster.vertex_buffer = rest.start + ib..rest.start + ib + vb;
        cluster.texture_data = rest.start + ib + vb..rest.end;
        cluster.data = data;
        Ok(cluster)
    }

    fn parse_namespace(data: &[u8], start: usize) -> Result<Namespace> {
        let mut r = Reader::new(data, start);
        if r.u32("namespace")? != NAMESPACE_MARKER {
            return Err(PhyreError::Bad("namespace marker"));
        }
        let _size = r.u32("namespace")?;
        let n_types = r.u32("namespace")? as usize;
        let n_classes = r.u32("namespace")? as usize;
        let n_members = r.u32("namespace")? as usize;
        let strings_size = r.u32("namespace")? as usize;
        let _buffers = (r.u32("namespace")?, r.u32("namespace")?);
        let needed = n_types * 4 + n_classes * 36 + n_members * 24 + strings_size;
        if needed > data.len().saturating_sub(r.p) {
            return Err(PhyreError::Truncated("namespace"));
        }
        let strings = r.p + n_types * 4 + n_classes * 36 + n_members * 24;
        let name = |off: u32| -> String {
            if (off as usize) < strings_size {
                cstr(&data[..strings + strings_size], strings + off as usize)
            } else {
                String::new()
            }
        };
        let mut types = Vec::with_capacity(n_types);
        for _ in 0..n_types {
            types.push(name(r.u32("namespace")?));
        }
        let mut classes = Vec::with_capacity(n_classes);
        let mut first = 0usize;
        for _ in 0..n_classes {
            let mut c = [0u32; 9];
            for v in &mut c {
                *v = r.u32("namespace")?;
            }
            let count = c[3] as usize;
            let members = first..first + count;
            first += count;
            classes.push(ClassDesc {
                name: name(c[2]),
                super_class: (c[0] as usize).checked_sub(1).filter(|&s| s < n_classes),
                size: c[1] & 0x0FFF_FFFF,
                members,
                base_offset: c[4],
            });
        }
        if first != n_members {
            return Err(PhyreError::Bad("namespace member count"));
        }
        let mut members = Vec::with_capacity(n_members);
        for (ci, c) in classes.iter().enumerate() {
            for _ in c.members.clone() {
                let mut m = [0u32; 6];
                for v in &mut m {
                    *v = r.u32("namespace")?;
                }
                members.push(MemberDesc {
                    name: name(m[0]),
                    type_id: m[1],
                    offset: m[2],
                    slot: m[2],
                    size: m[3],
                    flags: m[4],
                    fixed_array_size: m[5],
                    class: ci,
                });
            }
        }
        Ok(Namespace { types, classes, members })
    }

    /// Byte offset inside an object of the member a fixup names.
    fn member_offset(&self, list: usize, m: MemberRef) -> Result<u32> {
        let l = &self.lists[list];
        let off = match m {
            MemberRef::Offset(o) => o,
            MemberRef::Index(i) => {
                let md = self.namespace.members.get(i as usize).ok_or(PhyreError::Bad("fixup member"))?;
                if !self.namespace.lineage(l.class).any(|c| c == md.class) {
                    return Err(PhyreError::Bad("fixup member class"));
                }
                md.offset
            }
        };
        if off as usize >= l.object_size.max(1) {
            return Err(PhyreError::Bad("fixup offset"));
        }
        Ok(off)
    }

    fn object_id(&self, list: usize, index: u32) -> Result<ObjectId> {
        if index >= self.lists[list].count {
            return Err(PhyreError::Bad("fixup object"));
        }
        Ok(ObjectId { list: list as u32, index })
    }

    fn add_pointer(&mut self, list: usize, rec: PointerRecord) -> Result<()> {
        let obj = self.object_id(list, rec.object)?;
        let offset = self.member_offset(list, rec.member)?;
        let ptr = match rec.user {
            Some(u) => {
                if u as usize >= self.user_fixups.len() {
                    return Err(PhyreError::Bad("user fixup index"));
                }
                Pointer::User(u as usize)
            }
            None => {
                let tl = rec.target_list as usize;
                if tl >= self.lists.len() || rec.target_object >= self.lists[tl].count {
                    return Err(PhyreError::Bad("pointer target"));
                }
                let target = ObjectId { list: tl as u32, index: rec.target_object };
                Pointer::Object { target, offset: rec.target_offset, count: rec.element }
            }
        };
        // Pointers inside pointer arrays are keyed by element; plain pointers use element 0.
        let element = if self.pointer_arrays.contains_key(&(obj, offset)) { rec.element } else { 0 };
        self.pointers.insert((obj, offset, element), ptr);
        Ok(())
    }

    fn add_array(
        &mut self,
        list: usize,
        data: bool,
        m: MemberRef,
        i: u32,
        count: u32,
        offset: u32,
    ) -> Result<()> {
        let obj = self.object_id(list, i)?;
        let member = self.member_offset(list, m)?;
        let arrays = &self.lists[list].arrays;
        let start = arrays.start + offset as usize;
        if start > arrays.end {
            return Err(PhyreError::Bad("array offset"));
        }
        let entry = ArrayRef { count, start };
        if data {
            self.arrays.insert((obj, member), entry);
        } else {
            self.pointer_arrays.insert((obj, member), entry);
        }
        Ok(())
    }

    pub fn data(&self) -> &[u8] {
        &self.data
    }

    pub fn list(&self, id: ObjectId) -> &InstanceList {
        &self.lists[id.list as usize]
    }

    pub fn class_name(&self, id: ObjectId) -> &str {
        &self.namespace.classes[self.list(id).class].name
    }

    /// All objects whose class is `class` or derives from it.
    pub fn objects(&self, class: &str) -> Vec<ObjectId> {
        let mut out = Vec::new();
        for (li, l) in self.lists.iter().enumerate() {
            if self.namespace.is_a(l.class, class) {
                out.extend((0..l.count).map(|index| ObjectId { list: li as u32, index }));
            }
        }
        out
    }

    pub fn object_bytes(&self, id: ObjectId) -> &[u8] {
        let l = self.list(id);
        let start = l.objects.start + id.index as usize * l.object_size;
        &self.data[start..start + l.object_size]
    }

    /// A member by name; `a.b` names member `b` of the embedded struct `a`. The offset is from the object start.
    pub fn member(&self, id: ObjectId, name: &str) -> Option<MemberDesc> {
        let mut class = self.list(id).class;
        let (mut offset, mut slot) = (0, 0);
        let mut parts = name.split('.').peekable();
        while let Some(part) = parts.next() {
            let m = self.namespace.member(class, part)?;
            let shift = self.namespace.data_shift(class, m.class);
            if parts.peek().is_none() {
                return Some(MemberDesc {
                    offset: offset + shift + m.offset,
                    slot: slot + m.offset,
                    ..m.clone()
                });
            }
            offset += shift + m.offset;
            slot += m.offset;
            class = match self.namespace.type_of(m.type_id) {
                TypeRef::Class(c) => c,
                _ => return None,
            };
        }
        None
    }

    /// Raw bytes of a member.
    pub fn field(&self, id: ObjectId, name: &str) -> Option<&[u8]> {
        let m = self.member(id, name)?;
        self.object_bytes(id).get(m.offset as usize..(m.offset + m.size) as usize)
    }

    pub fn u32(&self, id: ObjectId, name: &str) -> Option<u32> {
        let b = self.field(id, name)?;
        Some(match b.len() {
            1 => u32::from(b[0]),
            2 => u32::from(u16::from_le_bytes([b[0], b[1]])),
            _ => u32::from_le_bytes(b.get(..4)?.try_into().ok()?),
        })
    }

    pub fn f32(&self, id: ObjectId, name: &str) -> Option<f32> {
        let b = self.field(id, name)?;
        Some(f32::from_le_bytes(b.get(..4)?.try_into().ok()?))
    }

    /// `n` floats starting at a member (vectors and matrices are stored as plain floats).
    pub fn floats(&self, id: ObjectId, name: &str, n: usize) -> Option<Vec<f32>> {
        let m = self.member(id, name)?;
        let b = self.object_bytes(id).get(m.offset as usize..m.offset as usize + 4 * n)?;
        Some(b.chunks_exact(4).map(|c| f32::from_le_bytes([c[0], c[1], c[2], c[3]])).collect())
    }

    /// Pointer stored at a byte offset of an object (element for arrays of pointers).
    pub fn pointer_at(&self, id: ObjectId, offset: u32, element: u32) -> Option<Pointer> {
        self.pointers.get(&(id, offset, element)).copied()
    }

    /// Pointer member by name; for `PArray<T>`-style members name the `m_els` slot via [`Self::array_ptr`].
    pub fn pointer(&self, id: ObjectId, name: &str) -> Option<Pointer> {
        let m = self.member(id, name)?;
        self.pointer_at(id, m.slot, 0)
    }

    /// Target object of a pointer member.
    pub fn target(&self, id: ObjectId, name: &str) -> Option<ObjectId> {
        match self.pointer(id, name)? {
            Pointer::Object { target, .. } => Some(target),
            Pointer::User(_) => None,
        }
    }

    /// Data array stored for the member at `offset` (byte offset in the object).
    pub fn array_at(&self, id: ObjectId, offset: u32) -> Option<ArrayRef> {
        self.arrays.get(&(id, offset)).copied()
    }

    /// Data array of a member (`PArray<...>` or raw pointer to data). The array's pointer slot is at the
    /// member's offset plus `slot` (4 for `PArray`, whose count comes first).
    pub fn array(&self, id: ObjectId, name: &str, slot: u32) -> Option<ArrayRef> {
        let m = self.member(id, name)?;
        self.array_at(id, m.slot + slot)
    }

    /// Element count stored in a `PArray` member (its first word).
    pub fn array_count(&self, id: ObjectId, name: &str) -> Option<u32> {
        let b = self.field(id, name)?;
        Some(u32::from_le_bytes(b.get(..4)?.try_into().ok()?))
    }

    /// Bytes of a data array, `len` bytes from its start (clamped to the list's array region).
    pub fn array_bytes(&self, id: ObjectId, a: ArrayRef, len: usize) -> &[u8] {
        let end = (a.start + len).min(self.list(id).arrays.end);
        &self.data[a.start.min(end)..end]
    }

    /// Objects referenced by a `PArray<T *>` / `PArray<T>` member.
    pub fn array_targets(&self, id: ObjectId, name: &str) -> Vec<ObjectId> {
        self.array_pointers(id, name)
            .into_iter()
            .filter_map(|p| match p {
                Pointer::Object { target, .. } => Some(target),
                Pointer::User(_) => None,
            })
            .collect()
    }

    /// Elements of an array member as pointers: arrays of pointers yield one pointer per element (external
    /// ones as user fixups); arrays of objects yield one pointer per object.
    pub fn array_pointers(&self, id: ObjectId, name: &str) -> Vec<Pointer> {
        let Some(m) = self.member(id, name) else { return Vec::new() };
        // `PArray` keeps its element pointer after the count; other containers keep it first.
        for slot in [m.slot + 4, m.slot] {
            if slot >= m.slot + m.size.max(4) {
                continue;
            }
            if self.pointer_arrays.contains_key(&(id, slot)) {
                let mut v: Vec<(u32, Pointer)> = self
                    .pointers
                    .iter()
                    .filter(|((o, off, _), _)| *o == id && *off == slot)
                    .map(|(&(_, _, e), &p)| (e, p))
                    .collect();
                v.sort_by_key(|&(e, _)| e);
                return v.into_iter().map(|(_, p)| p).collect();
            }
            if let Some(Pointer::Object { target, offset, count }) = self.pointer_at(id, slot, 0) {
                let n = if count > 0 { count } else { self.array_count(id, name).unwrap_or(1) };
                return (0..n)
                    .map(|k| ObjectId { list: target.list, index: target.index + k })
                    .filter(|o| o.index < self.list(*o).count)
                    .map(|target| Pointer::Object { target, offset, count: 0 })
                    .collect();
            }
        }
        Vec::new()
    }

    /// An external asset named by a user fixup: (asset id, asset class), e.g. a texture or effect in another
    /// cluster.
    pub fn import(&self, u: usize) -> Option<(String, String)> {
        let f = self.user_fixups.get(u)?;
        if self.namespace.type_name(f.type_id) != "PAssetReferenceImport" {
            return None;
        }
        let d = &self.data[f.data.clone()];
        let index = match d.len() {
            1 => u32::from(d[0]),
            2 => u32::from(u16::from_be_bytes([d[0], d[1]])),
            4 => u32::from_be_bytes([d[0], d[1], d[2], d[3]]),
            _ => return None,
        };
        let list = self.objects("PAssetReferenceImport");
        let obj = *list.get(index as usize)?;
        let class = match self.pointer(obj, "m_targetAssetType") {
            Some(Pointer::User(t)) => self.user_string(t),
            _ => String::new(),
        };
        Some((self.string(obj, "m_id")?, class))
    }

    /// A NUL-terminated string member (`PString` or `PChar *`).
    pub fn string(&self, id: ObjectId, name: &str) -> Option<String> {
        let m = self.member(id, name)?;
        if let Some(a) = self.array_at(id, m.slot) {
            let l = self.list(id);
            return Some(cstr(&self.data[..l.arrays.end], a.start));
        }
        match self.pointer_at(id, m.slot, 0)? {
            Pointer::User(u) => Some(self.user_string(u)),
            Pointer::Object { .. } => None,
        }
    }

    pub fn user_string(&self, u: usize) -> String {
        self.user_fixups.get(u).map(|f| cstr(&self.data[..f.data.end], f.data.start)).unwrap_or_default()
    }

    pub fn index_buffer(&self) -> &[u8] {
        &self.data[self.index_buffer.clone()]
    }

    pub fn vertex_buffer(&self) -> &[u8] {
        &self.data[self.vertex_buffer.clone()]
    }

    pub fn texture_data(&self) -> &[u8] {
        &self.data[self.texture_data.clone()]
    }

    pub fn pointer_count(&self) -> usize {
        self.pointers.len()
    }

    pub fn array_count_total(&self) -> usize {
        self.arrays.len() + self.pointer_arrays.len()
    }

    /// Every pointer slot of an object: (byte offset, element, target).
    pub fn pointers_of(&self, id: ObjectId) -> Vec<(u32, u32, Pointer)> {
        let mut v: Vec<_> = self
            .pointers
            .iter()
            .filter(|((o, ..), _)| *o == id)
            .map(|(&(_, off, el), &p)| (off, el, p))
            .collect();
        v.sort_by_key(|&(o, e, _)| (o, e));
        v
    }

    /// Every data array of an object: (byte offset, array).
    pub fn arrays_of(&self, id: ObjectId) -> Vec<(u32, ArrayRef)> {
        let mut v: Vec<_> =
            self.arrays.iter().filter(|((o, _), _)| *o == id).map(|(&(_, off), &a)| (off, a)).collect();
        v.sort_by_key(|&(o, _)| o);
        v
    }
}

/// One member of an object, formatted for display.
#[derive(Debug, Clone, PartialEq)]
pub struct FieldView {
    pub name: String,
    pub type_name: String,
    pub offset: u32,
    pub value: String,
}

impl Cluster {
    fn format_pointer(&self, p: Pointer) -> String {
        match p {
            Pointer::Object { target, offset, count } => {
                let mut s = format!("-> {}[{}]", self.class_name(target), target.index);
                if offset != 0 {
                    s.push_str(&format!("+{offset:#x}"));
                }
                if count > 1 {
                    s.push_str(&format!(" ×{count}"));
                }
                s
            }
            Pointer::User(u) => format!("-> \"{}\"", self.user_string(u)),
        }
    }

    /// Members of an object with decoded values: numbers for primitive types, pointer targets and array sizes
    /// for references, raw bytes otherwise.
    pub fn describe(&self, id: ObjectId) -> Vec<FieldView> {
        let mut out = Vec::new();
        self.describe_into(id, self.list(id).class, (0, 0), "", 0, &mut out);
        out
    }

    fn describe_into(
        &self,
        id: ObjectId,
        class: usize,
        (base, base_slot): (u32, u32),
        prefix: &str,
        depth: u32,
        out: &mut Vec<FieldView>,
    ) {
        let ns = &self.namespace;
        let bytes = self.object_bytes(id);
        for m in ns.all_members(class) {
            let offset = base + ns.data_shift(class, m.class) + m.offset;
            let member_slot = base_slot + m.offset;
            let start = offset as usize;
            let raw = bytes.get(start..start + m.size as usize).unwrap_or(&[]);
            let type_name = ns.type_name(m.type_id).to_owned();
            let name = format!("{prefix}{}", m.name);
            // Pointer and array slots at this member (PArray keeps its pointer 4 bytes in).
            let mut refs = Vec::new();
            for slot in [member_slot, member_slot + 4] {
                if slot >= member_slot + m.size.max(1) {
                    continue;
                }
                if let Some(a) = self.array_at(id, slot) {
                    refs.push(if a.count > 0 { format!("[{} elements]", a.count) } else { "[data]".into() });
                    if type_name == "PString" || type_name == "PChar" {
                        refs.push(format!("\"{}\"", cstr(&self.data[..self.list(id).arrays.end], a.start)));
                    }
                }
                if let Some(p) = self.pointer_at(id, slot, 0) {
                    refs.push(self.format_pointer(p));
                }
                if self.pointer_arrays.contains_key(&(id, slot)) {
                    let n = self.pointers.keys().filter(|(o, off, _)| *o == id && *off == slot).count();
                    refs.push(format!("[{n} pointers]"));
                }
            }
            if refs.is_empty()
                && depth < 4
                && let TypeRef::Class(c) = ns.type_of(m.type_id)
                && !ns.all_members(c).is_empty()
                && ns.classes[c].size <= m.size
            {
                self.describe_into(id, c, (offset, member_slot), &format!("{name}."), depth + 1, out);
                continue;
            }
            let value = if !refs.is_empty() {
                refs.join(" ")
            } else {
                format_primitive(&type_name, raw, m.fixed_array_size)
            };
            out.push(FieldView { name, type_name, offset, value });
        }
    }
}

fn format_primitive(type_name: &str, raw: &[u8], fixed: u32) -> String {
    let words = |f: &dyn Fn(&[u8]) -> String, w: usize| -> String {
        let v: Vec<String> = raw.chunks_exact(w).take(16).map(f).collect();
        if v.len() == 1 { v[0].clone() } else { format!("[{}]", v.join(", ")) }
    };
    match type_name {
        "float" => words(&|c| format!("{}", f32::from_le_bytes([c[0], c[1], c[2], c[3]])), 4),
        "PInt32" => words(&|c| format!("{}", i32::from_le_bytes([c[0], c[1], c[2], c[3]])), 4),
        "PUInt16" | "PInt16" => words(&|c| format!("{}", u16::from_le_bytes([c[0], c[1]])), 2),
        "PUInt8" | "PInt8" | "bool" if fixed <= 1 && raw.len() == 1 => format!("{}", raw[0]),
        "PChar" if fixed > 1 => {
            let end = raw.iter().position(|&b| b == 0).unwrap_or(raw.len());
            format!("\"{}\"", String::from_utf8_lossy(&raw[..end]))
        }
        _ if raw.len() == 4 => format!("{}", u32::from_le_bytes([raw[0], raw[1], raw[2], raw[3]])),
        _ if raw.len().is_multiple_of(4) && raw.iter().any(|&b| b != 0) && raw.len() <= 64 => {
            let v: Vec<String> = raw
                .chunks_exact(4)
                .map(|c| {
                    let f = f32::from_le_bytes([c[0], c[1], c[2], c[3]]);
                    if f.is_finite() && (f == 0.0 || f.abs() > 1e-6 && f.abs() < 1e7) {
                        format!("{f}")
                    } else {
                        format!("{:#x}", u32::from_le_bytes([c[0], c[1], c[2], c[3]]))
                    }
                })
                .collect();
            format!("[{}]", v.join(", "))
        }
        _ => {
            let hex: Vec<String> = raw.iter().take(32).map(|b| format!("{b:02x}")).collect();
            hex.join(" ") + if raw.len() > 32 { " …" } else { "" }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use proptest::prelude::*;

    #[test]
    fn leb128_and_member_refs() {
        let d = [0x05, 0xE5, 0x8E, 0x26];
        let mut r = Reader::new(&d, 0);
        assert_eq!(r.var().unwrap(), 5);
        assert_eq!(r.var().unwrap(), 624_485);
        assert!(matches!(member_ref(8), MemberRef::Index(4)));
        assert!(matches!(member_ref(49), MemberRef::Offset(24)));
    }

    #[test]
    fn selectors() {
        // list [1, 3]
        let mut r = Reader::new(&[2, 1, 3], 0);
        assert_eq!(select(&mut r, 2, 5).unwrap(), vec![1, 3]);
        // complement of [1, 3] in 5
        let mut r = Reader::new(&[2, 1, 3], 0);
        assert_eq!(select(&mut r, 3, 5).unwrap(), vec![0, 2, 4]);
        // bitmask 0b1001_0001 over 9 objects, then bit 8
        let mut r = Reader::new(&[0x91, 0x01], 0);
        assert_eq!(select(&mut r, 4, 9).unwrap(), vec![0, 4, 7, 8]);
        // start 2, stride 3, count 3
        let mut r = Reader::new(&[2, 3, 3], 0);
        assert_eq!(select(&mut r, 6, 12).unwrap(), vec![2, 5, 8]);
        assert!(select(&mut Reader::new(&[], 0), 7, 3).is_err());
    }

    #[test]
    fn array_and_pointer_records() {
        // mode 0 over 2 objects, member offset 24, (count, offset) pairs.
        let d = [0x00, 49, 3, 0, 5, 16];
        let recs = unpack_arrays(&mut Reader::new(&d, 0), 2, 2).unwrap();
        assert_eq!(recs.iter().map(|r| (r.1, r.2, r.3)).collect::<Vec<_>>(), vec![(0, 3, 0), (1, 5, 16)]);
        // mode 0 over 1 object: shared target list (flag 0x20), no offset (0x40), no element (8): user 0 ->
        // target object 7.
        let d = [0x68, 4, 9, 0, 7];
        let recs = unpack_pointers(&mut Reader::new(&d, 0), 1, 1).unwrap();
        let r = recs[0];
        assert_eq!((r.target_list, r.target_object, r.user), (9, 7, None));
        // user fixup 2 (stored as 3).
        let d = [0x68, 4, 9, 3];
        let r = unpack_pointers(&mut Reader::new(&d, 0), 1, 1).unwrap()[0];
        assert_eq!(r.user, Some(2));
    }

    #[test]
    fn rejects_other_files() {
        assert_eq!(Cluster::parse(vec![0; 100]).unwrap_err(), PhyreError::NotACluster);
        let mut d = vec![0u8; 100];
        d[..4].copy_from_slice(MAGIC);
        d[12..16].copy_from_slice(b"3SP\0");
        assert!(matches!(Cluster::parse(d).unwrap_err(), PhyreError::Platform(_)));
    }

    proptest! {
        #[test]
        fn parse_never_panics(mut bytes in proptest::collection::vec(any::<u8>(), 0..512)) {
            if bytes.len() >= 16 {
                bytes[..4].copy_from_slice(MAGIC);
                bytes[12..16].copy_from_slice(b"11XD");
            }
            let _ = Cluster::parse(bytes);
        }

        #[test]
        fn unpackers_never_panic(bytes in proptest::collection::vec(any::<u8>(), 0..64), n in 0u32..300, k in 0u32..50) {
            let _ = unpack_arrays(&mut Reader::new(&bytes, 0), k, n);
            let _ = unpack_pointers(&mut Reader::new(&bytes, 0), k, n);
        }
    }
}
