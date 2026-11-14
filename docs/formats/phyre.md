# Phyre clusters

The HD remaster stores models (`*.dae.phyre`), textures (`*.dds.phyre`), effects (`*.fx.phyre`) and animations
(`*.ags.phyre`) as PhyreEngine clusters built for D3D11. A cluster is a serialized object graph that describes
its own types, so objects are read by class and member name. Everything is little-endian. This description was
worked out from the files and checked against every cluster of both games (`cargo run -p xtask -- check-phyre`).

## Layout

| Part | Size | Content |
|---|---|---|
| Header | 0x54 | 21 `u32` (below) |
| Namespace | header word 2 | type, class and member descriptions |
| Instance list headers | 36 × lists | one per class used |
| Instance data | sum of list sizes | objects, then that list's arrays |
| User fixup data | header word 13 | string pool |
| User fixups | 12 × count | `u32` type, `u32` size, `u32` offset into the pool |
| Header classes | 4 × count + 16 × children | not needed for reading |
| Pointer-array fixups | header word 9 | packed (below) |
| Pointer fixups | header word 7 | packed |
| Array fixups | header word 5 | packed |
| Index buffer, vertex buffer | header words 18, 19 | mesh data |
| Texture data | rest of file | texture levels |

Header words: magic `RYHP`, header size, namespace size, platform (`11XD`), instance list count, array fixup
size and count, pointer fixup size and count, pointer-array fixup size and count, pointers-in-arrays count, user
fixup count, user fixup data size, total data size, header class instance count, header class child count,
physics engine id, index buffer size, vertex buffer size, largest texture size.

A few folders also contain big-endian PS3 clusters (magic `PHYR`, platform `GCM`); the PC game does not use them.

### Namespace

`u32` 0x01020304, size, type count, class count, member count, string table size, two default-buffer words; then
type names (string offsets), classes (9 `u32`: base class id (1-based, 0 = none), size (low 28 bits), name,
member count, three layout words, flags, default buffer offset), members (6 `u32`: name, type id, offset in the
object, size, flags, fixed array size), and the string table. Members are listed class by class. A type id below
the type count names a primitive type, the next id is unused, and higher ids name classes in order.

### Instance lists

Each header: class id (1-based), object count, total size, objects size, arrays size, pointers-in-arrays count,
array fixup count, pointer fixup count, pointer-array fixup count. A list's data is its objects (all the same
size) followed by the data their arrays and strings point to. Pointer fields are stored as zero.

## Packed fixups

Each fixup section is one byte stream covering all instance lists in order; for each list, records are read
until the list's count for that section is reached. Integers are LEB128.

A group starts with a byte *h*: mode = *h* & 7, flags = (*h* & 0xF9) | 1, plus 2 when the list has a single
object. Then a member value *v*: even = member index (*v* / 2, counted over the whole namespace), odd = byte
offset *v* / 2 inside the object. Modes select objects of the list:

| Mode | Objects |
|---|---|
| 0 | all |
| 1 | groups: repeated sub-mode byte, shared values, then a selector (2, 3, 4 or 6), until every object is covered |
| 2 | count, then that many indices |
| 3 | count, then indices to leave out; all others in order |
| 4 | bitmask, one bit per object |
| 5 | count, then explicit records |
| 6 | start, stride, count |

Indices are one byte when the list has fewer than 256 objects, LEB128 otherwise.

- **Array records**: per selected object, the element count (unless flags & 8) and the offset into the list's
  array data.
- **Pointer records**: when flags & 0x20 a shared target list follows the member value. Per record: unless flags &
  0x10, a user fixup number (*u* − 1; 0 means none); without a user fixup, the target object, target list (unless
  flags & 0x20) and byte offset into the target (unless flags & 0x40). Then, unless flags & 8, an element value:
  the index for arrays of pointers, the element count for pointers to arrays of objects.
- **Pointer-array records** use the array record layout and mark members that are arrays of pointers.

User fixups point at strings such as class names, texture formats and vertex semantics.

## Textures

`PTexture2D` (`m_width`, `m_height`) and `PTextureCubeMap` (`m_size`, six faces) name their format through a
user fixup: `DXT1`, `DXT3`, `DXT5`, `ARGB8` (bytes B, G, R, A) or `L8`. Levels follow each other in the
texture data, largest first, face after face; `m_maxMipLevel` + 1 levels fill the data exactly in every texture
of both games.

## Base-class offsets

A class's third layout word gives where its base class's part starts inside it (4 when a vtable comes first).
Data of inherited members is shifted by the sum of these offsets on the way from the object's class up to the
declaring class; fixups still name members by their declared offsets. Cameras are the main case:
`PCameraProjection` puts `PCamera`'s members 4 bytes later.

## Models and cameras

- Mesh instances take their transform from the node whose `m_worldMatrix` is the instance's
  `m_localToWorldMatrix`, composed from `m_localMatrix` up the `m_parent` chain. The world uses the PS2
  convention: Y points down.
- Maps with pre-rendered backgrounds have a `2d` model: layered quads and a scene camera (`CameraNodeScene*`)
  placed in front of them. The viewer fits the camera's field of view so the farthest layer fills the frame;
  the frame comes out at about 8:7, the PS2's 512 × 448.
